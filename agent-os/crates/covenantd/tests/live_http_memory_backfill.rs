//! Live HTTP coverage for `POST /memory/records/backfill`
//! (`memory_backfill_records`).
//!
//! The memory-record receipt-backfill mutator is live-tested over IPC by
//! `live_memory_backfill.rs`, but no test drives it over the HTTP
//! gateway, leaving the `MemoryBackfillBody` deserialization, bearer-auth
//! operator identity, the `memory.backfill.apply` scope gate, the SQLite
//! SAVEPOINT mutation, and the audit row unexercised on the HTTP path.
//! This mirrors that IPC test over HTTP, reusing its pre-spawn seed of a
//! legacy memory record and a correlatable receipt.
//!
//! Two scenarios:
//! - apply round-trip: an operator holding `memory.backfill.apply` POSTs
//!   an apply and receives `kind: "memory_records_backfilled"` with
//!   `row_count=1` and the canonical SAVEPOINT name; after the daemon
//!   exits, the on-disk record carries `metadata.receipt_id` merged in
//!   while its prior metadata keys survive.
//! - scope denial: an operator holding only `memory.backfill.dry_run`
//!   POSTs an apply and is rejected with a body naming
//!   `memory.backfill.apply`, and the record's metadata stays untouched.
//!
//! Hermetic — no external services. `#[ignore]`'d. Each test uses its own
//! tempdir to stay safe under `--test-threads`. Run with
//! `cargo test -p covenantd --test live_http_memory_backfill -- --ignored live_`.

use covenant_identity::LocalIdentity;
use covenant_memory::{MemoryStore, SqliteStore, MEMORY_BACKFILL_SAVEPOINT_NAME};
use covenant_types::{MemoryRecord, MemoryTier, ResourceKind, SettlementReceipt};
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

/// Spawn covenantd on a free HTTP port against `home`, waiting for both
/// its unix socket and the HTTP gateway to come up. Returns the child and
/// the gateway base URL.
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

/// Pre-create the operator identity, one legacy memory receipt (no
/// `memory_record_id`), and one matching uncorrelated memory record (same
/// owner as the receipt's payer) under `home`, returning the record id.
///
/// All writes land before the daemon exists because the daemon takes
/// ownership of the SQLite store on spawn. The identity is created first
/// so the record's owner pubkey matches the operator the daemon loads —
/// the backfill handler's `owner == peer.pubkey` server-side filter would
/// otherwise drop the record before the planner sees it.
async fn seed_legacy_memory_and_receipt(home: &Path) -> Uuid {
    let identity_path = home.join("identity").join("local.key");
    let identity =
        LocalIdentity::load_or_create(&identity_path, "user@local").expect("create identity");
    let operator = identity.agent_id();

    let memory_id = Uuid::new_v4();
    let store = SqliteStore::open(&home.join("memory.db")).expect("open memory store");
    store
        .put(MemoryRecord {
            id: memory_id,
            tier: MemoryTier::Working,
            owner: operator.clone(),
            text: "legacy memory awaiting receipt".into(),
            embedding: Vec::new(),
            metadata: json!({ "note": "preserved on merge" }),
            created_at: 1,
            parent: None,
        })
        .await
        .expect("put memory record");
    drop(store);

    let receipt = SettlementReceipt {
        id: Uuid::new_v4(),
        payer: operator,
        resource: ResourceKind::Memory,
        memory_record_id: None,
        credits_consumed: 1,
        settled_at: 2,
        chain: None,
        cluster: None,
        batch_id: None,
        merkle_root: None,
        tx_sig: None,
        slot: None,
        confirmed_at: None,
        onchain_sig: None,
    };
    // Strip the optional chain fields so the row reads as a legacy
    // "no memory_record_id" receipt through serde's #[serde(default)].
    let mut value = serde_json::to_value(&receipt).expect("serialize receipt");
    let obj = value.as_object_mut().expect("receipt is a JSON object");
    for key in [
        "chain",
        "cluster",
        "batch_id",
        "merkle_root",
        "tx_sig",
        "slot",
        "confirmed_at",
        "onchain_sig",
    ] {
        obj.remove(key);
    }
    let line = format!("{}\n", serde_json::to_string(&value).expect("serialize"));
    let receipts_dir = home.join("receipts");
    std::fs::create_dir_all(&receipts_dir).expect("create receipts dir");
    std::fs::write(receipts_dir.join("working.jsonl"), &line).expect("write receipts");

    memory_id
}

