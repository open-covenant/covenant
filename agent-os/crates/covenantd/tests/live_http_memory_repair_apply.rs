//! Live HTTP coverage for the apply path of `POST /memory/repair`
//! (`memory_repair`).
//!
//! The repair mutator has HTTP delegated-denial coverage
//! (`live_http_memory_repair_delegated_denial.rs`), but no test drives the
//! apply allowance over the gateway, leaving the `memory.repair.apply` gate,
//! the owner-visibility check, and the actual record mutation unexercised on
//! the HTTP path.
//!
//! Two scenarios:
//! - apply round-trip: an operator holding `memory.repair.apply` POSTs a
//!   `DeleteRecord` apply against a seeded operator-owned record and receives
//!   `kind: "memory_repaired"` with `outcome.changed` and `outcome.would_change`
//!   true; after the daemon exits the record is gone from the store.
//! - scope denial: the operator without the grant is rejected by name
//!   (`memory.repair.apply`) and the record survives untouched.
//!
//! Hermetic — no external services. `#[ignore]`'d. Each test uses its own
//! tempdir to stay safe under `--test-threads`. Run with
//! `cargo test -p covenantd --test live_http_memory_repair_apply -- --ignored live_`.

use covenant_identity::LocalIdentity;
use covenant_memory::{MemoryStore, SqliteStore};
use covenant_types::{
    MemoryRecord, MemoryRepairCommand, MemoryRepairMode, MemoryRepairRequest, MemoryTier,
};
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

/// Pre-create the operator identity and one operator-owned working-tier record
/// under `home`, returning its id. The identity is created first so the
/// record's owner matches the operator the daemon loads — the repair handler's
/// owner-visibility check (`record.owner == peer`) would otherwise reject it.
async fn seed_operator_record(home: &Path) -> Uuid {
    let identity_path = home.join("identity").join("local.key");
    let identity =
        LocalIdentity::load_or_create(&identity_path, "user@local").expect("create identity");
    let operator = identity.agent_id();

    let id = Uuid::new_v4();
    let store = SqliteStore::open(&home.join("memory.db")).expect("open memory store");
    store
        .put(MemoryRecord {
            id,
            tier: MemoryTier::Working,
            owner: operator,
            text: "repairable working memory".into(),
            embedding: Vec::new(),
            metadata: json!({}),
            created_at: 1,
            parent: None,
        })
        .await
        .expect("put memory record");
    drop(store);
    id
}

async fn record_exists(home: &Path, id: Uuid) -> bool {
    let store = SqliteStore::open(&home.join("memory.db")).expect("reopen memory store");
    store.get(id).await.expect("memory get").is_some()
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts POST /memory/repair apply deletes an operator-owned record over HTTP"]
async fn live_http_memory_repair_apply_round_trips_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let id = seed_operator_record(home.path()).await;
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    grant(&client, &base, "memory.repair.apply").await;

    let body = MemoryRepairRequest {
        mode: MemoryRepairMode::Apply,
        command: MemoryRepairCommand::DeleteRecord { id },
        reason: "http repair apply probe".into(),
    };
    let repaired: Value = client
        .post(format!("{base}/memory/repair"))
        .json(&body)
        .send()
        .await
        .expect("post memory repair apply")
        .json()
        .await
        .expect("repair body");
    assert_eq!(
        repaired["kind"], "memory_repaired",
        "granted apply must be accepted over HTTP: {repaired:?}",
    );
    assert_eq!(
        repaired["outcome"]["would_change"].as_bool(),
        Some(true),
        "deleting an existing record must be a change: {repaired:?}",
    );
    assert_eq!(
        repaired["outcome"]["changed"].as_bool(),
        Some(true),
        "an apply must report the change as committed: {repaired:?}",
    );

    drop(client);
    let _ = child.kill().await;
    let _ = child.wait().await;

    assert!(
        !record_exists(home.path(), id).await,
        "apply must delete the record from the store",
    );
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts a no-grant POST /memory/repair apply is rejected and leaves the record"]
async fn live_http_memory_repair_apply_requires_grant_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let id = seed_operator_record(home.path()).await;
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    // No grant. The operator authenticates but lacks memory.repair.apply, so
    // the apply must be rejected by name before the record is touched.
    let body = MemoryRepairRequest {
        mode: MemoryRepairMode::Apply,
        command: MemoryRepairCommand::DeleteRecord { id },
        reason: "http repair apply denial probe".into(),
    };
    let rejected: Value = client
        .post(format!("{base}/memory/repair"))
        .json(&body)
        .send()
        .await
        .expect("post memory repair apply without grant")
        .json()
        .await
        .expect("rejection body");
    assert_eq!(
        rejected["kind"], "error",
        "an ungranted apply must be rejected: {rejected:?}",
    );
    let message = rejected["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("memory.repair.apply"),
        "rejection must name the missing capability: {rejected:?}",
    );
    assert!(
        message.contains("requires capability"),
        "rejection must surface the requires-capability prefix: {rejected:?}",
    );

    drop(client);
    let _ = child.kill().await;
    let _ = child.wait().await;

    assert!(
        record_exists(home.path(), id).await,
        "a denied apply must leave the record in the store",
    );
}
