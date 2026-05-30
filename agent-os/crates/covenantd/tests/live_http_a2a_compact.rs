//! Live HTTP coverage for `POST /a2a/compact` (`compact_a2a`).
//!
//! A2A mailbox compaction is live-tested over the CLI by
//! `live_cli_a2a_compact_json.rs` and gated at the unit level, but no test
//! drives it over the HTTP gateway — leaving the `a2a.compact` gate and the
//! actual event-log drop unexercised on the HTTP path. Note the gateway
//! returns the daemon's raw `a2_a_compacted` slug, not the CLI's
//! `a2a_compacted` remap.
//!
//! A droppable task is created hermetically by driving one task through its
//! full lifecycle over HTTP — send, lease, respond, post result, receive —
//! which leaves it `seen + delivered + posted == drained`, the predicate
//! `compute_droppable_task_ids` reaps.
//!
//! Two scenarios:
//! - compact round-trip: an operator holding `a2a.compact` POSTs and receives
//!   `kind: "a2_a_compacted"` with `dropped >= 1` (the drained task's events).
//! - scope denial: the operator without the grant is rejected by name
//!   (`a2a.compact`) and the droppable task survives — proven by a follow-up
//!   granted compact that still drops it.
//!
//! Hermetic — no external services. `#[ignore]`'d. Each test uses its own
//! tempdir to stay safe under `--test-threads`. Run with
//! `cargo test -p covenantd --test live_http_a2a_compact -- --ignored live_`.

use covenant_a2a::{A2ATask, A2ATaskResult};
use covenant_mcp::Content;
use covenant_types::AgentId;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
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

async fn spawn_http_daemon(home: &Path) -> (Child, String) {
    let port = pick_free_port();
    let base = format!("http://127.0.0.1:{port}");
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
    wait_for_http(&base).await;
    (child, base)
}

async fn operator_client(home: &Path) -> reqwest::Client {
    let token = read_operator_token(home).await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .expect("reqwest client")
}

fn read_peer_pubkey(home: &Path) -> [u8; 32] {
    let id = covenant_identity::LocalIdentity::load_or_create(
        &home.join("identity").join("local.key"),
        "user@local",
    )
    .expect("load identity");
    id.pubkey_bytes()
}

fn task_for(peer: &AgentId, intent: &str) -> A2ATask {
    A2ATask {
        id: Uuid::new_v4(),
        sender: peer.clone(),
        recipient: peer.clone(),
        intent_text: intent.to_string(),
        task_kind: None,
        parent: None,
        deadline_ms: None,
        idempotency: None,
    }
}

async fn grant(client: &reqwest::Client, base: &str, action: &str) {
    let granted: Value = client
        .post(format!("{base}/capabilities/grant"))
        .json(&json!({ "action": action }))
        .send()
        .await
        .expect("send capability grant")
        .json()
        .await
        .expect("grant body json");
    assert_eq!(
        granted["kind"], "capability_granted",
        "grant {action} must succeed: {granted:?}",
    );
}

async fn post_compact(client: &reqwest::Client, base: &str) -> Value {
    client
        .post(format!("{base}/a2a/compact"))
        .send()
        .await
        .expect("post a2a compact")
        .json()
        .await
        .expect("a2a compact body")
}

/// Drive one task through its full lifecycle over HTTP — send, lease, respond,
/// post result, receive — leaving it `seen + delivered + posted == drained`,
/// which `compute_droppable_task_ids` reaps. Both grants are self-scoped to the
/// operator's own identity.
async fn seed_droppable_task(client: &reqwest::Client, base: &str, peer: &AgentId) {
    grant(client, base, &format!("a2a.send.{}", peer.display)).await;

    let task = task_for(peer, "http compact probe");
    let _: Value = client
        .post(format!("{base}/a2a/tasks"))
        .json(&task)
        .send()
        .await
        .expect("post task")
        .json()
        .await
        .expect("task body");
    let leased: Value = client
        .get(format!("{base}/a2a/tasks/next"))
        .send()
        .await
        .expect("lease task")
        .json()
        .await
        .expect("lease body");
    assert_eq!(
        leased.pointer("/task/id").and_then(Value::as_str),
        Some(task.id.to_string().as_str()),
        "lease must return the dispatched task id: {leased:?}",
    );

    grant(client, base, &format!("a2a.respond.{}", peer.display)).await;

    let result = A2ATaskResult::ok(task.id, vec![Content::text("http compact result probe")]);
    let posted: Value = client
        .post(format!("{base}/a2a/results"))
        .json(&result)
        .send()
        .await
        .expect("post result")
        .json()
        .await
        .expect("post result body");
    assert_eq!(
        posted["kind"], "a2_a_result_posted",
        "result post must be accepted so the task can drain: {posted:?}",
    );

    // Receiving the result emits ResultRecv, making posted == drained == 1 so
    // the task crosses into droppable.
    let received: Value = client
        .get(format!("{base}/a2a/results/next"))
        .send()
        .await
        .expect("receive result")
        .json()
        .await
        .expect("receive body");
    assert!(
        !received["result"].is_null(),
        "the posted result must be received so the task becomes droppable: {received:?}",
    );
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts POST /a2a/compact drops a fully-drained task over HTTP"]
async fn live_http_a2a_compact_round_trips_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;
    let peer = AgentId::new("user@local", read_peer_pubkey(home.path()));

    seed_droppable_task(&client, &base, &peer).await;

    grant(&client, &base, "a2a.compact").await;

    let compacted = post_compact(&client, &base).await;
    assert_eq!(
        compacted["kind"], "a2_a_compacted",
        "granted compact must be accepted over HTTP with the daemon's raw slug: {compacted:?}",
    );
    assert!(
        compacted["dropped"].as_u64().unwrap_or(0) >= 1,
        "compact must drop the fully-drained task's events: {compacted:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts a no-grant POST /a2a/compact is rejected and leaves the droppable task"]
async fn live_http_a2a_compact_requires_grant_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;
    let peer = AgentId::new("user@local", read_peer_pubkey(home.path()));

    seed_droppable_task(&client, &base, &peer).await;

    // No a2a.compact grant. The operator authenticates but lacks the
    // capability, so the compact must be rejected by name before any drop.
    let rejected = post_compact(&client, &base).await;
    assert_eq!(
        rejected["kind"], "error",
        "an ungranted compact must be rejected: {rejected:?}",
    );
    assert!(
        rejected["message"]
            .as_str()
            .unwrap_or_default()
            .contains("a2a.compact"),
        "rejection must name the missing capability: {rejected:?}",
    );

    // The denied compact must have left the droppable task intact: granting the
    // capability and retrying still drops it.
    grant(&client, &base, "a2a.compact").await;
    let compacted = post_compact(&client, &base).await;
    assert_eq!(
        compacted["kind"], "a2_a_compacted",
        "the granted retry must be accepted: {compacted:?}",
    );
    assert!(
        compacted["dropped"].as_u64().unwrap_or(0) >= 1,
        "the droppable task must survive a denied compact and be dropped once granted: {compacted:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
