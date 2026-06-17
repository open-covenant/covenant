//! Live HTTP coverage for `GET /a2a/queue?deadline_within_ms`.
//!
//! The deadline filter is live-covered only over the CLI/socket path
//! (`live_cli_a2a_status_deadline_filter.rs`). `live_http_a2a_queue_state_filter.rs`
//! covers only `state_filter` over the gateway, so the `deadline_within_ms`
//! query param — deserialized into `LimitParams` and forwarded to
//! `Request::A2AQueue` in covenantd's HTTP layer — is never parsed-and-applied
//! through HTTP. This mirrors the CLI deadline coverage on the HTTP boundary.
//!
//! `a2a_entry_matches_deadline_within` keeps an entry only when its
//! `task.deadline_ms` is set and `<= now_ms + window`, dropping every task
//! without a deadline under an active filter. Queue three tasks against the
//! operator-as-self peer: one urgent (deadline inside the window), one far (well
//! outside it), one with no deadline at all. Under `deadline_within_ms=<window>`
//! only the urgent task id survives; the far and no-deadline ids must both be
//! absent. Asserting the absence of both — not just the presence of the urgent
//! row — is what rules out a gateway that ignores the param, and including the
//! no-deadline task proves the filter excludes missing deadlines rather than
//! merely ordering by remaining time. Assertions compare task UUIDs, not counts.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_a2a_queue_deadline_within -- --ignored live_`.

use covenant_a2a::A2ATask;
use covenant_types::AgentId;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn task_with_deadline(peer: &AgentId, intent: &str, deadline_ms: Option<u64>) -> A2ATask {
    A2ATask {
        id: Uuid::new_v4(),
        sender: peer.clone(),
        recipient: peer.clone(),
        intent_text: intent.to_string(),
        task_kind: None,
        parent: None,
        deadline_ms,
        idempotency: None,
    }
}

fn task_ids_in(value: &Value) -> Vec<String> {
    value["tasks"]
        .as_array()
        .expect("tasks array")
        .iter()
        .filter_map(|t| t.pointer("/task/id").and_then(Value::as_str))
        .map(String::from)
        .collect()
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts GET /a2a/queue?deadline_within_ms keeps only the near-deadline task"]
async fn live_http_a2a_queue_deadline_within_keeps_only_urgent() {
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

    let grant: Value = client
        .post(format!("{base}/capabilities/grant"))
        .json(&json!({ "action": format!("a2a.send.{}", peer.display) }))
        .send()
        .await
        .expect("send capability grant")
        .json()
        .await
        .expect("grant body json");
    assert_eq!(grant["kind"], "capability_granted");

    // Window is 60s. Urgent lands 30s out (inside the window), far lands 24h out
    // (well outside), and the third task carries no deadline at all. All three
    // stay queued — the deadline filter is state-independent.
    let window_ms: u64 = 60_000;
    let base_now = now_ms();
    let urgent = task_with_deadline(&peer, "urgent http deadline probe", Some(base_now + 30_000));
    let far = task_with_deadline(
        &peer,
        "far http deadline probe",
        Some(base_now + 86_400_000),
    );
    let no_deadline = task_with_deadline(&peer, "no-deadline http probe", None);

    for task in [&urgent, &far, &no_deadline] {
        let _: Value = client
            .post(format!("{base}/a2a/tasks"))
            .json(task)
            .send()
            .await
            .expect("post task")
            .json()
            .await
            .expect("task body");
    }

    let filtered: Value = client
        .get(format!("{base}/a2a/queue?limit=20&deadline_within_ms={window_ms}"))
        .send()
        .await
        .expect("queue deadline-within")
        .json()
        .await
        .expect("queue body");
    let ids = task_ids_in(&filtered);

    assert!(
        ids.contains(&urgent.id.to_string()),
        "deadline_within_ms={window_ms} must keep the urgent task: {ids:?}",
    );
    assert!(
        !ids.contains(&far.id.to_string()),
        "deadline_within_ms={window_ms} must drop the far-deadline task: {ids:?}",
    );
    assert!(
        !ids.contains(&no_deadline.id.to_string()),
        "deadline_within_ms={window_ms} must drop the task with no deadline: {ids:?}",
    );

    // Sanity: without the filter all three are visible, proving the absences
    // above are the filter at work, not tasks that never landed.
    let unfiltered: Value = client
        .get(format!("{base}/a2a/queue?limit=20"))
        .send()
        .await
        .expect("queue unfiltered")
        .json()
        .await
        .expect("queue body");
    let all = task_ids_in(&unfiltered);
    assert!(
        [&urgent, &far, &no_deadline]
            .iter()
            .all(|t| all.contains(&t.id.to_string())),
        "unfiltered /a2a/queue must surface all three task ids: {all:?}",
    );

    let _ = child.kill().await;
}
