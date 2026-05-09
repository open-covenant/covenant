//! Live integration test: spawns covenantd against a tempdir HOME, exercises
//! the A2A duplex (send task / try_recv task / post result / try_recv result)
//! end-to-end, and verifies the capability gates from both sides — that
//! ungranted requests are rejected, that grants permit the request, and that
//! rejected requests do not enqueue.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_a2a -- --ignored live_`.

use covenant_a2a::{A2ATask, A2ATaskResult};
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_mcp::Content;
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

async fn authenticate(stream: &mut UnixStream, home: &std::path::Path) {
    let token_b58 = read_operator_token(home).await;
    match req(stream, Request::Authenticate { token_b58 }).await {
        Response::Authenticated { .. } => {}
        other => panic!("authenticate failed: {other:?}"),
    }
}

/// Reads the daemon's on-disk ed25519 public key bytes — same value the
/// operator peer was registered with. Required because the spoof check
/// compares the full `AgentId` (display + pubkey) on every A2A send.
fn read_peer_pubkey(home: &std::path::Path) -> [u8; 32] {
    let path = home.join("identity").join("local.key");
    let id = covenant_identity::LocalIdentity::load_or_create(&path, "user@local")
        .expect("load identity");
    id.pubkey_bytes()
}

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

#[tokio::test]
#[ignore = "live: spawns covenantd + drives the A2A duplex through real IPC"]
async fn live_covenantd_a2a_duplex_with_capability_gating() {
    let home = tempfile::tempdir().expect("tempdir");
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

    let mut stream = UnixStream::connect(&sock).await.expect("connect");
    authenticate(&mut stream, home.path()).await;

    // `task.sender` must match the authenticated peer; the operator
    // authenticates as `user@local` so that's the sender we use.
    // `try_recv` filters by `recipient`, so v0 round-trips have to
    // address the task to the same peer that drains it.
    let pubkey = read_peer_pubkey(home.path());
    let peer = AgentId::new("user@local", pubkey);
    let task = A2ATask {
        id: Uuid::new_v4(),
        sender: peer.clone(),
        recipient: peer.clone(),
        intent_text: "find recent papers".into(),
        parent: None,
        deadline_ms: None,
        idempotency_key: None,
        duplicate_risk: None,
    };

    // 1. Send before grant — rejected.
    match req(&mut stream, Request::SendA2ATask { task: task.clone() }).await {
        Response::Error { message } => {
            assert!(
                message.contains(&format!("a2a.send.{}", task.recipient.display)),
                "rejection should name the missing cap: {message}"
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }

    // 1a. The mailbox stays empty after the rejection.
    match req(&mut stream, Request::TryRecvA2ATask).await {
        Response::A2ATaskOpt { task: None } => {}
        other => panic!("rejected task must not enqueue: {other:?}"),
    }

    // 2. Grant a2a.send.<recipient>, send, recv.
    match req(
        &mut stream,
        Request::GrantCapability {
            action: format!("a2a.send.{}", task.recipient.display),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { .. } => {}
        other => panic!("grant failed: {other:?}"),
    }

    match req(&mut stream, Request::SendA2ATask { task: task.clone() }).await {
        Response::A2ATaskQueued { task_id } => assert_eq!(task_id, task.id),
        other => panic!("expected A2ATaskQueued, got {other:?}"),
    }

    match req(&mut stream, Request::TryRecvA2ATask).await {
        Response::A2ATaskOpt { task: Some(t) } => {
            assert_eq!(t.id, task.id);
            assert_eq!(t.recipient, peer);
        }
        other => panic!("expected queued task, got {other:?}"),
    }

    // 2a. Empty after drain.
    match req(&mut stream, Request::TryRecvA2ATask).await {
        Response::A2ATaskOpt { task: None } => {}
        other => panic!("expected empty queue, got {other:?}"),
    }

    // 3. Post a result before granting a2a.respond — rejected.
    let result = A2ATaskResult::ok(task.id, vec![Content::text("done")]);
    match req(
        &mut stream,
        Request::PostA2AResult {
            result: result.clone(),
        },
    )
    .await
    {
        Response::Error { message } => {
            assert!(
                message.contains(&format!("a2a.respond.{}", task.sender.display)),
                "rejection should name the sender-scoped cap: {message}"
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }

    match req(&mut stream, Request::TryRecvA2AResult).await {
        Response::A2AResultOpt { result: None } => {}
        other => panic!("rejected result must not enqueue: {other:?}"),
    }

    // 3a. A result for an unknown task_id is rejected even though
    //     a2a.respond.<some-sender> may eventually be granted: the
    //     mailbox has no record of the task ever having been sent.
    let stray = A2ATaskResult::ok(Uuid::new_v4(), vec![Content::text("stray")]);
    match req(&mut stream, Request::PostA2AResult { result: stray }).await {
        Response::Error { message } => {
            assert!(
                message.contains("never dispatched"),
                "rejection should call out the unknown task_id: {message}"
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }

    // 4. Grant a2a.respond.<sender>, post, recv.
    match req(
        &mut stream,
        Request::GrantCapability {
            action: format!("a2a.respond.{}", task.sender.display),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { .. } => {}
        other => panic!("grant failed: {other:?}"),
    }

    match req(
        &mut stream,
        Request::PostA2AResult {
            result: result.clone(),
        },
    )
    .await
    {
        Response::A2AResultPosted { task_id } => assert_eq!(task_id, task.id),
        other => panic!("expected A2AResultPosted, got {other:?}"),
    }

    match req(&mut stream, Request::TryRecvA2AResult).await {
        Response::A2AResultOpt {
            result: Some(got), ..
        } => {
            assert_eq!(got.task_id, task.id);
            assert_eq!(got.status, covenant_a2a::A2ATaskStatus::Ok);
        }
        other => panic!("expected queued result, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + drives the recipient admission gate through real IPC"]
async fn live_covenantd_a2a_recipient_admission_gate_rejects_unallowed() {
    let home = tempfile::tempdir().expect("tempdir");
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

    let mut stream = UnixStream::connect(&sock).await.expect("connect");
    authenticate(&mut stream, home.path()).await;

    // Recv gate: foreign recipient pubkey ≠ peer pubkey, so the gate
    // fires. v0 has no IPC verb to grant a cap on a foreign subject,
    // so this test verifies only the rejection path; the pass path is
    // mock-tested with direct store injection.
    let pubkey = read_peer_pubkey(home.path());
    let peer = AgentId::new("user@local", pubkey);
    let foreign_recipient = AgentId::new("victim@local", [7u8; 32]);
    let task = A2ATask {
        id: Uuid::new_v4(),
        sender: peer.clone(),
        recipient: foreign_recipient.clone(),
        intent_text: "spam".into(),
        parent: None,
        deadline_ms: None,
        idempotency_key: None,
        duplicate_risk: None,
    };

    // Grant the sender-side cap so the recv gate is the *only* thing
    // standing in the way. This makes the test discriminate the recv
    // gate from the send-cap gate.
    match req(
        &mut stream,
        Request::GrantCapability {
            action: format!("a2a.send.{}", foreign_recipient.display),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { .. } => {}
        other => panic!("grant failed: {other:?}"),
    }

    match req(&mut stream, Request::SendA2ATask { task: task.clone() }).await {
        Response::Error { message } => {
            assert!(
                message.contains("recipient has not granted"),
                "rejection should name the recv gate: {message}"
            );
            assert!(
                message.contains(&format!("a2a.recv.{}", peer.display)),
                "rejection should name the missing cap: {message}"
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }

    // Mailbox stays empty after the rejection — the gate fires before
    // `mailbox.send_task`.
    match req(&mut stream, Request::TryRecvA2ATask).await {
        Response::A2ATaskOpt { task: None } => {}
        other => panic!("rejected task must not enqueue: {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
}
