//! Live integration test: spawns covenantd against a tempdir HOME
//! pre-seeded with a non-operator delegate peer, has the delegate
//! authenticate, and verifies that `Request::SubmitIntent` is
//! rejected by the `memory.write` capability gate before any
//! working-memory record is written.
//!
//! The matrix marks `memory.write` as `delegated-denial-only`: a
//! delegated allowance would let an arbitrary peer plant entries in
//! the operator's working-memory tier as a side effect of intent
//! dispatch, and that requires a human release review. The matrix
//! policy authorizes automation to add denial-only coverage here.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_memory_write_delegated_denial -- --ignored live_`.

use covenant_audit::AuditKind;
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
}

async fn wait_for_sock(path: &std::path::Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn read_operator_token(home: &std::path::Path) -> String {
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

async fn authenticated_stream(sock: &std::path::Path, token_b58: &str) -> UnixStream {
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
#[ignore = "live: spawns covenantd + verifies delegated denial for memory.write via SubmitIntent"]
async fn live_covenantd_memory_write_rejects_non_operator_intent_dispatch() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([131u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [132u8; 32];
    let delegate_display = "delegate-intent-submitter@local";
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

    // ── Phase 1: a non-operator delegate submitting an intent
    //     without any `memory.write` grant must be rejected at the
    //     capability gate that runs inside `dispatch_intent` before
    //     working memory is written. The intent text is chosen so
    //     it does not match a default `.covenantignore` rule —
    //     ignored intents return `IntentResult { status: "ignored",
    //     ... }` rather than `Response::Error`, which would mask
    //     the gate.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::SubmitIntent {
                text: "delegated memory.write denial probe".into(),
            },
        )
        .await
        {
            Response::Error { message } => assert!(
                message.contains("memory.write"),
                "missing-grant memory.write must reject by capability gate, got {message:?}"
            ),
            other => panic!(
                "non-operator delegate must not dispatch intents without memory.write, got {other:?}"
            ),
        }
    }

    // ── Phase 2: the rejection must surface in the delegate's
    //     audit feed as a `CapabilityCheck` row with `passed=false`
    //     and `missing_actions` containing `memory.write`. The
    //     audit row is the durable evidence that the gate fired at
    //     the dispatch layer (rather than the request being
    //     silently dropped). The delegate's `recent_audit` is
    //     filtered to issuer=peer, so the row is visible without an
    //     extra grant.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        let events = match req(&mut stream, Request::RecentAudit { limit: 50 }).await {
            Response::AuditEvents { events } => events,
            other => panic!("delegate recent_audit failed: {other:?}"),
        };
        let row = events
            .iter()
            .find(|e| {
                matches!(
                    &e.kind,
                    AuditKind::CapabilityCheck {
                        passed: false,
                        missing_actions,
                        ..
                    } if missing_actions.iter().any(|a| a == "memory.write")
                )
            })
            .expect(
                "expected a CapabilityCheck audit row for memory.write with passed=false in the delegate's feed",
            );
        match &row.kind {
            AuditKind::CapabilityCheck { agent_id, .. } => {
                assert_eq!(
                    agent_id, "memory:write",
                    "audit row must name the memory-write scope id"
                );
            }
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    // ── Phase 3: the delegate session is still usable. A bare
    //     `Request::Ping` round-trips, so the rejection is scoped
    //     to the privileged action, not the entire session.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, Request::Ping).await {
            Response::Pong => {}
            other => panic!("delegate must remain live after denial, got {other:?}"),
        }
    }

    let _ = child.kill().await;
}
