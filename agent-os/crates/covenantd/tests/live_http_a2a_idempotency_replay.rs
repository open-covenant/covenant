//! Live HTTP coverage for A2A receiver-side idempotency replay.
//!
//! covenant-a2a dedups idempotent tasks by result cache, not at enqueue:
//! `send_task` (covenant-a2a/src/lib.rs:686) computes an idempotency cache
//! key (sender + recipient + task_kind-or-intent_text + key, but NOT
//! task.id) and, on a cache hit, synthesizes the cached result re-stamped
//! with the new task.id and pushes it straight onto the results queue
//! without enqueuing a task. The cache is populated when a leased
//! idempotent task's result is posted (`send_result`). The IPC boundary is
//! covered by `live_ipc_a2a_idempotency_replay.rs`; this mirrors it over
//! the HTTP gateway, where `POST /a2a/tasks` carries the send-gate and
//! `POST /a2a/results` carries the respond-gate.
//!
//! Using the HTTP gateway exclusively, the operator (both sender and
//! recipient — the cross-peer recv gate is a loopback no-op) grants itself
//! `a2a.send` and `a2a.respond`, then:
//!
//! 1. Posts an idempotent task, leases it, posts its result, and drains
//!    that result — populating the receiver-side cache.
//! 2. Re-posts a task with a fresh id but the same sender/recipient/
//!    intent_text/key. The daemon serves the cached result re-stamped with
//!    the replay id and enqueues no second task: `GET /a2a/tasks/next`
//!    drains to a null task and `GET /a2a/results/next` returns the cached
//!    content.
//! 3. Posts a third task carrying a *distinct* idempotency key, which
//!    misses the cache and enqueues normally — pinning the dedup as
//!    key-specific rather than a blanket post-result swallow.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_a2a_idempotency_replay -- --ignored live_`.

use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency, A2ATask, A2ATaskResult, A2ATaskStatus};
use covenant_mcp::Content;
use covenant_types::AgentId;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;
use uuid::Uuid;

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
        if let Ok(text) = std::fs::read_to_string(&path) {
            let token = text.trim();
            if !token.is_empty() {
                return token.to_string();
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("operator token never appeared at {}", path.display());
}

async fn wait_for_http(base: &str) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .expect("reqwest client");
    for _ in 0..80 {
        match client.get(format!("{base}/health")).send().await {
            Ok(response) if response.status().is_success() => return,
            _ => sleep(Duration::from_millis(50)).await,
        }
    }
    panic!("http gateway never became healthy at {base}/health");
}

fn read_peer_pubkey(home: &Path) -> [u8; 32] {
    let id = covenant_identity::LocalIdentity::load_or_create(
        &home.join("identity").join("local.key"),
        "user@local",
    )
    .expect("load identity");
    id.pubkey_bytes()
}

