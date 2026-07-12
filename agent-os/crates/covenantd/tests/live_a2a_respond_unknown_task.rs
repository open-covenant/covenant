//! Live integration test: spawns covenantd against a tempdir HOME
//! pre-seeded with a non-operator delegate peer, and verifies that
//! `Request::PostA2AResult` for a task id that was never dispatched
//! through this daemon is refused by the unknown-task lookup gate
//! BEFORE the respond capability gate.
//!
//! `post_a2a_result` (lib.rs:2683-2702) looks up the task's sender
//! first, because the capability it must require is derived from the
//! sender (`a2a.respond.<sender_display>`) and an unknown task has no
//! sender. When `lookup_task_sender` returns `Ok(None)` it records an
//! `AuditKind::A2AResultRejected { reason: "unknown_task" }` row on the
//! posting peer's feed and returns `Response::Error` naming
//! "was never dispatched". The sibling delegated-denial test
//! (`live_a2a_respond_delegated_denial.rs`) deliberately sends a real
//! task first, so it exercises the capability gate and never this
//! lookup gate.
//!
//! The delegate holds no `a2a.respond` capability: if the capability
//! gate ran first, the rejection would name that capability. Asserting
//! the message instead names the unknown-task condition pins the
//! ordering — the lookup precedes the capability check.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_a2a_respond_unknown_task -- --ignored live_`.

use covenant_a2a::A2ATaskResult;
use covenant_audit::AuditKind;
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::time::sleep;
use uuid::Uuid;

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
#[ignore = "live: spawns covenantd + verifies a2a respond unknown-task rejection precedes the capability gate"]
async fn live_covenantd_a2a_respond_rejects_unknown_task_before_capability() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([171u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [172u8; 32];
    let delegate_display = "delegate-a2a-unknown-task@local";
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

    // A fresh task id that was never dispatched through this daemon.
    let unknown_task_id = Uuid::new_v4();

    // ── Phase 1: the delegate posts a result for the unknown task
    //     WITHOUT any a2a.respond capability. The lookup gate fires
    //     before the capability gate: the message names the
    //     never-dispatched condition, not the a2a.respond capability.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::PostA2AResult {
                result: A2ATaskResult::ok(unknown_task_id, vec![]),
            },
        )
        .await
        {
            Response::Error { message } => {
                assert!(
                    message.contains("was never dispatched"),
                    "unknown task_id must be rejected by the lookup gate, got {message:?}"
                );
                assert!(
                    !message.contains("a2a.respond"),
                    "the lookup gate must fire before the capability gate, so the \
                     rejection must NOT name the a2a.respond capability, got {message:?}"
                );
            }
            other => panic!(
                "posting a result for a never-dispatched task must be rejected, got {other:?}"
            ),
        }
    }

    // ── Phase 2: the rejection is recorded. The unknown-task row is
    //     issued under the posting peer, so it surfaces on the
    //     delegate's own peer-scoped audit feed.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::RecentAudit {
                limit: 50,
                since_ms: None,
                prefer_stream: None,
            },
        )
        .await
        {
            Response::AuditEvents { events } => assert!(
                events.iter().any(|event| matches!(
                    &event.kind,
                    AuditKind::A2AResultRejected { task_id, reason }
                        if *task_id == unknown_task_id && reason == "unknown_task"
                )),
                "the unknown-task rejection must record an \
                 A2AResultRejected{{unknown_task}} audit row: {events:?}"
            ),
            other => panic!("expected Response::AuditEvents, got {other:?}"),
        }
    }

    // ── Phase 3: the delegate session is still usable. A bare
    //     `Request::Ping` round-trips, so the rejection is scoped to
    //     the bad post, not the entire session.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, Request::Ping).await {
            Response::Pong => {}
            other => panic!("delegate must remain live after the rejection, got {other:?}"),
        }
    }

    let _ = child.kill().await;
}
