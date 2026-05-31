//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::RepairA2ATask` over the raw IPC socket — requeue one in-flight
//! lease back to the queue and confirm it is re-leasable.
//!
//! The verb is covered today over the CLI (`live_cli_a2a_repair.rs`) and HTTP
//! (`live_http_a2a_repair.rs`) but never over the raw Unix socket both are built
//! on. This pins the `Response::A2ARepaired { outcome }` wire shape
//! (covenant-ipc/src/lib.rs:888) reached through the `a2a.repair.requeue` gate:
//! a leased task is read back via `A2AQueue` for its real lease id, requeued,
//! and the queue then reports it `Queued` at attempt 1 and hands it out again.
//!
//! Hermetic — one self-addressed loopback task, no network, Solana, or model.
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_repair_a2a_task -- --ignored live_`.

use covenant_a2a::{
    A2ADuplicateRisk, A2ARepairAction, A2ARepairCommand, A2ARepairRequest, A2ARepairState, A2ATask,
    A2ATaskQueueState,
};
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

/// The 32-byte ed25519 key of the daemon's operator. `load_or_create` loads the
/// key the daemon already wrote, so the self-scoped `a2a.send` grant resolves to
/// the same identity the connection is authenticated as.
fn read_peer_pubkey(home: &Path) -> [u8; 32] {
    covenant_identity::LocalIdentity::load_or_create(
        &home.join("identity").join("local.key"),
        "user@local",
    )
    .expect("load identity")
    .pubkey_bytes()
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
#[ignore = "live: spawns covenantd + drives Request::RepairA2ATask over the socket"]
async fn live_ipc_repair_a2a_task_requeues_in_flight() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;
    let peer = AgentId::new("user@local", read_peer_pubkey(home.path()));

    grant(&mut stream, format!("a2a.send.{}", peer.display)).await;
    grant(&mut stream, "a2a.repair.requeue".into()).await;

    let task = A2ATask {
        id: Uuid::new_v4(),
        sender: peer.clone(),
        recipient: peer.clone(),
        intent_text: "live ipc repair requeue".into(),
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
        Response::A2ATaskOpt { task: Some(got) } => assert_eq!(got.id, task.id),
        other => panic!("expected Response::A2ATaskOpt Some, got {other:?}"),
    }

    // Read the leased entry's real lease id over the socket.
    let lease_id = match req(
        &mut stream,
        Request::A2AQueue {
            limit: 20,
            min_lease_age_ms: Some(0),
            deadline_within_ms: None,
            state_filter: None,
        },
    )
    .await
    {
        Response::A2AQueue { tasks, .. } => {
            let entry = tasks
                .iter()
                .find(|e| e.task.id == task.id)
                .expect("the leased task must appear in the queue");
            assert_eq!(
                entry.state,
                A2ATaskQueueState::InFlight,
                "a leased task must be in-flight: {entry:?}"
            );
            entry.lease_id.expect("a leased entry carries a lease id")
        }
        other => panic!("expected Response::A2AQueue, got {other:?}"),
    };

    // The pin: requeue the in-flight lease.
    match req(
        &mut stream,
        Request::RepairA2ATask {
            request: A2ARepairRequest {
                task_id: task.id,
                command: A2ARepairCommand::Requeue {
                    lease_id: Some(lease_id),
                    duplicate_risk: A2ADuplicateRisk::Idempotent,
                },
                reason: "live ipc repair test".into(),
            },
        },
    )
    .await
    {
        Response::A2ARepaired { outcome } => {
            assert_eq!(
                outcome.task_id, task.id,
                "outcome must target the requeued task"
            );
            assert_eq!(
                outcome.action,
                A2ARepairAction::Requeued,
                "action must be requeued"
            );
            assert_eq!(
                outcome.state,
                A2ARepairState::Queued,
                "requeue lands the task as queued"
            );
            assert_eq!(outcome.attempt, 1, "requeue increments attempt to 1");
            assert!(
                outcome.result.is_none(),
                "requeue carries no result: {:?}",
                outcome.result
            );
        }
        other => panic!("expected Response::A2ARepaired, got {other:?}"),
    }

    // The requeue landed: the task is back to queued at attempt 1.
    match req(
        &mut stream,
        Request::A2AQueue {
            limit: 20,
            min_lease_age_ms: None,
            deadline_within_ms: None,
            state_filter: None,
        },
    )
    .await
    {
        Response::A2AQueue { tasks, .. } => {
            let entry = tasks
                .iter()
                .find(|e| e.task.id == task.id)
                .expect("the requeued task must still be in the queue");
            assert_eq!(
                entry.state,
                A2ATaskQueueState::Queued,
                "the requeued task must be back to queued: {entry:?}"
            );
            assert_eq!(
                entry.attempt, 1,
                "the requeued task must be at attempt 1: {entry:?}"
            );
        }
        other => panic!("expected Response::A2AQueue, got {other:?}"),
    }

    // And re-leasable: a fresh lease hands the same task out again.
    match req(&mut stream, Request::TryRecvA2ATask).await {
        Response::A2ATaskOpt { task: Some(got) } => assert_eq!(got.id, task.id),
        other => panic!("requeued task was not receivable: {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
