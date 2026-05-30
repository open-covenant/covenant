//! Live HTTP coverage for `POST /settlement/receipts/backfill`
//! (`settlement_backfill_receipts`).
//!
//! The settlement receipt backfill mutator is live-tested over IPC by
//! `live_settlement_backfill.rs` and over the CLI, but no test drives it
//! over the HTTP gateway, leaving the `SettlementBackfillBody`
//! deserialization, bearer-auth operator identity, the
//! `settlement.backfill.apply` scope gate, the rollback checkpoint, and
//! the atomic store rewrite unexercised on the HTTP path. This mirrors
//! that IPC test over HTTP, reusing its legacy-row seed.
//!
//! Two scenarios:
//! - apply round-trip: an operator holding `settlement.backfill.apply`
//!   POSTs an apply and receives `kind: "settlement_receipts_backfilled"`
//!   with `row_count=2` and a rollback path; the receipts store is
//!   rewritten and exactly one rollback checkpoint holds the original
//!   content.
//! - scope denial: an operator holding only `settlement.backfill.dry_run`
//!   POSTs an apply and is rejected with a body naming
//!   `settlement.backfill.apply`, leaving the store and the rollback set
//!   untouched.
//!
//! Hermetic — no external services. `#[ignore]`'d. Each test uses its own
//! tempdir to stay safe under `--test-threads`. Run with
//! `cargo test -p covenantd --test live_http_settlement_backfill -- --ignored live_`.

use covenant_types::{AgentId, ResourceKind, SettlementReceipt};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
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

/// Write `count` legacy receipt rows (optional onchain fields removed)
/// into `<home>/receipts/working.jsonl` and return the raw content. The
/// mutator's repair contract is re-introducing those missing keys via
/// serde defaults, so stripping them is what makes a row a candidate.
fn seed_legacy_rows(home: &Path, count: u64) -> String {
    let mut lines = String::new();
    for i in 0..count {
        let receipt = SettlementReceipt {
            id: Uuid::from_u128(0x5e7u128 + i as u128),
            payer: AgentId::new("user@local", [0u8; 32]),
            resource: ResourceKind::Memory,
            memory_record_id: None,
            credits_consumed: 7,
            settled_at: 7,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        };
        let mut value = serde_json::to_value(&receipt).unwrap();
        let obj = value.as_object_mut().unwrap();
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
        lines.push_str(&serde_json::to_string(&value).unwrap());
        lines.push('\n');
    }
    let receipts_dir = home.join("receipts");
    std::fs::create_dir_all(&receipts_dir).unwrap();
    std::fs::write(receipts_dir.join("working.jsonl"), &lines).unwrap();
    lines
}

fn rollback_files(home: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(home.join("receipts")) {
        Ok(entries) => entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().contains(".backfill-rollback-"))
                    .unwrap_or(false)
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts POST /settlement/receipts/backfill applies with a recoverable rollback over HTTP"]
async fn live_http_settlement_backfill_apply_round_trips_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let original = seed_legacy_rows(home.path(), 2);
    let client = operator_client(home.path()).await;

    grant(&client, &base, "settlement.backfill.apply").await;

    let posted: Value = client
        .post(format!("{base}/settlement/receipts/backfill"))
        .json(&json!({ "dry_run": false }))
        .send()
        .await
        .expect("post backfill apply")
        .json()
        .await
        .expect("backfill body");
    assert_eq!(
        posted["kind"], "settlement_receipts_backfilled",
        "granted apply must be accepted over HTTP: {posted:?}",
    );
    assert_eq!(
        posted["row_count"].as_u64(),
        Some(2),
        "both seeded legacy rows must be repaired: {posted:?}",
    );
    assert_eq!(
        posted["dry_run"], false,
        "an apply must report dry_run=false: {posted:?}",
    );
    let rollback = posted["rollback_path"]
        .as_str()
        .expect("apply must return a rollback path");

    let store_path = home.path().join("receipts").join("working.jsonl");
    assert_ne!(
        std::fs::read_to_string(&store_path).expect("read store"),
        original,
        "apply must rewrite the store",
    );
    assert_eq!(
        std::fs::read_to_string(rollback).expect("read rollback"),
        original,
        "rollback checkpoint must hold the original content",
    );
    let checkpoints = rollback_files(home.path());
    assert_eq!(
        checkpoints.len(),
        1,
        "apply must write exactly one rollback file; got {checkpoints:?}",
    );

    let _ = child.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts a dry_run-only grant cannot drive a settlement backfill apply over HTTP"]
async fn live_http_settlement_backfill_dry_run_scope_rejects_apply_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let original = seed_legacy_rows(home.path(), 1);
    let client = operator_client(home.path()).await;

    // Grant only the dry_run cap. An apply must then be rejected by name —
    // a generic auth failure or a missing-grant arm would give false
    // assurance about scope discipline on the HTTP path.
    grant(&client, &base, "settlement.backfill.dry_run").await;

    let rejected: Value = client
        .post(format!("{base}/settlement/receipts/backfill"))
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
        message.contains("settlement.backfill.apply"),
        "rejection must name the missing apply cap: {rejected:?}",
    );
    assert!(
        message.contains("requires capability"),
        "rejection must surface the requires-capability prefix: {rejected:?}",
    );

    let store_path = home.path().join("receipts").join("working.jsonl");
    assert_eq!(
        std::fs::read_to_string(&store_path).expect("read store"),
        original,
        "a denied apply must not touch the store",
    );
    assert!(
        rollback_files(home.path()).is_empty(),
        "a denied apply must not write a rollback checkpoint",
    );

    let _ = child.kill().await;
}