async fn read_record_metadata(home: &Path, id: Uuid) -> Value {
    let store = SqliteStore::open(&home.join("memory.db")).expect("reopen memory store");
    store
        .get(id)
        .await
        .expect("memory get")
        .expect("record exists")
        .metadata
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts POST /memory/records/backfill applies and merges receipt_id over HTTP"]
async fn live_http_memory_backfill_apply_round_trips_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let memory_id = seed_legacy_memory_and_receipt(home.path()).await;
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    grant(&client, &base, "memory.backfill.apply").await;

    let posted: Value = client
        .post(format!("{base}/memory/records/backfill"))
        .json(&json!({ "dry_run": false }))
        .send()
        .await
        .expect("post backfill apply")
        .json()
        .await
        .expect("backfill body");
    assert_eq!(
        posted["kind"], "memory_records_backfilled",
        "granted apply must be accepted over HTTP: {posted:?}",
    );
    assert_eq!(
        posted["row_count"].as_u64(),
        Some(1),
        "the one seeded legacy row must be repaired: {posted:?}",
    );
    assert_eq!(
        posted["dry_run"], false,
        "an apply must report dry_run=false: {posted:?}",
    );
    assert_eq!(
        posted["savepoint_name"].as_str(),
        Some(MEMORY_BACKFILL_SAVEPOINT_NAME),
        "response must carry the canonical SAVEPOINT name: {posted:?}",
    );

    drop(client);
    let _ = child.kill().await;
    let _ = child.wait().await;

    let metadata = read_record_metadata(home.path(), memory_id).await;
    assert!(
        metadata.get("receipt_id").and_then(Value::as_str).is_some(),
        "apply must merge receipt_id into the record metadata: {metadata:?}",
    );
    assert_eq!(
        metadata.get("note").and_then(Value::as_str),
        Some("preserved on merge"),
        "apply must preserve pre-existing metadata keys: {metadata:?}",
    );
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts a dry_run-only grant cannot drive a memory backfill apply over HTTP"]
async fn live_http_memory_backfill_dry_run_scope_rejects_apply_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let memory_id = seed_legacy_memory_and_receipt(home.path()).await;
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    // Grant only the dry_run cap. An apply must then be rejected by name —
    // a generic auth failure or a missing-grant arm would give false
    // assurance about scope discipline on the HTTP path.
    grant(&client, &base, "memory.backfill.dry_run").await;

    let rejected: Value = client
        .post(format!("{base}/memory/records/backfill"))
        .json(&json!({ "dry_run": false }))
        .send()
        .await
        .expect("post backfill apply under dry_run grant")
        .json()
        .await
        .expect("rejection body");
    assert_eq!(
        rejected["kind"], "error",
        "a dry_run-only grant must not authorize an apply: {rejected:?}",
    );
    let message = rejected["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("memory.backfill.apply"),
        "rejection must name the missing apply cap: {rejected:?}",
    );
    assert!(
        message.contains("requires capability"),
        "rejection must surface the requires-capability prefix: {rejected:?}",
    );

    drop(client);
    let _ = child.kill().await;
    let _ = child.wait().await;

    let metadata = read_record_metadata(home.path(), memory_id).await;
    assert!(
        metadata.get("receipt_id").is_none(),
        "a denied apply must not touch the store: {metadata:?}",
    );
}
