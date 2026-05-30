//! Live HTTP coverage for `POST /a2a/repair` (`repair_a2a_task`) — the
//! requeue allow-path.
//!
//! A2A task repair is live-tested over the CLI by `live_cli_a2a_repair.rs`
//! and gated in-process, and the HTTP delegated-denial path is pinned by
//! `live_http_a2a_recovery_delegated_denial.rs`. But that denial test hits
//! the capability gate with a random task id, so the repair never reaches a
//! real in-flight task — the requeue itself is never exercised over the
//! gateway. Note the gateway returns the daemon's raw `a2_a_repaired` slug.
//!
//! An in-flight task is created hermetically by sending one task and leasing
//! it over HTTP, then reading `GET /a2a/queue` for its real `lease_id` (the
//! requeue command must carry the matching lease or `assert_lease_match`
//! rejects it).
//!
//! Two scenarios:
//! - requeue round-trip: an operator holding `a2a.repair.requeue` POSTs a
//!   `Requeue` command for the leased task and receives `kind:
//!   "a2_a_repaired"` with `outcome.action: "requeued"` and `attempt: 1`; the
//!   task returns to the queue in the `queued` state.
//! - scope denial: the operator without the grant is rejected by name
//!   (`a2a.repair.requeue`) and the task survives in flight — proven by a
//!   follow-up granted repair that requeues it with the same lease.
//!
//! Hermetic — no external services. `#[ignore]`'d. Each test uses its own
//! tempdir to stay safe under `--test-threads`. Run with
//! `cargo test -p covenantd --test live_http_a2a_repair -- --ignored live_`.

use covenant_a2a::{A2ADuplicateRisk, A2ARepairCommand, A2ARepairRequest, A2ATask};
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

async fn queue_entry(client: &reqwest::Client, base: &str, task_id: &Uuid) -> Value {
    let queue: Value = client
        .get(format!("{base}/a2a/queue?limit=20"))
        .send()
        .await
        .expect("get a2a queue")
        .json()
        .await
        .expect("a2a queue body");
    queue["tasks"]
        .as_array()
        .expect("queue carries a tasks array")
        .iter()
        .find(|entry| {
            entry.pointer("/task/id").and_then(Value::as_str) == Some(task_id.to_string().as_str())
        })
        .cloned()
        .unwrap_or_else(|| panic!("task {task_id} is not in the queue: {queue:?}"))
}

async fn post_repair(client: &reqwest::Client, base: &str, request: &A2ARepairRequest) -> Value {
    client
        .post(format!("{base}/a2a/repair"))
        .json(request)
        .send()
        .await
        .expect("post a2a repair")
        .json()
        .await
        .expect("a2a repair body")
}

/// Send one self-addressed task over HTTP and lease it, leaving it `InFlight`.
/// Returns the task id and the real lease id the requeue command must echo.
async fn lease_in_flight_task(
    client: &reqwest::Client,
    base: &str,
    peer: &AgentId,
) -> (Uuid, Uuid) {
    grant(client, base, &format!("a2a.send.{}", peer.display)).await;

    let task = task_for(peer, "http repair probe");
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

    let entry = queue_entry(client, base, &task.id).await;
    assert_eq!(
        entry["state"], "in_flight",
        "the leased task must be in flight before repair: {entry:?}",
    );
    let lease_id: Uuid = entry["lease_id"]
        .as_str()
        .expect("in-flight entry carries a lease_id")
        .parse()
        .expect("lease_id is a uuid");

    (task.id, lease_id)
}

fn requeue(task_id: Uuid, lease_id: Uuid) -> A2ARepairRequest {
    A2ARepairRequest {
        task_id,
        command: A2ARepairCommand::Requeue {
            lease_id: Some(lease_id),
            duplicate_risk: A2ADuplicateRisk::Idempotent,
        },
        reason: "http repair requeue probe".into(),
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts POST /a2a/repair requeues a leased task over HTTP"]
async fn live_http_a2a_repair_requeues_in_flight_task_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;
    let peer = AgentId::new("user@local", read_peer_pubkey(home.path()));

    let (task_id, lease_id) = lease_in_flight_task(&client, &base, &peer).await;

    grant(&client, &base, "a2a.repair.requeue").await;

    let repaired = post_repair(&client, &base, &requeue(task_id, lease_id)).await;
    assert_eq!(
        repaired["kind"], "a2_a_repaired",
        "granted repair must be accepted over HTTP with the daemon's raw slug: {repaired:?}",
    );
    assert_eq!(
        repaired.pointer("/outcome/action").and_then(Value::as_str),
        Some("requeued"),
        "the repair outcome must report a requeue: {repaired:?}",
    );
    assert_eq!(
        repaired.pointer("/outcome/state").and_then(Value::as_str),
        Some("queued"),
        "a requeued task must land back in the queued state: {repaired:?}",
    );
    assert_eq!(
        repaired.pointer("/outcome/attempt").and_then(Value::as_u64),
        Some(1),
        "the first requeue must report attempt 1: {repaired:?}",
    );

    let entry = queue_entry(&client, &base, &task_id).await;
    assert_eq!(
        entry["state"], "queued",
        "the repaired task must be back in the queue, not in flight: {entry:?}",
    );
    assert_eq!(
        entry["attempt"].as_u64(),
        Some(1),
        "the requeued task must carry the incremented attempt: {entry:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts a no-grant POST /a2a/repair is rejected and leaves the task in flight"]
async fn live_http_a2a_repair_requires_grant_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;
    let peer = AgentId::new("user@local", read_peer_pubkey(home.path()));

    let (task_id, lease_id) = lease_in_flight_task(&client, &base, &peer).await;

    // No a2a.repair.requeue grant. The operator authenticates but lacks the
    // capability, so the repair must be rejected by name before any mutation.
    let rejected = post_repair(&client, &base, &requeue(task_id, lease_id)).await;
    assert_eq!(
        rejected["kind"], "error",
        "an ungranted repair must be rejected: {rejected:?}",
    );
    assert!(
        rejected["message"]
            .as_str()
            .unwrap_or_default()
            .contains("a2a.repair.requeue"),
        "rejection must name the missing capability: {rejected:?}",
    );

    // The denied repair must have left the task in flight, lease untouched.
    let entry = queue_entry(&client, &base, &task_id).await;
    assert_eq!(
        entry["state"], "in_flight",
        "a denied repair must not mutate the leased task: {entry:?}",
    );

    // Granting the capability and retrying with the same lease requeues it,
    // proving the denial neither dropped the task nor disturbed its lease.
    grant(&client, &base, "a2a.repair.requeue").await;
    let repaired = post_repair(&client, &base, &requeue(task_id, lease_id)).await;
    assert_eq!(
        repaired["kind"], "a2_a_repaired",
        "the granted retry must be accepted: {repaired:?}",
    );
    assert_eq!(
        repaired.pointer("/outcome/action").and_then(Value::as_str),
        Some("requeued"),
        "the in-flight task must survive a denied repair and requeue once granted: {repaired:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
