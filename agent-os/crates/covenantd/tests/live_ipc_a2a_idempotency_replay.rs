//! Live IPC coverage for A2A receiver-side idempotency replay.
//!
//! covenant-a2a dedups idempotent tasks by result cache, not by enqueue:
//! `send_task` (covenant-a2a/src/lib.rs:686) computes an idempotency cache
//! key (sender + recipient + task_kind-or-intent_text + key, but NOT
//! task.id) and, on a cache hit, synthesizes the cached result re-stamped
//! with the new task.id and pushes it straight onto the results queue
//! without enqueuing a task. The cache is populated when a leased
//! idempotent task's result is posted (`send_result`). This replay dedup
//! is unit-covered against the mailbox impls directly, but every live A2A
//! test sends exactly once and never replays a completed idempotent task,
//! so the behavior is unproven through the daemon's IPC surface where the
//! send-gate, respond-gate, and capability enforcement sit.
//!
//! Driving the raw Unix socket exclusively, the operator (both sender and
//! recipient — the cross-peer recv gate is a loopback no-op) grants itself
//! `a2a.send` and `a2a.respond`, then:
//!
//! 1. Sends an idempotent task, leases it, posts its result, and drains
//!    that result — populating the receiver-side cache.
//! 2. Re-sends a task with a fresh id but the same sender/recipient/
//!    intent_text/key. The daemon serves the cached result re-stamped with
//!    the replay id and enqueues no second task: `TryRecvA2ATask` drains to
//!    null and `TryRecvA2AResult` returns the cached content.
//! 3. Sends a third task carrying a *distinct* idempotency key, which
//!    misses the cache and enqueues normally — pinning the dedup as
//!    key-specific rather than a blanket post-result swallow.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_a2a_idempotency_replay -- --ignored live_`.

