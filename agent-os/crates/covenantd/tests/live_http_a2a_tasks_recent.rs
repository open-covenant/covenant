//! Live HTTP coverage for `GET /a2a/tasks/recent` (`recent_a2a_tasks`).
//!
//! Recent-task listing ships and is daemon-tested, but no test drives it over
//! the HTTP gateway. That leaves the `LimitParams` query, the bearer-auth peer
//! identity, the both-sides `sender == peer || recipient == peer` row scoping,
//! the non-draining listing semantics, and the raw daemon wire unexercised on
//! the HTTP path. The wire shape matters: `recent_a2a_tasks` returns
//! `Response::A2ATasks` (`kind: "a2_a_tasks"`, a `tasks` array only), which is
//! a different variant from `/a2a/queue`'s `Response::A2AQueue`
//! (`kind: "a2_a_queue"`, carrying both `tasks` and `results`). A dispatch
//! collapse between the two would be a real gateway regression.
//!
//! One scenario over the gateway exclusively: authenticate as the operator,
//! confirm `/a2a/tasks/recent` is an empty `a2_a_tasks` list before any task
//! exists, then send one self-addressed task (the operator is both sender and
//! recipient, so the scoping filter keeps it). The listing must then carry
//! exactly that task — a typed matching id — with no `results` key (proving the
//! A2ATasks variant, not A2AQueue), and a second read must still carry it,
//! proving the listing does not drain queue state on view.
//!
//! Hermetic — no external services. `#[ignore]`'d. Each test uses its own
//! tempdir to stay safe under `--test-threads`. Run with
//! `cargo test -p covenantd --test live_http_a2a_tasks_recent -- --ignored live_`.

use covenant_a2a::A2ATask;
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

async fn recent_tasks(client: &reqwest::Client, base: &str) -> Value {
    client
        .get(format!("{base}/a2a/tasks/recent?limit=10"))
        .send()
        .await
        .expect("get a2a tasks recent")
        .json()
        .await
        .expect("recent tasks body")
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts GET /a2a/tasks/recent lists a sent task without draining over HTTP"]
async fn live_http_a2a_tasks_recent_lists_without_draining_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;
    let peer = AgentId::new("user@local", read_peer_pubkey(home.path()));

    // Empty baseline: an `a2_a_tasks` envelope with no rows before any task
    // exists, so a regression that always returns `[]` cannot later pass by
    // matching the populated shape.
    let empty = recent_tasks(&client, &base).await;
    assert_eq!(
        empty["kind"], "a2_a_tasks",
        "recent tasks must be an a2_a_tasks envelope, not the a2_a_queue shape: {empty:?}",
    );
    assert!(
        empty.get("results").is_none(),
        "the A2ATasks listing variant carries no results key (that is A2AQueue): {empty:?}",
    );
    assert_eq!(
        empty["tasks"].as_array().map(|t| t.len()),
        Some(0),
        "no task has been sent yet, so the listing must be empty: {empty:?}",
    );

    // Send one self-addressed task; the operator is both sender and recipient,
    // so the both-sides scoping filter keeps it.
    grant(&client, &base, &format!("a2a.send.{}", peer.display)).await;
    let task = A2ATask {
        id: Uuid::new_v4(),
        sender: peer.clone(),
        recipient: peer.clone(),
        intent_text: "http recent-tasks probe".to_string(),
        task_kind: None,
        parent: None,
        deadline_ms: None,
        idempotency: None,
    };
    let sent: Value = client
        .post(format!("{base}/a2a/tasks"))
        .json(&task)
        .send()
        .await
        .expect("post task")
        .json()
        .await
        .expect("task body");
    assert_ne!(
        sent["kind"], "error",
        "the granted task send must be accepted so a row exists to list: {sent:?}",
    );

    let listed = recent_tasks(&client, &base).await;
    assert_eq!(
        listed["kind"], "a2_a_tasks",
        "listing slug must hold: {listed:?}"
    );
    assert!(
        listed.get("results").is_none(),
        "the populated listing must still be the A2ATasks variant: {listed:?}",
    );
    let rows = listed["tasks"].as_array().expect("tasks array");
    assert_eq!(
        rows.len(),
        1,
        "the one sent task, addressed to and from this peer, must be listed: {listed:?}",
    );
    let typed: A2ATask =
        serde_json::from_value(rows[0].clone()).expect("row deserializes as A2ATask");
    assert_eq!(
        typed.id, task.id,
        "listed task must be the one that was sent"
    );

    // Reading again must still surface the row: the recent listing is a
    // non-draining view, not a queue pop.
    let relisted = recent_tasks(&client, &base).await;
    assert_eq!(
        relisted["tasks"].as_array().map(|t| t.len()),
        Some(1),
        "the listing must not drain queue state on read: {relisted:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
