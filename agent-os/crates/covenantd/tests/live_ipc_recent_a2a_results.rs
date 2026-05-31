//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::RecentA2AResults` over the raw IPC socket across a full A2A task
//! lifecycle — send, lease, post a result, then list it without draining.
//!
//! The verb is covered today over HTTP (`live_http_a2a_results_recent.rs`, GET
//! `/a2a/results/recent`) but never over the raw Unix socket both are built on.
//! This pins that wire contract: `Response::A2AResults { results }`
//! (covenant-ipc/src/lib.rs:878), and the queued/leased/posted handshake it
//! depends on — `A2ATaskQueued`, `A2ATaskOpt`, `A2AResultPosted`.
//!
//! Seeded through the public API: the operator grants itself `a2a.send.<display>`
//! and `a2a.respond.<display>`, sends one self-addressed task, leases it back
//! (operator is both sender and recipient), and posts an `Ok` result carrying a
//! unique marker. The empty baseline stops a regression that always returns
//! `[]`; the marker proves the listed result is the one posted; the re-read
//! proves the listing does not drain. Hermetic — no external services.
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_recent_a2a_results -- --ignored live_`.

use covenant_a2a::{A2ATask, A2ATaskResult, A2ATaskStatus};
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

#[tokio::test]
#[ignore = "live: spawns covenantd + drives Request::RecentA2AResults over the socket"]
async fn live_ipc_recent_a2a_results_lists_without_draining() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    // Empty baseline: a fresh mailbox lists no results. This stops a regression
    // that always returns [] from masquerading as the populated path.
    match req(&mut stream, Request::RecentA2AResults { limit: 10 }).await {
        Response::A2AResults { results } => assert!(
            results.is_empty(),
            "a fresh mailbox has no A2A results: {results:?}"
        ),
        other => panic!("expected Response::A2AResults, got {other:?}"),
    }

    // Grant the send and respond scopes the lifecycle needs.
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

    // Send a self-addressed task: the operator is both sender and recipient.
    let task = A2ATask {
        id: Uuid::new_v4(),
        sender: peer.clone(),
        recipient: peer.clone(),
        intent_text: "ipc recent-results probe".into(),
        task_kind: None,
        parent: None,
        deadline_ms: None,
        idempotency: None,
    };
    match req(&mut stream, Request::SendA2ATask { task: task.clone() }).await {
        Response::A2ATaskQueued { task_id } => {
            assert_eq!(task_id, task.id, "the queued id must echo the sent task")
        }
        other => panic!("expected Response::A2ATaskQueued, got {other:?}"),
    }

    // Lease it back, then post an Ok result carrying a unique marker.
    match req(&mut stream, Request::TryRecvA2ATask).await {
        Response::A2ATaskOpt { task: Some(t) } => {
            assert_eq!(t.id, task.id, "the leased task must be the one queued")
        }
        other => panic!("expected Response::A2ATaskOpt with the leased task, got {other:?}"),
    }

    const MARKER: &str = "ipc-recent-results-integrity-probe";
    match req(
        &mut stream,
        Request::PostA2AResult {
            result: A2ATaskResult::ok(task.id, vec![Content::text(MARKER)]),
        },
    )
    .await
    {
        Response::A2AResultPosted { task_id } => {
            assert_eq!(task_id, task.id, "the posted result must key off the task")
        }
        other => panic!("expected Response::A2AResultPosted, got {other:?}"),
    }

    // The posted result now lists, keyed by task, Ok, and carrying the marker.
    match req(&mut stream, Request::RecentA2AResults { limit: 10 }).await {
        Response::A2AResults { results } => {
            assert_eq!(
                results.len(),
                1,
                "exactly one result was posted: {results:?}"
            );
            assert_eq!(
                results[0].task_id, task.id,
                "the result must key off the task: {:?}",
                results[0]
            );
            assert_eq!(
                results[0].status,
                A2ATaskStatus::Ok,
                "the posted status must round-trip: {:?}",
                results[0]
            );
            let json = serde_json::to_string(&results[0]).expect("serialize result");
            assert!(
                json.contains(MARKER),
                "the listed result must carry the posted content marker: {json}"
            );
        }
        other => panic!("expected Response::A2AResults, got {other:?}"),
    }

    // Non-draining: reading the mailbox does not consume the result.
    match req(&mut stream, Request::RecentA2AResults { limit: 10 }).await {
        Response::A2AResults { results } => assert_eq!(
            results.len(),
            1,
            "RecentA2AResults is a non-draining read; the row must persist: {results:?}"
        ),
        other => panic!("expected Response::A2AResults, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
