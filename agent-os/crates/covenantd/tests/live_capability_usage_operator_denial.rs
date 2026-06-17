//! Live integration test: the operator-only capability-usage query refuses a
//! non-operator delegate at the live socket.
//!
//! `Request::CapabilityUsage` reports delegated-authority state — which
//! capabilities exist, who holds each, the scope that bounds each, and how much
//! budget remains — so it must be refused to any peer that is not the operator,
//! or that state leaks to a holder that should only be able to use its own
//! grant. The in-process unit test `capability_usage_requires_operator_identity`
//! pins this against constructed state; this drives it through the real daemon.
//!
//! The operator grants a capability (so there is real state to protect) and can
//! read the usage snapshot, while a pre-seeded non-operator delegate
//! authenticating over the same daemon is refused outright — `Response::Error`
//! naming the operator requirement, never a grants snapshot — so the denial
//! cannot be mistaken for an empty-but-allowed result and the gate is shown to
//! key on identity rather than failing the query for everyone. A bare `Ping`
//! still round-trips on the delegate's connection, so the refusal is scoped to
//! the privileged query, not the session.
//!
//! Per the live matrix policy, automation may add denial-only coverage for an
//! operator-gated query. Hermetic — no external services. `#[ignore]`'d. Run
//! with
//! `cargo test -p covenantd --test live_capability_usage_operator_denial -- --ignored live_`.

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

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies the operator-gated capability-usage query refuses a non-operator delegate"]
async fn live_covenantd_capability_usage_refuses_non_operator() {
    let home = tempfile::tempdir().expect("tempdir");

    // Pre-seed a non-operator delegate peer into the registry before the daemon
    // starts, so it can authenticate but holds no operator authority.
    let delegate_token = PeerToken::from_bytes([151u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [152u8; 32];
    let delegate_display = "delegate-usage-reader@local";
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

    // ── Phase 1: the operator grants a capability — real delegated-authority
    //     state the query must protect — and confirms the operator itself can
    //     read the usage snapshot. The gate keys on identity, not a query that
    //     simply fails for everyone.
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
        match req(&mut stream, Request::CapabilityUsage).await {
            Response::CapabilityUsage { grants } => assert!(
                grants.iter().any(|g| g.signature_b58 == signature_b58),
                "the operator can read the grant it just made",
            ),
            other => panic!("the operator must be allowed the usage query, got {other:?}"),
        }
    }

    // ── Phase 2: a non-operator delegate authenticating over the same daemon is
    //     refused outright — Response::Error naming the operator requirement,
    //     carrying no grants — so delegated-authority state never leaks to a peer
    //     that merely holds a grant, and the refusal is not an empty-but-allowed
    //     snapshot.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, Request::CapabilityUsage).await {
            Response::Error { message } => assert!(
                message.contains("operator identity"),
                "the refusal must name the operator requirement, got {message:?}",
            ),
            Response::CapabilityUsage { grants } => panic!(
                "a non-operator delegate must be refused, not handed {} grant rows",
                grants.len(),
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
