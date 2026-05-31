//! Live CLI coverage for `covenant memory backfill-receipt-correlation --json`.
//!
//! The daemon + IPC path is exercised by `live_memory_backfill.rs`; this file
//! drives the same `Request::BackfillMemoryRecords` through the real `covenant`
//! binary so a regression in CLI argument parsing, the
//! `covenant.memory.backfill.v1` JSON envelope, dry-run safety, or
//! capability-grant wiring surfaces as a CLI failure rather than only a
//! daemon-level one.
//!
//! Three scenarios:
//! - dry_run_then_apply: one legacy memory receipt (resource=Memory,
//!   `memory_record_id=None`) and one uncorrelated working-tier memory record
//!   sharing the receipt's payer are pre-seeded with deterministic ids. The
//!   operator grants `memory.backfill.dry_run`, `memory.backfill.apply`, and
//!   `memory.read`. A `--dry-run --json` run reports `row_count=1`,
//!   `dry_run=true`, the canonical savepoint, and leaves the record without a
//!   `receipt_id` (confirmed over `memory recent --json`). A second run without
//!   `--dry-run` reports `dry_run=false`, the daemon-visible metadata now
//!   carries `receipt_id` equal to the seeded receipt id with the pre-existing
//!   `note` key preserved, the `memory_record_backfill_applied` audit row
//!   surfaces on the operator feed for both the dry run and the apply, and the
//!   on-disk SQLite store reflects the merge once the daemon is stopped.
//! - requires_apply_capability: with no capability granted, an apply run is
//!   rejected (non-zero exit, stderr naming `memory.backfill.apply`) and the
//!   on-disk record metadata is left untouched.
//! - rejects_unsupported_flags: against a running daemon, an unknown flag is
//!   rejected by CLI argument parsing and the reserved `--scope-pubkey` is
//!   rejected by the daemon as not-yet-supported; both fail with a non-zero
//!   exit. (The CLI connects and authenticates before parsing the subcommand,
//!   so a daemon must be present even for the parse-level rejection.)
//!
//! Hermetic — no external services. `#[ignore]`'d so they only run under
//! `--ignored live_`. Each test uses its own tempdir to stay parallel-safe.

use covenant_identity::LocalIdentity;
use covenant_memory::{MemoryStore, SqliteStore, MEMORY_BACKFILL_SAVEPOINT_NAME};
use covenant_types::{MemoryRecord, MemoryTier, ResourceKind, SettlementReceipt};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::time::sleep;
use uuid::Uuid;

const SEED_MEMORY_ID: u128 = 0x00c0_ffee;
const SEED_RECEIPT_ID: u128 = 0x0bad_beef;

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

