//! Live HTTP coverage for the apply path of `POST /memory/compact`
//! (`memory_compact`).
//!
//! The compaction mutator has CLI coverage (`live_cli_memory_compaction.rs`),
//! an in-memory gateway round-trip, and HTTP delegated-denial coverage, but no
//! test drives the apply allowance over a spawned daemon — leaving the
//! `memory.compact.<mode>` gate, the operator-identity guard, and the actual
//! record deletion unexercised on the HTTP path.
//!
//! Two scenarios:
//! - apply round-trip: an operator holding `memory.compact.apply` POSTs an
//!   `Apply` compaction whose policy deletes working-tier records older than a
//!   cutoff and receives `kind: "memory_compacted"` with `outcome.changed` and
//!   `outcome.would_change` true and the seeded record id in `outcome.deleted`;
//!   after the daemon exits the record is gone from the store.
//! - scope denial: the operator without the grant is rejected by name
//!   (`memory.compact.apply`) and the record survives untouched.
//!
//! Hermetic — no external services. `#[ignore]`'d. Each test uses its own
//! tempdir to stay safe under `--test-threads`. Run with
//! `cargo test -p covenantd --test live_http_memory_compact_apply -- --ignored live_`.

use covenant_identity::LocalIdentity;
use covenant_memory::{MemoryStore, SqliteStore};
use covenant_types::{
    MemoryCompactionPolicy, MemoryCompactionRequest, MemoryRecord, MemoryRepairMode, MemoryTier,
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
/// under `home`, returning its id. The identity is created first so the daemon
/// loads the same operator the bearer token authenticates as, which the
/// compaction handler's operator-identity guard requires. `created_at` is 1 so
/// a cutoff of 2 selects it for deletion.
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
            text: "compactable working memory".into(),
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

fn compact_apply_request() -> MemoryCompactionRequest {
    MemoryCompactionRequest {
        mode: MemoryRepairMode::Apply,
        policy: MemoryCompactionPolicy {
            delete_working_before_ms: Some(2),
            ..Default::default()
        },
        reason: "http compaction apply probe".into(),
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts POST /memory/compact apply deletes a working-tier record over HTTP"]
async fn live_http_memory_compact_apply_round_trips_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let id = seed_operator_record(home.path()).await;
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    grant(&client, &base, "memory.compact.apply").await;

    let compacted: Value = client
        .post(format!("{base}/memory/compact"))
        .json(&compact_apply_request())
        .send()
        .await
        .expect("post memory compact apply")
        .json()
        .await
        .expect("compact body");
    assert_eq!(
        compacted["kind"], "memory_compacted",
        "granted apply must be accepted over HTTP: {compacted:?}",
    );
    assert_eq!(
        compacted["outcome"]["would_change"].as_bool(),
        Some(true),
        "deleting a working record before the cutoff must be a change: {compacted:?}",
    );
    assert_eq!(
        compacted["outcome"]["changed"].as_bool(),
        Some(true),
        "an apply must report the change as committed: {compacted:?}",
    );
    let deleted = compacted["outcome"]["deleted"]
        .as_array()
        .expect("outcome.deleted must be an array");
    assert!(
        deleted
            .iter()
            .any(|entry| entry.as_str() == Some(id.to_string().as_str())),
        "the seeded record id must appear in outcome.deleted: {compacted:?}",
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
#[ignore = "live: spawns covenantd + asserts a no-grant POST /memory/compact apply is rejected and leaves the record"]
async fn live_http_memory_compact_apply_requires_grant_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let id = seed_operator_record(home.path()).await;
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    // No grant. The operator authenticates but lacks memory.compact.apply, so
    // the apply must be rejected by name before any record is touched.
    let rejected: Value = client
        .post(format!("{base}/memory/compact"))
        .json(&compact_apply_request())
        .send()
        .await
        .expect("post memory compact apply without grant")
        .json()
        .await
        .expect("rejection body");
    assert_eq!(
        rejected["kind"], "error",
        "an ungranted apply must be rejected: {rejected:?}",
    );
    let message = rejected["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("memory.compact.apply"),
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