use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency, A2ATask, A2ATaskResult, A2ATaskStatus};
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
/// task is addressed to and leased from.
fn operator_agent_id(home: &Path) -> AgentId {
    let pubkey = covenant_identity::LocalIdentity::load_or_create(
        &home.join("identity").join("local.key"),
        "user@local",
    )
    .expect("identity")
    .pubkey_bytes();
    AgentId::new("user@local", pubkey)
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

/// A self-addressed idempotent task. The id is fresh each call; the cache key
/// is derived from sender/recipient/intent/key, not the id.
fn idempotent_task(peer: &AgentId, intent: &str, key: &str) -> A2ATask {
    A2ATask {
        id: Uuid::new_v4(),
        sender: peer.clone(),
        recipient: peer.clone(),
        intent_text: intent.into(),
        task_kind: None,
        parent: None,
        deadline_ms: None,
        idempotency: Some(A2AIdempotency::new(A2ADuplicateSafety::Idempotent, key)),
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + drives the A2A idempotency replay path over IPC — a re-sent completed idempotent task returns the cached result without a second enqueue, while a distinct key enqueues"]
async fn live_ipc_a2a_idempotency_replay_returns_cached_result_without_second_enqueue() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    let peer = operator_agent_id(home.path());
    for action in [
        format!("a2a.send.{}", peer.display),
        format!("a2a.respond.{}", peer.display),
    ] {
        match req(
            &mut stream,
            Request::GrantCapability {
                action: action.clone(),
                scope: None,
                expires_at: None,
            },
        )
        .await
        {
            Response::CapabilityGranted { .. } => {}
            other => panic!("expected Response::CapabilityGranted for {action}, got {other:?}"),
        }
    }

    const INTENT: &str = "ipc-a2a-idempotency-replay-probe";
    const KEY: &str = "idem-key-alpha";
    const MARKER: &str = "ipc-a2a-idempotency-cached-content";

    // 1. Send an idempotent task, lease it, post its result. This populates the
    //    receiver-side result cache for (sender, recipient, intent, KEY).
    let first = idempotent_task(&peer, INTENT, KEY);
    match req(&mut stream, Request::SendA2ATask { task: first.clone() }).await {
        Response::A2ATaskQueued { task_id } => {
            assert_eq!(task_id, first.id, "the queued id must echo the sent task")
        }
        other => panic!("expected Response::A2ATaskQueued for the first send, got {other:?}"),
    }
    match req(&mut stream, Request::TryRecvA2ATask).await {
        Response::A2ATaskOpt { task: Some(t) } => {
            assert_eq!(t.id, first.id, "the leased task must be the one queued")
        }
        other => panic!("expected Response::A2ATaskOpt with the leased task, got {other:?}"),
    }
    match req(
        &mut stream,
        Request::PostA2AResult {
            result: A2ATaskResult::ok(first.id, vec![Content::text(MARKER)]),
        },
    )
    .await
    {
        Response::A2AResultPosted { task_id } => {
            assert_eq!(task_id, first.id, "the posted result must key off the task")
        }
        other => panic!("expected Response::A2AResultPosted, got {other:?}"),
    }

    // Drain the first result so the queue is empty before the replay, and
    // confirm the normal post path delivers to the sender.
    match req(&mut stream, Request::TryRecvA2AResult).await {
        Response::A2AResultOpt { result: Some(r) } => {
            assert_eq!(r.task_id, first.id, "the drained result must be the first task's");
            assert_eq!(r.status, A2ATaskStatus::Ok, "the posted status must round-trip");
        }
        other => panic!("expected Response::A2AResultOpt with the first result, got {other:?}"),
    }

    // 2. Replay: re-send with a fresh id but the SAME sender/recipient/intent/
    //    key. The daemon must serve the cached result without enqueuing again.
    let replay = idempotent_task(&peer, INTENT, KEY);
    assert_ne!(replay.id, first.id, "the replay must carry a distinct task id");
    match req(&mut stream, Request::SendA2ATask { task: replay.clone() }).await {
        Response::A2ATaskQueued { task_id } => assert_eq!(
            task_id, replay.id,
            "the enqueue ack echoes the posted id on the cache-hit path too"
        ),
        other => panic!("expected Response::A2ATaskQueued for the replay, got {other:?}"),
    }

    // 2a. No second task was enqueued — the queue drains to null.
    match req(&mut stream, Request::TryRecvA2ATask).await {
        Response::A2ATaskOpt { task } => assert!(
            task.is_none(),
            "an idempotent replay of a completed task must enqueue no second task: {task:?}"
        ),
        other => panic!("expected Response::A2ATaskOpt after the replay, got {other:?}"),
    }

    // 2b. The cached result is served for the replay, re-stamped with its id.
    match req(&mut stream, Request::TryRecvA2AResult).await {
        Response::A2AResultOpt { result: Some(r) } => {
            assert_eq!(
                r.task_id, replay.id,
                "the replayed result must be re-stamped with the replay task id"
            );
            assert_eq!(
                r.status,
                A2ATaskStatus::Ok,
                "the cached status must survive replay: {r:?}"
            );
            let json = serde_json::to_string(&r).expect("serialize replayed result");
            assert!(
                json.contains(MARKER),
                "the replayed result must carry the cached content marker: {json}"
            );
        }
        other => panic!("expected Response::A2AResultOpt with the cached result, got {other:?}"),
    }

    // 3. Negative control: a re-send carrying a DISTINCT idempotency key misses
    //    the cache and enqueues normally, proving the dedup is key-specific.
    let distinct = idempotent_task(&peer, INTENT, "idem-key-beta");
    match req(&mut stream, Request::SendA2ATask { task: distinct.clone() }).await {
        Response::A2ATaskQueued { task_id } => assert_eq!(task_id, distinct.id),
        other => panic!("expected Response::A2ATaskQueued for the distinct-key send, got {other:?}"),
    }
    match req(&mut stream, Request::TryRecvA2ATask).await {
        Response::A2ATaskOpt { task: Some(t) } => assert_eq!(
            t.id, distinct.id,
            "a send under a distinct idempotency key must enqueue and lease"
        ),
        other => panic!("expected Response::A2ATaskOpt with the distinct-key task, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
