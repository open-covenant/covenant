//! Live CLI coverage for `covenant settlement backfill-receipts --json`.
//!
//! Closes the last `deferred` row in
//! `agent-os/autonomy/privileged-cli-live-matrix.json`. The dispatch
//! path and capability gate are already exercised end-to-end by
//! `live_settlement_backfill.rs` over raw IPC; this file drives the
//! same path through the real `covenant` binary so a regression in CLI
//! argument parsing, JSON envelope shape, or capability-grant wiring
//! surfaces as a CLI test failure rather than only a daemon-level one.
//!
//! Two scenarios:
//! - happy_path: operator grants `settlement.backfill.apply`, two
//!   legacy rows are seeded into `<home>/receipts/working.jsonl`, the
//!   verb runs without `--dry-run`, and the JSON envelope reports
//!   `row_count=2`, `dry_run=false`, and a `rollback_path` whose file
//!   content equals the original seeded text. The store file content
//!   must differ from the original.
//! - dry_run: operator grants `settlement.backfill.dry_run`, no
//!   legacy rows exist, the verb runs with `--dry-run`, and the JSON
//!   envelope reports `row_count=0`, `dry_run=true`, `rollback_path`
//!   null.
//!
//! Hermetic — no external services. `#[ignore]`'d so they only run
//! under `--ignored live_`.

use covenant_types::{AgentId, ResourceKind, SettlementReceipt};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;
use uuid::Uuid;

fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().unwrap().port()
}

fn covenant_cli_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/covenant")
        .canonicalize()
        .expect("covenant CLI binary not built; run `cargo build -p covenant` first")
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

async fn wait_for_operator_token(home: &Path) {
    let path = home.join("peers").join("operator.token");
    for _ in 0..50 {
        if let Ok(s) = std::fs::read_to_string(&path) {
            if !s.trim().is_empty() {
                return;
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("operator token never appeared at {}", path.display());
}

async fn run_cli(cli_exe: &Path, home: &Path, args: &[&str]) -> String {
    let output = Command::new(cli_exe)
        .args(args)
        .env("COVENANT_HOME", home)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "CLI failed for {args:?}: status={:?} stdout={stdout:?} stderr={stderr:?}",
        output.status
    );
    assert!(
        stderr.trim().is_empty(),
        "CLI command {args:?} must not emit stderr on success: {stderr:?}"
    );
    stdout
}

/// Write `count` legacy receipt rows (optional onchain fields removed)
/// into `<home>/receipts/working.jsonl` and return the raw content.
/// The backfill mutator's repair contract is re-introducing those
/// missing keys via serde-decodable defaults, so removing them is what
/// makes a row a backfill candidate. Mirrors the helper in
/// `live_settlement_backfill.rs` but is inlined here to keep the test
/// surface flat.
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

#[tokio::test]
#[ignore = "live: spawns covenantd + runs `covenant settlement backfill-receipts --json` subprocesses"]
async fn live_cli_settlement_backfill_receipts_json_happy_path() {
    let home = tempfile::tempdir().expect("tempdir");

    let port = pick_free_port();
    let daemon_exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(daemon_exe)
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
    wait_for_operator_token(home.path()).await;

    let original = seed_legacy_rows(home.path(), 2);

    let cli_exe = covenant_cli_bin();
    run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "settlement.backfill.apply"],
    )
    .await;

    let stdout = run_cli(
        &cli_exe,
        home.path(),
        &["settlement", "backfill-receipts", "--json"],
    )
    .await;

    let value: Value = serde_json::from_str(stdout.trim())
        .expect("settlement backfill-receipts --json must be valid JSON");
    assert_eq!(
        value.get("schema").and_then(Value::as_str),
        Some("covenant.settlement.backfill.v1"),
    );
    assert_eq!(
        value
            .get("row_count")
            .and_then(Value::as_u64)
            .expect("row_count must encode as JSON unsigned integer"),
        2,
        "both seeded legacy rows must be repaired: {value:?}",
    );
    assert!(
        !value
            .get("dry_run")
            .and_then(Value::as_bool)
            .expect("dry_run must encode as JSON bool"),
        "apply must report dry_run=false: {value:?}",
    );
    let rollback_path = value
        .get("rollback_path")
        .and_then(Value::as_str)
        .expect("apply must return a rollback_path string");
    assert!(
        !rollback_path.is_empty(),
        "rollback_path must be non-empty: {value:?}",
    );
    let rollback_pathbuf = PathBuf::from(rollback_path);
    assert!(
        rollback_pathbuf.starts_with(home.path().join("receipts")),
        "rollback_path must live under <home>/receipts/: {rollback_path}",
    );

    let rollback_content = std::fs::read_to_string(&rollback_pathbuf).expect("read rollback file");
    assert_eq!(
        rollback_content, original,
        "rollback checkpoint must hold the original seeded content",
    );

    let store_path = home.path().join("receipts").join("working.jsonl");
    let after = std::fs::read_to_string(&store_path).expect("read store");
    assert_ne!(after, original, "apply must rewrite the receipts store",);

    let _ = child.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + runs `covenant settlement backfill-receipts --dry-run --json` subprocesses"]
async fn live_cli_settlement_backfill_receipts_json_dry_run() {
    let home = tempfile::tempdir().expect("tempdir");

    let port = pick_free_port();
    let daemon_exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(daemon_exe)
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
    wait_for_operator_token(home.path()).await;

    let cli_exe = covenant_cli_bin();
    run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "settlement.backfill.dry_run"],
    )
    .await;

    let stdout = run_cli(
        &cli_exe,
        home.path(),
        &["settlement", "backfill-receipts", "--dry-run", "--json"],
    )
    .await;

    let value: Value = serde_json::from_str(stdout.trim())
        .expect("settlement backfill-receipts --dry-run --json must be valid JSON");
    assert_eq!(
        value.get("schema").and_then(Value::as_str),
        Some("covenant.settlement.backfill.v1"),
    );
    assert_eq!(
        value
            .get("row_count")
            .and_then(Value::as_u64)
            .expect("row_count must encode as JSON unsigned integer"),
        0,
        "no legacy rows seeded; dry-run must report row_count=0: {value:?}",
    );
    assert!(
        value
            .get("dry_run")
            .and_then(Value::as_bool)
            .expect("dry_run must encode as JSON bool"),
        "dry-run must report dry_run=true: {value:?}",
    );
    assert!(
        value
            .get("rollback_path")
            .map(Value::is_null)
            .unwrap_or(false),
        "dry-run must not produce a rollback_path: {value:?}",
    );

    let _ = child.kill().await;
}
