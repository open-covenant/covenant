//! Live integration test: the operator-only provenance query surfaces a real
//! privileged action with the audit chain verified, and refuses a non-operator
//! delegate at the live socket.
//!
//! `Request::QueryProvenance` is the surface that answers "who did what
//! privileged action, under which authorizing rule, with what outcome" from a
//! tamper-evident record: the daemon projects the capability family of audit
//! kinds into privileged-action rows, verifies the audit hash-chain in the same
//! call so the answer is never read from tampered rows, and gates the
//! cross-actor view on the operator identity. The in-process unit test
//! `query_provenance_projects_filters_and_gates_on_operator` pins the
//! projection, filters, limit, and gate against synthetic `InMemoryAuditLog`
//! rows and a synthetic non-operator `AgentId`; this drives the surface through
//! the real daemon, where the rows come from a real grant, the chain is the real
//! on-disk store, and the gate keys on a real authenticated connection.
//!
//! The operator makes a real `tool.call.echo` grant — `grant_capability` signs
//! the capability, records it, and emits the `CapabilityGranted` audit row whose
//! `signature_b58` is the same value it returns to the caller — then queries
//! provenance and finds that grant surfaced as a `granted` action whose
//! authorizing rule equals that exact signature, alongside the verified verdict
//! of the real audit hash-chain. So a regression where a real grant stops
//! emitting the row the projection reads, or the query stops verifying the real
//! chain, is caught — neither is reachable by the unit test, which hand-feeds the
//! rows into an `InMemoryAuditLog`. A pre-seeded non-operator delegate
//! authenticating over the same daemon is then refused outright —
//! `Response::Error` naming the operator requirement, never a
//! `ProvenanceActions` snapshot — so the cross-actor view cannot leak to a
//! non-operator and an empty-but-allowed reply is not mistaken for a pass. A
//! bare `Ping` still round-trips on the delegate's connection, so the refusal is
//! scoped to the privileged query, not the session.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_provenance_query -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
}

async fn wait_for_sock(path: &Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn read_operator_token(home: &Path) -> String {
    let path = home.join("peers").join("operator.token");
    for _ in 0..50 {
        if let Ok(s) = std::fs::read_to_string(&path) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("operator token never appeared at {}", path.display());
}

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

async fn authenticated_stream(sock: &Path, token_b58: &str) -> UnixStream {
    let mut stream = UnixStream::connect(sock).await.expect("connect");
    let request = Request::Authenticate {
        token_b58: token_b58.to_string(),
    };
    match req(&mut stream, request).await {
        Response::Authenticated { .. } => stream,
        other => panic!("authentication failed: {other:?}"),
    }
}

fn provenance_query() -> Request {
    Request::QueryProvenance {
        since_ms: None,
        until_ms: None,
        actor: None,
        approver: None,
        rule: None,
        outcome: None,
        limit: 10,
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies QueryProvenance surfaces a real grant with the audit chain verified and refuses a non-operator delegate"]
async fn live_covenantd_query_provenance_surfaces_real_grant_and_gates_on_operator() {
    let home = tempfile::tempdir().expect("tempdir");

    // Pre-seed a non-operator delegate peer into the registry before the daemon
    // starts, so it can authenticate but holds no operator authority.
    let delegate_token = PeerToken::from_bytes([171u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [172u8; 32];
    let delegate_display = "delegate-provenance-reader@local";
    let registry_path = home.path().join("peers").join("registry.jsonl");
    {
        let registry = JsonlPeerRegistry::open(registry_path)
            .await
            .expect("open seed registry");
        registry
            .register(PeerEntry {
                token: delegate_token,
                agent_id: AgentId::new(delegate_display, delegate_pubkey),
                registered_at: 1_700_000_000_000,
            })
            .await
            .expect("seed delegate");
    }

    let port = pick_free_port();
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(exe)
        .env("COVENANT_HOME", home.path())
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");

    let sock = home.path().join("sock");
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon never created its socket at {}", sock.display());
    }

    let operator_token = read_operator_token(home.path()).await;
    assert_ne!(
        operator_token, delegate_token_b58,
        "operator and delegate tokens must differ"
    );

    // ── Phase 1: the operator makes a real capability grant — `grant_capability`
    //     signs it, records it, and emits the `CapabilityGranted` audit row whose
    //     `signature_b58` is the same value returned here — then the provenance
    //     query surfaces that grant as a `granted` action bound to the exact
    //     signature, carrying the verified verdict of the real on-disk audit
    //     chain. The unit test hand-feeds rows into an in-memory log; this proves
    //     the real grant→audit→projection wiring and a real chain verification.
    {
        let mut stream = authenticated_stream(&sock, &operator_token).await;
        let signature_b58 = match req(
            &mut stream,
            Request::GrantCapability {
                action: "tool.call.echo".into(),
                scope: Some(serde_json::json!({ "version": 1, "tool": "echo" })),
                expires_at: None,
            },
        )
        .await
        {
            Response::CapabilityGranted { signature_b58, .. } => signature_b58,
            other => panic!("operator grant failed: {other:?}"),
        };

        match req(&mut stream, provenance_query()).await {
            Response::ProvenanceActions {
                integrity,
                actions,
                scanned,
            } => {
                assert!(
                    integrity.valid,
                    "a provenance answer must carry the verified verdict of the real \
                     on-disk audit hash-chain, not raw rows",
                );
                assert!(
                    scanned >= 1,
                    "the just-recorded grant event is in window and must be scanned, \
                     got scanned={scanned}",
                );
                assert!(
                    actions.iter().any(|a| a.outcome == "granted"
                        && a.action == "tool.call.echo"
                        && a.rule.as_deref() == Some(signature_b58.as_str())
                        && a.approver.is_some()),
                    "the real grant must surface as a `granted` privileged action whose \
                     authorizing rule is the daemon-returned signature {signature_b58}; \
                     got {actions:?}",
                );
            }
            other => panic!("the operator must be allowed the provenance query, got {other:?}"),
        }
    }

    // ── Phase 2: a non-operator delegate authenticating over the same daemon is
    //     refused outright — Response::Error naming the operator requirement,
    //     carrying no actions — so the cross-actor provenance view never leaks to
    //     a peer that is not the operator, and the refusal is not an
    //     empty-but-allowed snapshot.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, provenance_query()).await {
            Response::Error { message } => assert!(
                message.contains("operator identity"),
                "the refusal must name the operator requirement, got {message:?}",
            ),
            Response::ProvenanceActions { actions, .. } => panic!(
                "a non-operator delegate must be refused, not handed {} provenance rows",
                actions.len(),
            ),
            other => panic!("a non-operator delegate must be refused, got {other:?}"),
        }
    }

    // ── Phase 3: the denial is scoped to the privileged query, not the session.
    //     The delegate's connection still round-trips a bare Ping.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, Request::Ping).await {
            Response::Pong => {}
            other => panic!("the delegate must remain live after denial, got {other:?}"),
        }
    }

    let _ = child.kill().await;
}