async fn spawn_daemon(home: &Path) -> Child {
    let port = pick_free_port();
    let daemon_exe = env!("CARGO_BIN_EXE_covenantd");
    let child = Command::new(daemon_exe)
        .env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");
    let sock = home.join("sock");
    if !wait_for_sock(&sock).await {
        panic!("daemon never created its socket at {}", sock.display());
    }
    wait_for_operator_token(home).await;
    child
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

async fn run_cli_output(cli_exe: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(cli_exe)
        .args(args)
        .env("COVENANT_HOME", home)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI")
}

/// Pre-create the operator identity, one uncorrelated working-tier memory
/// record, and one legacy memory settlement receipt (resource=Memory,
/// `memory_record_id=None`) whose payer matches the record owner, all under
/// `home` with deterministic ids. The daemon owns the SQLite store on spawn,
/// so every write lands before the daemon exists, and the identity has to land
/// first so the record owner matches the operator the daemon loads — otherwise
/// the backfill's `owner == payer` correlation drops the record. Returns
/// `(memory_id, receipt_id)`; an apply merges `receipt_id` into the record.
async fn seed_legacy_memory_and_receipt(home: &Path) -> (Uuid, Uuid) {
    let identity_path = home.join("identity").join("local.key");
    let identity =
        LocalIdentity::load_or_create(&identity_path, "user@local").expect("create identity");
    let operator = identity.agent_id();

    let memory_id = Uuid::from_u128(SEED_MEMORY_ID);
    let store = SqliteStore::open(&home.join("memory.db")).expect("open memory store");
    store
        .put(MemoryRecord {
            id: memory_id,
            tier: MemoryTier::Working,
            owner: operator.clone(),
            text: "legacy memory awaiting receipt".into(),
            embedding: Vec::new(),
            metadata: serde_json::json!({ "note": "preserved on merge" }),
            created_at: 1,
            parent: None,
        })
        .await
        .expect("put memory record");
    drop(store);

    let receipt_id = Uuid::from_u128(SEED_RECEIPT_ID);
    let receipt = SettlementReceipt {
        id: receipt_id,
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
    // Mirror the legacy on-wire shape the backfill is meant to detect: strip
    // every optional chain field so the row decodes through serde defaults.
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

    (memory_id, receipt_id)
}

async fn on_disk_metadata(home: &Path, id: Uuid) -> Value {
    let store = SqliteStore::open(&home.join("memory.db")).expect("reopen memory store");
    store
        .get(id)
        .await
        .expect("memory get")
        .expect("record exists")
        .metadata
}

fn daemon_metadata(read_json: &Value, id: Uuid) -> Value {
    let target = id.to_string();
    read_json["records"]
        .as_array()
        .expect("memory recent --json must carry a records array")
        .iter()
        .find(|record| record["id"].as_str() == Some(target.as_str()))
        .unwrap_or_else(|| panic!("memory record {target} missing from feed: {read_json:?}"))
        ["metadata"]
        .clone()
}

fn assert_backfill_envelope(value: &Value, row_count: u64, dry_run: bool) {
    assert_eq!(
        value.get("schema").and_then(Value::as_str),
        Some("covenant.memory.backfill.v1"),
        "schema must be pinned: {value:?}",
    );
    assert_eq!(
        value.get("row_count").and_then(Value::as_u64),
        Some(row_count),
        "row_count must encode as a JSON unsigned integer: {value:?}",
    );
    assert_eq!(
        value.get("dry_run").and_then(Value::as_bool),
        Some(dry_run),
        "dry_run must encode as a JSON bool: {value:?}",
    );
    assert_eq!(
        value.get("savepoint_name").and_then(Value::as_str),
        Some(MEMORY_BACKFILL_SAVEPOINT_NAME),
        "savepoint_name must be the canonical constant: {value:?}",
    );
}

async fn memory_recent(cli: &Path, home: &Path) -> Value {
    let stdout = run_cli(
        cli,
        home,
        &[
            "memory", "recent", "--tier", "working", "--limit", "10", "--json",
        ],
    )
    .await;
    serde_json::from_str(stdout.trim()).expect("memory recent --json must be valid JSON")
}

#[tokio::test]
#[ignore = "live: spawns covenantd + runs `covenant memory backfill-receipt-correlation --json` subprocesses"]
async fn live_cli_memory_backfill_receipt_correlation_json_dry_run_then_apply() {
    let home = tempfile::tempdir().expect("tempdir");
    let (memory_id, receipt_id) = seed_legacy_memory_and_receipt(home.path()).await;
    let mut child = spawn_daemon(home.path()).await;
    let cli = covenant_cli_bin();

    for action in [
        "memory.backfill.dry_run",
        "memory.backfill.apply",
        "memory.read",
    ] {
        run_cli(&cli, home.path(), &["capabilities", "grant", action]).await;
    }

    let dry_stdout = run_cli(
        &cli,
        home.path(),
        &[
            "memory",
            "backfill-receipt-correlation",
            "--dry-run",
            "--json",
        ],
    )
    .await;
    let dry: Value = serde_json::from_str(dry_stdout.trim())
        .expect("backfill-receipt-correlation --dry-run --json must be valid JSON");
    assert_backfill_envelope(&dry, 1, true);

    let recent = memory_recent(&cli, home.path()).await;
    assert!(
        daemon_metadata(&recent, memory_id)
            .get("receipt_id")
            .is_none(),
        "dry-run must not correlate the record: {recent:?}",
    );

    let apply_stdout = run_cli(
        &cli,
        home.path(),
        &["memory", "backfill-receipt-correlation", "--json"],
    )
    .await;
    let apply: Value = serde_json::from_str(apply_stdout.trim())
        .expect("backfill-receipt-correlation --json must be valid JSON");
    assert_backfill_envelope(&apply, 1, false);

    let recent = memory_recent(&cli, home.path()).await;
    let metadata = daemon_metadata(&recent, memory_id);
    assert_eq!(
        metadata.get("receipt_id").and_then(Value::as_str),
        Some(receipt_id.to_string().as_str()),
        "apply must merge the seeded receipt id into the record: {metadata:?}",
    );
    assert_eq!(
        metadata.get("note").and_then(Value::as_str),
        Some("preserved on merge"),
        "apply must preserve pre-existing metadata keys: {metadata:?}",
    );

    let audit_stdout = run_cli(
        &cli,
        home.path(),
        &["audit", "recent", "--limit", "50", "--json"],
    )
    .await;
    let audit: Value =
        serde_json::from_str(audit_stdout.trim()).expect("audit recent --json must be valid JSON");
    let backfill_rows: Vec<&Value> = audit["events"]
        .as_array()
        .expect("audit recent --json must carry an events array")
        .iter()
        .filter(|event| event["kind"]["type"].as_str() == Some("memory_record_backfill_applied"))
        .collect();
    assert!(
        backfill_rows.iter().any(|event| {
            event["kind"]["dry_run"].as_bool() == Some(false)
                && event["kind"]["row_count"].as_u64() == Some(1)
                && event["kind"]["savepoint_name"].as_str() == Some(MEMORY_BACKFILL_SAVEPOINT_NAME)
        }),
        "apply must surface a memory_record_backfill_applied row on the operator feed: {audit:?}",
    );
    assert!(
        backfill_rows
            .iter()
            .any(|event| event["kind"]["dry_run"].as_bool() == Some(true)),
        "the dry run must also be audited: {audit:?}",
    );

    // Stop the daemon so the SQLite write-ahead is flushed and re-opening the
    // store from the test does not race the daemon's connection.
    let _ = child.kill().await;
    let _ = child.wait().await;

    let durable = on_disk_metadata(home.path(), memory_id).await;
    assert_eq!(
        durable.get("receipt_id").and_then(Value::as_str),
        Some(receipt_id.to_string().as_str()),
        "apply must persist the merged receipt id to disk: {durable:?}",
    );
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts an ungranted memory backfill apply is denied"]
async fn live_cli_memory_backfill_receipt_correlation_json_requires_apply_capability() {
    let home = tempfile::tempdir().expect("tempdir");
    let (memory_id, _receipt_id) = seed_legacy_memory_and_receipt(home.path()).await;
    let mut child = spawn_daemon(home.path()).await;
    let cli = covenant_cli_bin();

    let output = run_cli_output(
        &cli,
        home.path(),
        &["memory", "backfill-receipt-correlation", "--json"],
    )
    .await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "an ungranted apply must fail: status={:?} stderr={stderr:?}",
        output.status,
    );
    assert!(
        stderr.contains("memory.backfill.apply"),
        "rejection must name the missing apply capability: {stderr:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;

    let metadata = on_disk_metadata(home.path(), memory_id).await;
    assert!(
        metadata.get("receipt_id").is_none(),
        "a denied apply must not mutate the store: {metadata:?}",
    );
}

#[tokio::test]
#[ignore = "live: spawns covenantd + runs `covenant memory backfill-receipt-correlation` with unsupported flags"]
async fn live_cli_memory_backfill_receipt_correlation_json_rejects_unsupported_flags() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let cli = covenant_cli_bin();

    let unknown = run_cli_output(
        &cli,
        home.path(),
        &["memory", "backfill-receipt-correlation", "--bogus"],
    )
    .await;
    let unknown_stderr = String::from_utf8_lossy(&unknown.stderr);
    assert!(
        !unknown.status.success(),
        "an unknown flag must fail: status={:?} stderr={unknown_stderr:?}",
        unknown.status,
    );
    assert!(
        unknown_stderr.contains("unknown flag") && unknown_stderr.contains("--bogus"),
        "the unknown-flag rejection must name the flag: {unknown_stderr:?}",
    );

    let reserved = run_cli_output(
        &cli,
        home.path(),
        &[
            "memory",
            "backfill-receipt-correlation",
            "--scope-pubkey",
            "deadbeef",
        ],
    )
    .await;
    let reserved_stderr = String::from_utf8_lossy(&reserved.stderr);
    assert!(
        !reserved.status.success(),
        "reserved --scope-pubkey must fail: status={:?} stderr={reserved_stderr:?}",
        reserved.status,
    );
    assert!(
        reserved_stderr.contains("not yet supported"),
        "the --scope-pubkey rejection must report it is not yet supported: {reserved_stderr:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
