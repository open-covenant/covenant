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

    let task = A2ATask {
        id: Uuid::new_v4(),
        sender: AgentId::new("orch@local", [0u8; 32]),
        recipient: AgentId::new("research@local", [0u8; 32]),
        intent_text: "find recent papers".into(),
        parent: None,
        deadline_ms: None,
    };

    // 1. Send before grant — rejected.
    match req(&mut stream, Request::SendA2ATask { task: task.clone() }).await {
        Response::Error { message } => {
            assert!(
                message.contains("a2a.send.research@local"),
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
            assert_eq!(t.recipient.display, "research@local");
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
                message.contains("a2a.respond"),
                "rejection should name the missing cap: {message}"
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }

    match req(&mut stream, Request::TryRecvA2AResult).await {
        Response::A2AResultOpt { result: None } => {}
        other => panic!("rejected result must not enqueue: {other:?}"),
    }

    // 4. Grant a2a.respond, post, recv.
    match req(
        &mut stream,
        Request::GrantCapability {
            action: "a2a.respond".into(),
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