/// A self-addressed idempotent task. The id is fresh each call; the cache key
/// is derived from sender/recipient/intent/key, not the id.
fn idempotent_task(peer: &AgentId, intent: &str, key: &str) -> A2ATask {
    A2ATask {
        id: Uuid::new_v4(),
        sender: peer.clone(),
        recipient: peer.clone(),
        intent_text: intent.to_string(),
        task_kind: None,
        parent: None,
        deadline_ms: None,
        idempotency: Some(A2AIdempotency::new(A2ADuplicateSafety::Idempotent, key)),
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + drives the A2A idempotency replay path over HTTP — a re-posted completed idempotent task returns the cached result without a second enqueue, while a distinct key enqueues"]
async fn live_http_a2a_idempotency_replay_returns_cached_result_without_second_enqueue() {
    let home = tempfile::tempdir().expect("tempdir");

    let port = pick_free_port();
    let base = format!("http://127.0.0.1:{port}");
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
    wait_for_http(&base).await;

    let token = read_operator_token(home.path()).await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .expect("reqwest client");

    let pubkey = read_peer_pubkey(home.path());
    let peer = AgentId::new("user@local", pubkey);

    for action in [
        format!("a2a.send.{}", peer.display),
        format!("a2a.respond.{}", peer.display),
    ] {
        let granted: Value = client
            .post(format!("{base}/capabilities/grant"))
            .json(&json!({ "action": action }))
            .send()
            .await
            .expect("grant capability")
            .json()
            .await
            .expect("grant body");
        assert_eq!(
            granted["kind"], "capability_granted",
            "grant failed: {granted:?}"
        );
    }

    const INTENT: &str = "http-a2a-idempotency-replay-probe";
    const KEY: &str = "idem-key-alpha";
    const MARKER: &str = "http-a2a-idempotency-cached-content";

    // 1. Post an idempotent task, lease it, post its result. This populates the
    //    receiver-side result cache for (sender, recipient, intent, KEY).
    let first = idempotent_task(&peer, INTENT, KEY);
    let queued: Value = client
        .post(format!("{base}/a2a/tasks"))
        .json(&first)
        .send()
        .await
        .expect("post first task")
        .json()
        .await
        .expect("first task body");
    assert_eq!(
        queued["kind"], "a2_a_task_queued",
        "first enqueue ack: {queued:?}"
    );
    assert_eq!(
        queued.pointer("/task_id").and_then(Value::as_str),
        Some(first.id.to_string().as_str()),
        "first ack must echo the posted id: {queued:?}",
    );

    let leased: Value = client
        .get(format!("{base}/a2a/tasks/next"))
        .send()
        .await
        .expect("lease first task")
        .json()
        .await
        .expect("lease body");
    assert_eq!(
        leased.pointer("/task/id").and_then(Value::as_str),
        Some(first.id.to_string().as_str()),
        "lease must return the dispatched task id: {leased:?}",
    );

    let result = A2ATaskResult::ok(first.id, vec![Content::text(MARKER)]);
    let posted: Value = client
        .post(format!("{base}/a2a/results"))
        .json(&result)
        .send()
        .await
        .expect("post first result")
        .json()
        .await
        .expect("post result body");
    assert_eq!(
        posted["kind"], "a2_a_result_posted",
        "result post ack: {posted:?}"
    );
    assert_eq!(
        posted.pointer("/task_id").and_then(Value::as_str),
        Some(first.id.to_string().as_str()),
        "result post must echo the task id: {posted:?}",
    );

    // Drain the first result so the queue is empty before the replay, and
    // confirm the normal post path delivers to the sender over HTTP.
    let drained_first: Value = client
        .get(format!("{base}/a2a/results/next"))
        .send()
        .await
        .expect("drain first result")
        .json()
        .await
        .expect("drain body");
    assert_eq!(drained_first["kind"], "a2_a_result_opt");
    let first_row = drained_first
        .get("result")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| panic!("first result must be readable: {drained_first:?}"));
    let first_typed: A2ATaskResult =
        serde_json::from_value(first_row).expect("first result deserializes");
    assert_eq!(
        first_typed.task_id, first.id,
        "drained result must be the first task's"
    );
    assert_eq!(first_typed.status, A2ATaskStatus::Ok);

    // 2. Replay: re-post with a fresh id but the SAME sender/recipient/intent/
    //    key. The daemon must serve the cached result without enqueuing again.
    let replay = idempotent_task(&peer, INTENT, KEY);
    assert_ne!(
        replay.id, first.id,
        "the replay must carry a distinct task id"
    );
    let replay_ack: Value = client
        .post(format!("{base}/a2a/tasks"))
        .json(&replay)
        .send()
        .await
        .expect("post replay task")
        .json()
        .await
        .expect("replay ack body");
    assert_eq!(
        replay_ack["kind"], "a2_a_task_queued",
        "the enqueue ack fires on the cache-hit path too: {replay_ack:?}",
    );

    // 2a. No second task was enqueued — the queue drains to a null task.
    let post_replay_queue: Value = client
        .get(format!("{base}/a2a/tasks/next"))
        .send()
        .await
        .expect("read tasks after replay")
        .json()
        .await
        .expect("tasks body");
    assert_eq!(post_replay_queue["kind"], "a2_a_task_opt");
    assert!(
        post_replay_queue["task"].is_null(),
        "an idempotent replay of a completed task must enqueue no second task: {post_replay_queue:?}",
    );

    // 2b. The cached result is served for the replay, re-stamped with its id.
    let replayed: Value = client
        .get(format!("{base}/a2a/results/next"))
        .send()
        .await
        .expect("read replayed result")
        .json()
        .await
        .expect("replayed body");
    assert_eq!(replayed["kind"], "a2_a_result_opt");
    let replay_row = replayed
        .get("result")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| panic!("the replay must serve the cached result: {replayed:?}"));
    assert!(
        serde_json::to_string(&replay_row).unwrap().contains(MARKER),
        "the replayed result must carry the cached content marker: {replay_row:?}",
    );
    let replay_typed: A2ATaskResult =
        serde_json::from_value(replay_row).expect("replayed result deserializes");
    assert_eq!(
        replay_typed.task_id, replay.id,
        "the replayed result must be re-stamped with the replay task id",
    );
    assert_eq!(
        replay_typed.status,
        A2ATaskStatus::Ok,
        "the cached status must survive replay",
    );

    // 3. Negative control: a re-post under a DISTINCT idempotency key misses
    //    the cache and enqueues normally, proving the dedup is key-specific.
    let distinct = idempotent_task(&peer, INTENT, "idem-key-beta");
    let distinct_ack: Value = client
        .post(format!("{base}/a2a/tasks"))
        .json(&distinct)
        .send()
        .await
        .expect("post distinct-key task")
        .json()
        .await
        .expect("distinct ack body");
    assert_eq!(
        distinct_ack["kind"], "a2_a_task_queued",
        "distinct-key ack: {distinct_ack:?}"
    );
    let distinct_leased: Value = client
        .get(format!("{base}/a2a/tasks/next"))
        .send()
        .await
        .expect("lease distinct-key task")
        .json()
        .await
        .expect("distinct lease body");
    assert_eq!(
        distinct_leased.pointer("/task/id").and_then(Value::as_str),
        Some(distinct.id.to_string().as_str()),
        "a post under a distinct idempotency key must enqueue and lease: {distinct_leased:?}",
    );

    let _ = child.kill().await;
}
