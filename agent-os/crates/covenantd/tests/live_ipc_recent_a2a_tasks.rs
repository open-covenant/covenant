//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::RecentA2ATasks` over the raw IPC socket — empty mailbox, one
//! self-addressed task, then a non-draining re-read.
//!
//! The verb is covered today over HTTP (`live_http_a2a_tasks_recent.rs`, GET
//! `/a2a/tasks/recent`) but never over the raw Unix socket both are built on.
//! This pins that wire contract: the `Response::A2ATasks { tasks }` variant
//! (covenant-ipc/src/lib.rs:875) — a tasks-only envelope, distinct from the
//! `A2AQueue` shape — and the both-sides `sender == peer || recipient == peer`
//! row scoping in `recent_a2a_tasks`.
//!
//! The task is seeded through the public API: the operator grants itself
//! `a2a.send.<display>` and sends one self-addressed `A2ATask` (operator is both
//! sender and recipient, so the scoping filter keeps it). The empty baseline
//! stops a regression that always returns `[]`; the non-draining re-read stops
//! one that consumes the row on read. Hermetic — no external services.
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_recent_a2a_tasks -- --ignored live_`.

use covenant_a2a::A2ATask;
use covenant_ipc::{read_frame, write_frame, Request, Response};
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
/// `load_or_create` loads the daemon-created key, so it matches the peer used
/// for the both-sides row-scoping filter.
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

#[tokio::test]
#[ignore = "live: spawns covenantd + drives Request::RecentA2ATasks over the socket"]
async fn live_ipc_recent_a2a_tasks_lists_without_draining() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    // Empty baseline: a fresh mailbox lists no tasks. This stops a regression
    // that always returns [] from masquerading as the populated path.
    match req(&mut stream, Request::RecentA2ATasks { limit: 10 }).await {
        Response::A2ATasks { tasks } => assert!(
            tasks.is_empty(),
            "a fresh mailbox has no A2A tasks: {tasks:?}"
        ),
        other => panic!("expected Response::A2ATasks, got {other:?}"),
    }

    // Seed one self-addressed task through the public API. The operator is both
    // sender and recipient, so the both-sides scoping filter keeps the row.
    let peer = operator_agent_id(home.path());
    match req(
        &mut stream,
        Request::GrantCapability {
            action: format!("a2a.send.{}", peer.display),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { .. } => {}
        other => panic!("expected Response::CapabilityGranted, got {other:?}"),
    }
    let task = A2ATask {
        id: Uuid::new_v4(),
        sender: peer.clone(),
        recipient: peer.clone(),
        intent_text: "socket recent-tasks probe".into(),
        task_kind: None,
        parent: None,
        deadline_ms: None,
        idempotency: None,
    };
    match req(&mut stream, Request::SendA2ATask { task: task.clone() }).await {
        Response::Error { message } => {
            panic!("SendA2ATask must accept a self-addressed task: {message}")
        }
        _ => {}
    }

    // Populated read: the sent task surfaces exactly once, keyed by its id.
    match req(&mut stream, Request::RecentA2ATasks { limit: 10 }).await {
        Response::A2ATasks { tasks } => {
            assert_eq!(
                tasks.len(),
                1,
                "the sent task must surface exactly once: {tasks:?}"
            );
            assert_eq!(
                tasks[0].id, task.id,
                "the listed task must be the one sent: {:?}",
                tasks[0]
            );
        }
        other => panic!("expected Response::A2ATasks, got {other:?}"),
    }

    // Non-draining: reading the mailbox does not consume the row.
    match req(&mut stream, Request::RecentA2ATasks { limit: 10 }).await {
        Response::A2ATasks { tasks } => assert_eq!(
            tasks.len(),
            1,
            "RecentA2ATasks is a non-draining read; the row must persist: {tasks:?}"
        ),
        other => panic!("expected Response::A2ATasks, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
