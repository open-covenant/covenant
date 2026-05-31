//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::CompactA2A` over the raw IPC socket, asserting the daemon gates the
//! compaction on `a2a.compact` and reaps a fully-drained task's events.
//!
//! The verb is covered today over HTTP (`live_http_a2a_compact.rs`) but never
//! over the raw Unix socket both that route and the CLI are built on. This pins
//! the `Response::A2ACompacted { dropped }` envelope (covenant-ipc/src/lib.rs:885)
//! reached through the `a2a.compact` gate (covenantd/src/lib.rs:2651).
//!
//! One self-addressed task is driven through its whole lifecycle over the same
//! socket — send, lease, respond, post, receive — so `posted == drained == 1`
//! crosses it into droppable, then the granted compact drops it. Both the
//! `a2a.send` and `a2a.respond` grants are self-scoped to the operator's own
//! identity. Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_compact_a2a -- --ignored live_`.

use covenant_a2a::{A2ATask, A2ATaskResult};
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_mcp::Content;
use covenant_types::AgentId;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;
use uuid::Uuid;

fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().unwrap().port()
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

/// The operator identity the daemon authenticates the connection as.
/// `load_or_create` loads the daemon-created key, so it matches the peer the
/// self-scoped `a2a.send`/`a2a.respond` grants resolve to.
fn operator_agent_id(home: &Path) -> AgentId {
    let id = covenant_identity::LocalIdentity::load_or_create(
        &home.join("identity").join("local.key"),
        "user@local",
    )
    .expect("load identity");
    AgentId::new("user@local", id.pubkey_bytes())
}

async fn spawn_daemon(home: &Path) -> Child {
    let port = pick_free_port();
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let child = Command::new(exe)
        .env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");
    if !wait_for_sock(&home.join("sock")).await {
        panic!("daemon never created its socket");
    }
    child
}

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

async fn authenticated_stream(home: &Path) -> UnixStream {
    let mut stream = UnixStream::connect(home.join("sock"))
        .await
        .expect("connect socket");
    let token = read_operator_token(home).await;
    match req(&mut stream, Request::Authenticate { token_b58: token }).await {
        Response::Authenticated { .. } => {}
        other => panic!("authenticate failed: {other:?}"),
    }
    stream
}

async fn grant(stream: &mut UnixStream, action: String) {
    match req(
        stream,
        Request::GrantCapability {
            action,
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { .. } => {}
        other => panic!("expected Response::CapabilityGranted, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + drives Request::CompactA2A over the socket"]
async fn live_ipc_compact_a2a_drops_drained_task() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;
    let peer = operator_agent_id(home.path());

    // Without the grant the compact is rejected by name before any drop.
    match req(&mut stream, Request::CompactA2A).await {
        Response::Error { message } => {
            assert!(
                message.contains("a2a.compact"),
                "ungranted compact must name the missing capability: {message}"
            );
            assert!(
                message.contains("requires capability"),
                "rejection must surface the requires-capability prefix: {message}"
            );
        }
        other => panic!("expected Response::Error before grant, got {other:?}"),
    }

    // Drive one self-addressed task through its whole lifecycle so it becomes
    // droppable: send, lease, respond, post, receive.
    grant(&mut stream, format!("a2a.send.{}", peer.display)).await;
    let task = A2ATask {
        id: Uuid::new_v4(),
        sender: peer.clone(),
        recipient: peer.clone(),
        intent_text: "socket compact probe".into(),
        task_kind: None,
        parent: None,
        deadline_ms: None,
        idempotency: None,
    };
    match req(&mut stream, Request::SendA2ATask { task: task.clone() }).await {
        Response::A2ATaskQueued { task_id } => assert_eq!(task_id, task.id),
        other => panic!("expected Response::A2ATaskQueued, got {other:?}"),
    }
    match req(&mut stream, Request::TryRecvA2ATask).await {
        Response::A2ATaskOpt { task: Some(t) } => assert_eq!(t.id, task.id),
        other => panic!("expected Response::A2ATaskOpt Some, got {other:?}"),
    }

    grant(&mut stream, format!("a2a.respond.{}", peer.display)).await;
    let result = A2ATaskResult::ok(task.id, vec![Content::text("socket compact result probe")]);
    match req(&mut stream, Request::PostA2AResult { result }).await {
        Response::A2AResultPosted { task_id } => assert_eq!(task_id, task.id),
        other => panic!("expected Response::A2AResultPosted, got {other:?}"),
    }
    // Receiving the result emits ResultRecv, making posted == drained == 1 so
    // the task crosses into droppable.
    match req(&mut stream, Request::TryRecvA2AResult).await {
        Response::A2AResultOpt { result: Some(got) } => {
            assert_eq!(got.task_id, task.id);
            assert_eq!(got.status, covenant_a2a::A2ATaskStatus::Ok);
        }
        other => panic!("expected Response::A2AResultOpt Some, got {other:?}"),
    }

    // Granted compact reaps the fully-drained task's events.
    grant(&mut stream, "a2a.compact".into()).await;
    match req(&mut stream, Request::CompactA2A).await {
        Response::A2ACompacted { dropped } => assert!(
            dropped >= 1,
            "a fully-drained task's events must be dropped: dropped={dropped}"
        ),
        other => panic!("expected Response::A2ACompacted, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
