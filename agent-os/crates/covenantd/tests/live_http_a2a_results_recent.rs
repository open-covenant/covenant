//! Live HTTP coverage for `GET /a2a/results/recent` (`recent_a2a_results`).
//!
//! Recent-result listing ships and is IPC/daemon-tested (the scrub/visibility
//! unit tests in `covenantd`), but no test drives it over the HTTP gateway.
//! That leaves the `LimitParams` query, the bearer-auth peer identity, the
//! `lookup_task_sender == peer` row scoping, the non-draining listing
//! semantics, and the raw daemon wire (`kind: "a2_a_results"`) unexercised on
//! the HTTP path. The slug matters: serde's `rename_all = "snake_case"` splits
//! `A2AResults` into `a2_a_results`, distinct from the draining
//! `a2_a_result_opt` (`/a2a/results/next`) envelope and the `a2_a_result_posted`
//! POST ack, so a dispatch collapse would be a real gateway regression.
//!
//! One scenario over the gateway exclusively: authenticate as the operator,
//! confirm `/a2a/results/recent` is an empty `a2_a_results` list before any
//! result exists (so a regression that always returns `[]` cannot pass), then
//! send + lease a self-addressed task and post its result (the operator is the
//! task sender, so the scoping join keeps the row). The listing must then carry
//! exactly that result — typed `task_id`/`status` and content marker intact —
//! and a second read must still carry it, proving the dashboard listing does
//! not drain the mailbox the way `/a2a/results/next` does.
//!
//! Hermetic — no external services. `#[ignore]`'d. Each test uses its own
//! tempdir to stay safe under `--test-threads`. Run with
//! `cargo test -p covenantd --test live_http_a2a_results_recent -- --ignored live_`.

use covenant_a2a::{A2ATask, A2ATaskResult, A2ATaskStatus};
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

async fn recent_results(client: &reqwest::Client, base: &str) -> Value {
    client
        .get(format!("{base}/a2a/results/recent?limit=10"))
        .send()
        .await
        .expect("get a2a results recent")
        .json()
        .await
        .expect("recent results body")
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts GET /a2a/results/recent lists a posted result without draining over HTTP"]
async fn live_http_a2a_results_recent_lists_without_draining_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;
    let peer = AgentId::new("user@local", read_peer_pubkey(home.path()));

    // Empty baseline: the listing is a well-formed `a2_a_results` envelope with
    // no rows before any result exists, so a regression that always returns an
    // empty array cannot later pass by matching the populated shape.
    let empty = recent_results(&client, &base).await;
    assert_eq!(
        empty["kind"], "a2_a_results",
        "recent results must be an a2_a_results envelope, not the /next a2_a_result_opt or the a2_a_result_posted ack: {empty:?}",
    );
    assert_eq!(
        empty["results"].as_array().map(|r| r.len()),
        Some(0),
        "no result has been posted yet, so the listing must be empty: {empty:?}",
    );

    // Mint one operator-owned result: send a self-addressed task, lease it, then
    // post its result. The operator is the task sender, so the
    // lookup_task_sender == peer scoping join keeps the row.
    grant(&client, &base, &format!("a2a.send.{}", peer.display)).await;
    grant(&client, &base, &format!("a2a.respond.{}", peer.display)).await;

    let task = A2ATask {
        id: Uuid::new_v4(),
        sender: peer.clone(),
        recipient: peer.clone(),
        intent_text: "http recent-results probe".to_string(),
        task_kind: None,
        parent: None,
        deadline_ms: None,
        idempotency: None,
    };
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

    const MARKER: &str = "http-recent-results-integrity-probe";
    let result = A2ATaskResult::ok(task.id, vec![Content::text(MARKER)]);
    let posted: Value = client
        .post(format!("{base}/a2a/results"))
        .json(&result)
        .send()
        .await
        .expect("post result")
        .json()
        .await
        .expect("post body");
    assert_eq!(
        posted["kind"], "a2_a_result_posted",
        "the granted result post must be accepted so a row exists to list: {posted:?}",
    );

    let listed = recent_results(&client, &base).await;
    assert_eq!(
        listed["kind"], "a2_a_results",
        "listing slug must hold: {listed:?}"
    );
    let rows = listed["results"].as_array().expect("results array");
    assert_eq!(
        rows.len(),
        1,
        "the one posted result, sent by this peer, must be listed: {listed:?}",
    );
    assert!(
        serde_json::to_string(&rows[0]).unwrap().contains(MARKER),
        "the listed result must carry the posted content over HTTP: {listed:?}",
    );
    let typed: A2ATaskResult =
        serde_json::from_value(rows[0].clone()).expect("row deserializes as A2ATaskResult");
    assert_eq!(
        typed.task_id, task.id,
        "listed result must be the posted task's"
    );
    assert_eq!(
        typed.status,
        A2ATaskStatus::Ok,
        "listed result must carry the posted Ok status",
    );

    // Reading again must still surface the row: the recent listing is a
    // non-draining dashboard view, unlike the single-delivery /a2a/results/next.
    let relisted = recent_results(&client, &base).await;
    assert_eq!(
        relisted["results"].as_array().map(|r| r.len()),
        Some(1),
        "the listing must not drain the mailbox on read: {relisted:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
