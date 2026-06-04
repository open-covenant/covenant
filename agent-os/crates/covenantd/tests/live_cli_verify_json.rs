//! Live CLI coverage for `covenant verify --json`.
//!
//! Spawns a real daemon against a temp home, runs the state verifier
//! through the CLI, and asserts stdout is one stable JSON object. Opt-in
//! because it crosses process and socket boundaries. Run from `agent-os/`
//! after `cargo build -p covenant`.

use covenant_audit::{AuditEvent, AuditKind};
use covenant_memory::{MemoryStore, SqliteStore};
use covenant_types::{AgentId, MemoryRecord, MemoryTier, ResourceKind, SettlementReceipt};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
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

async fn wait_for_sock(path: &std::path::Path) -> bool {
    for _ in 0..100 {
        if UnixStream::connect(path).await.is_ok() {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn wait_for_operator_token(home: &std::path::Path) {
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

async fn spawn_daemon(home: &std::path::Path, port: u16) -> tokio::process::Child {
    let daemon_exe = env!("CARGO_BIN_EXE_covenantd");
    Command::new(daemon_exe)
        .env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd")
}

async fn wait_for_daemon(home: &std::path::Path, child: &mut tokio::process::Child) {
    let sock = home.join("sock");
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon never created its socket at {}", sock.display());
    }
    wait_for_operator_token(home).await;
}

async fn run_cli_raw(
    cli_exe: &std::path::Path,
    home: &std::path::Path,
    args: &[&str],
) -> std::process::Output {
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

async fn run_cli(cli_exe: &std::path::Path, home: &std::path::Path, args: &[&str]) -> String {
    let output = run_cli_raw(cli_exe, home, args).await;
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

fn stale_parent_drift_for(value: &Value, memory_id: Uuid) -> Option<&Value> {
    value["drift"].as_array()?.iter().find(|item| {
        item["kind"].as_str() == Some("memory_stale_parent")
            && item["id"].as_str() == Some(&memory_id.to_string())
    })
}

#[tokio::test]
#[ignore = "live: spawns covenantd + runs `covenant verify --json` subprocess"]
async fn live_cli_verify_json_round_trip() {
    let home = tempfile::tempdir().expect("tempdir");

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;

    let cli_exe = covenant_cli_bin();
    let output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "CLI failed: status={:?} stdout={stdout:?} stderr={stderr:?}",
        output.status
    );
    assert!(
        stderr.trim().is_empty(),
        "verify --json must not emit stderr on success: {stderr:?}"
    );

    let value: Value =
        serde_json::from_str(stdout.trim()).expect("verify --json must be valid JSON");
    assert_eq!(value["kind"].as_str(), Some("verify_report"));
    assert_eq!(value["window"].as_u64(), Some(25));
    assert_eq!(value["orphans_total"].as_u64(), Some(0));
    assert!(
        value["checks"]
            .as_array()
            .expect("checks array")
            .iter()
            .all(|check| check["passed"].as_bool() == Some(true)),
        "fresh daemon should have only passing checks: {value:?}"
    );
    assert!(value["drift"].as_array().expect("drift array").is_empty());

    let _ = child.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, mutates state out of band, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_drift_after_audit_loss() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;

    run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.write"],
    )
    .await;
    run_cli(&cli_exe, home.path(), &["intent", "verify drift fixture"]).await;

    let _ = child.kill().await;
    let audit_events = home.path().join("audit").join("events.jsonl");
    std::fs::write(&audit_events, "").expect("clear audit events");

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        !output.status.success(),
        "verify --json should exit non-zero when drift exists: status={:?} stdout={stdout:?} stderr={stderr:?}",
        output.status
    );
    assert!(
        stderr.trim().is_empty(),
        "verify --json should report drift on stdout without stderr noise: {stderr:?}"
    );

    let value: Value =
        serde_json::from_str(stdout.trim()).expect("verify --json drift stdout must be JSON");
    assert_eq!(value["kind"].as_str(), Some("verify_report"));
    assert!(
        value["orphans_total"]
            .as_u64()
            .is_some_and(|count| count > 0),
        "drift report should count orphaned state: {value:?}"
    );

    let drift = value["drift"].as_array().expect("drift array");
    let memory_without_audit = drift.iter().find(|item| {
        item["kind"].as_str() == Some("memory_without_audit")
            && item["message"]
                .as_str()
                .is_some_and(|message| message.contains("IntentDispatched audit row"))
    });
    let item = memory_without_audit
        .unwrap_or_else(|| panic!("expected memory_without_audit drift row: {value:?}"));
    assert!(
        item["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("explicit repair command")),
        "drift row should include actionable repair guidance: {item:?}"
    );

    assert!(
        value["checks"]
            .as_array()
            .expect("checks array")
            .iter()
            .any(|check| check["name"].as_str() == Some("memory ↔ audit")
                && check["passed"].as_bool() == Some(false)),
        "memory/audit check should fail when audit evidence is removed: {value:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, purges memory, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_drift_after_memory_purge() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;

    for action in ["memory.write", "memory.purge"] {
        let output = run_cli_raw(
            &cli_exe,
            home.path(),
            &["capabilities", "grant", action, "--json"],
        )
        .await;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert!(
            output.status.success(),
            "capabilities grant failed: action={action} status={:?} stdout={stdout:?} stderr={stderr:?}",
            output.status
        );
        assert!(
            stderr.trim().is_empty(),
            "capabilities grant must not emit stderr on success: {stderr:?}"
        );
        let value: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!("capabilities grant --json must be valid JSON: {e}: {stdout:?}")
        });
        assert_eq!(value["kind"].as_str(), Some("capability_granted"));
        assert_eq!(value["action"].as_str(), Some(action));
    }

    let intent_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["intent", "--json", "drift fixture: baseline"],
    )
    .await;
    let intent_stdout = String::from_utf8_lossy(&intent_output.stdout).to_string();
    let intent_stderr = String::from_utf8_lossy(&intent_output.stderr).to_string();
    assert!(
        intent_output.status.success(),
        "intent failed: status={:?} stdout={intent_stdout:?} stderr={intent_stderr:?}",
        intent_output.status
    );
    assert!(
        intent_stderr.trim().is_empty(),
        "intent --json must not emit stderr on success: {intent_stderr:?}"
    );
    let intent_json: Value =
        serde_json::from_str(intent_stdout.trim()).expect("intent --json must be valid JSON");
    assert_eq!(intent_json["kind"].as_str(), Some("intent_result"));
    let intent_id = intent_json["intent_id"]
        .as_str()
        .expect("intent_result.intent_id must be a string");

    let purge_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["memory", "purge", "--before-ms", "9999999999999", "--json"],
    )
    .await;
    let purge_stdout = String::from_utf8_lossy(&purge_output.stdout).to_string();
    let purge_stderr = String::from_utf8_lossy(&purge_output.stderr).to_string();
    assert!(
        purge_output.status.success(),
        "memory purge failed: status={:?} stdout={purge_stdout:?} stderr={purge_stderr:?}",
        purge_output.status
    );
    assert!(
        purge_stderr.trim().is_empty(),
        "memory purge --json must not emit stderr on success: {purge_stderr:?}"
    );
    let purge_json: Value =
        serde_json::from_str(purge_stdout.trim()).expect("memory purge --json must be valid JSON");
    assert_eq!(purge_json["kind"].as_str(), Some("memory_purged"));

    let output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "100"],
    )
    .await;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        !output.status.success(),
        "verify --json should exit non-zero when drift exists: status={:?} stdout={stdout:?} stderr={stderr:?}",
        output.status
    );
    assert!(
        stderr.trim().is_empty(),
        "verify --json should report drift on stdout without stderr noise: {stderr:?}"
    );

    let value: Value =
        serde_json::from_str(stdout.trim()).expect("verify --json drift stdout must be JSON");
    assert_eq!(value["kind"].as_str(), Some("verify_report"));
    assert_eq!(value["window"].as_u64(), Some(100));
    assert!(
        value["orphans_total"].as_u64().unwrap_or(0) > 0,
        "memory purge drift should report orphans_total > 0: {value:?}"
    );

    let drift = value["drift"].as_array().expect("drift array");
    assert!(
        drift.iter().any(|item| {
            item["kind"].as_str() == Some("audit_without_memory")
                && item["id"].as_str() == Some(intent_id)
                && item["repair"]
                    .as_str()
                    .is_some_and(|repair| !repair.trim().is_empty())
        }),
        "expected audit_without_memory drift for {intent_id}: {value:?}"
    );

    let _ = child.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, injects stale memory, repairs it through CLI, and verifies again"]
async fn live_cli_verify_json_repair_clears_stale_parent_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;

    run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.write"],
    )
    .await;
    let intent_stdout = run_cli(
        &cli_exe,
        home.path(),
        &["intent", "--json", "verify repair fixture"],
    )
    .await;
    let intent: Value =
        serde_json::from_str(intent_stdout.trim()).expect("intent --json must be JSON");
    let memory_id: Uuid = intent["intent_id"]
        .as_str()
        .expect("intent_id")
        .parse()
        .expect("intent_id must be uuid");

    let _ = child.kill().await;

    let missing_parent = Uuid::new_v4();
    let store = SqliteStore::open(&home.path().join("memory.db")).expect("open memory db");
    let mut record = store
        .get(memory_id)
        .await
        .expect("load memory")
        .expect("memory record exists");
    record.parent = Some(missing_parent);
    store.put(record).await.expect("inject stale parent");
    drop(store);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify should fail while stale parent drift exists: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");
    let item = stale_parent_drift_for(&drift, memory_id)
        .unwrap_or_else(|| panic!("expected stale parent drift for {memory_id}: {drift:?}"));
    assert!(
        item["message"]
            .as_str()
            .is_some_and(|message| message.contains(&missing_parent.to_string())),
        "stale parent drift should name the missing parent: {item:?}"
    );

    run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.repair.apply"],
    )
    .await;
    let memory_id_s = memory_id.to_string();
    let missing_parent_s = missing_parent.to_string();
    let repair_stdout = run_cli(
        &cli_exe,
        home.path(),
        &[
            "memory",
            "repair",
            "detach-parent",
            &memory_id_s,
            "--expected-parent",
            &missing_parent_s,
            "--reason",
            "live verifier repair",
            "--apply",
        ],
    )
    .await;
    let repair: Value =
        serde_json::from_str(repair_stdout.trim()).expect("memory repair stdout must be JSON");
    assert_eq!(repair["id"].as_str(), Some(memory_id_s.as_str()));
    assert_eq!(repair["action"].as_str(), Some("detach_parent"));
    assert_eq!(repair["mode"].as_str(), Some("apply"));
    assert_eq!(repair["changed"].as_bool(), Some(true));
    assert!(repair["after"]["parent"].is_null(), "{repair:?}");

    let clean_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let clean_stdout = String::from_utf8_lossy(&clean_output.stdout).to_string();
    let clean_stderr = String::from_utf8_lossy(&clean_output.stderr).to_string();
    assert!(
        clean_output.status.success(),
        "verify should pass after targeted repair: status={:?} stdout={clean_stdout:?} stderr={clean_stderr:?}",
        clean_output.status
    );
    let clean: Value =
        serde_json::from_str(clean_stdout.trim()).expect("post-repair verify stdout must be JSON");
    assert!(
        stale_parent_drift_for(&clean, memory_id).is_none(),
        "targeted stale parent drift should be gone after repair: {clean:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, rewrites memory.created_at + injects a back-dated receipt, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_settled_before_created_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;

    run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.write"],
    )
    .await;
    let intent_stdout = run_cli(
        &cli_exe,
        home.path(),
        &["intent", "--json", "verify temporal fixture"],
    )
    .await;
    let intent: Value =
        serde_json::from_str(intent_stdout.trim()).expect("intent --json must be JSON");
    let memory_id: Uuid = intent["intent_id"]
        .as_str()
        .expect("intent_id")
        .parse()
        .expect("intent_id must be uuid");

    let _ = child.kill().await;

    let store = SqliteStore::open(&home.path().join("memory.db")).expect("open memory db");
    let mut record = store
        .get(memory_id)
        .await
        .expect("load memory")
        .expect("memory record exists");
    let owner = record.owner.clone();
    record.created_at = 9_000_000;
    store.put(record).await.expect("rewrite memory created_at");
    let reread = store
        .get(memory_id)
        .await
        .expect("reload memory")
        .expect("memory persists");
    assert_eq!(
        reread.created_at, 9_000_000,
        "SqliteStore must persist the rewritten created_at",
    );
    drop(store);

    let receipt_id = Uuid::new_v4();
    let backdated = SettlementReceipt {
        id: receipt_id,
        payer: owner,
        resource: ResourceKind::Memory,
        memory_record_id: Some(memory_id),
        credits_consumed: 1,
        settled_at: 1_000,
        chain: None,
        cluster: None,
        batch_id: None,
        merkle_root: None,
        tx_sig: None,
        slot: None,
        confirmed_at: None,
        onchain_sig: None,
    };
    let receipts_path = home.path().join("receipts").join("working.jsonl");
    use std::io::Write as _;
    let mut receipts = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&receipts_path)
        .expect("open receipts/working.jsonl for append");
    writeln!(receipts, "{}", serde_json::to_string(&backdated).unwrap())
        .expect("append back-dated receipt");
    drop(receipts);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when temporal drift exists: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let temporal = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("memory_receipt_settled_before_created")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        })
        .unwrap_or_else(|| panic!("expected temporal drift for {receipt_id}: {drift:?}"));
    let message = temporal["message"].as_str().unwrap_or("");
    assert!(
        message.contains("settled_at=1000") && message.contains("created_at=9000000"),
        "drift message should record both timestamps: {message:?}"
    );
    assert!(
        !drift["drift"].as_array().unwrap().iter().any(|item| {
            item["kind"].as_str() == Some("memory_receipt_owner_mismatch")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        }),
        "matched payer must not double-report under owner mismatch: {drift:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, injects a Compute receipt carrying memory_record_id, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_resource_mismatch_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let receipts_dir = home.path().join("receipts");
    std::fs::create_dir_all(&receipts_dir).expect("create receipts dir");
    let receipt_id = Uuid::new_v4();
    let mismatch = SettlementReceipt {
        id: receipt_id,
        payer: AgentId::new("user@local", [0u8; 32]),
        resource: ResourceKind::Compute,
        memory_record_id: Some(Uuid::new_v4()),
        credits_consumed: 1,
        settled_at: 1,
        chain: None,
        cluster: None,
        batch_id: None,
        merkle_root: None,
        tx_sig: None,
        slot: None,
        confirmed_at: None,
        onchain_sig: None,
    };
    let receipts_path = receipts_dir.join("working.jsonl");
    use std::io::Write as _;
    let mut receipts = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&receipts_path)
        .expect("open receipts/working.jsonl for append");
    writeln!(receipts, "{}", serde_json::to_string(&mismatch).unwrap())
        .expect("append mismatched receipt");
    drop(receipts);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when resource-mismatch drift exists: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let mismatch_row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("memory_receipt_resource_mismatch")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        })
        .unwrap_or_else(|| panic!("expected resource-mismatch drift for {receipt_id}: {drift:?}"));
    assert!(
        mismatch_row["message"]
            .as_str()
            .is_some_and(|m| m.contains("Compute")),
        "drift message should record the observed resource: {mismatch_row:?}"
    );
    assert!(
        !drift["drift"].as_array().unwrap().iter().any(|item| {
            item["kind"].as_str() == Some("receipt_without_memory_record")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        }),
        "cross-resource receipt must not double-report under receipt_without_memory_record: {drift:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a duplicate IntentDispatched audit row, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_intent_dispatched_duplicate_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;

    run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.write"],
    )
    .await;
    let intent_stdout = run_cli(
        &cli_exe,
        home.path(),
        &["intent", "--json", "duplicate dispatch fixture"],
    )
    .await;
    let intent: Value =
        serde_json::from_str(intent_stdout.trim()).expect("intent --json must be JSON");
    let intent_id: Uuid = intent["intent_id"]
        .as_str()
        .expect("intent_id")
        .parse()
        .expect("intent_id must be uuid");

    let _ = child.kill().await;

    let events_path = home.path().join("audit").join("events.jsonl");
    let raw = std::fs::read_to_string(&events_path).expect("read audit events");
    let intent_id_str = intent_id.to_string();
    let original = raw
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|event| {
            event["kind"]["type"].as_str() == Some("intent_dispatched")
                && event["kind"]["intent_id"].as_str() == Some(&intent_id_str)
        })
        .unwrap_or_else(|| panic!("no IntentDispatched row for {intent_id} in {raw}"));
    let mut duplicate = original.clone();
    duplicate["id"] = Value::String(Uuid::new_v4().to_string());
    let mut events = std::fs::OpenOptions::new()
        .append(true)
        .open(&events_path)
        .expect("open events.jsonl for append");
    use std::io::Write as _;
    writeln!(events, "{}", serde_json::to_string(&duplicate).unwrap())
        .expect("append duplicate row");
    drop(events);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when duplicate intent drift exists: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let duplicates: Vec<&Value> = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .filter(|item| {
            item["kind"].as_str() == Some("intent_dispatched_duplicate")
                && item["id"].as_str() == Some(&intent_id_str)
        })
        .collect();
    assert_eq!(
        duplicates.len(),
        1,
        "exactly one intent_dispatched_duplicate row per intent_id: {drift:?}"
    );
    assert!(
        duplicates[0]["message"]
            .as_str()
            .is_some_and(|m| m.contains("2 IntentDispatched")),
        "duplicate row should record the observed count: {:?}",
        duplicates[0]
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, injects a two-node parent cycle via SqliteStore, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_parent_cycle_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;

    run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.write"],
    )
    .await;
    let intent_stdout = run_cli(
        &cli_exe,
        home.path(),
        &["intent", "--json", "verify cycle fixture"],
    )
    .await;
    let intent: Value =
        serde_json::from_str(intent_stdout.trim()).expect("intent --json must be JSON");
    let a_id: Uuid = intent["intent_id"]
        .as_str()
        .expect("intent_id")
        .parse()
        .expect("intent_id must be uuid");

    let _ = child.kill().await;

    let store = SqliteStore::open(&home.path().join("memory.db")).expect("open memory db");
    let mut a = store
        .get(a_id)
        .await
        .expect("load A")
        .expect("memory record A exists");
    let b_id = Uuid::new_v4();
    let b = MemoryRecord {
        id: b_id,
        tier: MemoryTier::Working,
        owner: a.owner.clone(),
        text: "cycle node B".into(),
        embedding: vec![],
        metadata: serde_json::json!({}),
        created_at: a.created_at,
        parent: Some(a_id),
    };
    store.put(b).await.expect("insert B");
    a.parent = Some(b_id);
    store.put(a).await.expect("rewrite A parent");
    let reread_a = store
        .get(a_id)
        .await
        .expect("reload A")
        .expect("A persists");
    assert_eq!(
        reread_a.parent,
        Some(b_id),
        "SqliteStore must persist the cycle parent through put"
    );
    drop(store);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when cycle drift exists: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let cycle = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("memory_parent_cycle")
                && item["id"].as_str() == Some(&a_id.to_string())
        })
        .unwrap_or_else(|| panic!("expected memory_parent_cycle drift for {a_id}: {drift:?}"));
    assert!(
        cycle["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("detach_parent")),
        "parent cycle drift repair should name detach_parent: {cycle:?}"
    );
    assert!(
        !drift["drift"].as_array().unwrap().iter().any(|item| {
            (item["kind"].as_str() == Some("memory_stale_parent")
                || item["kind"].as_str() == Some("memory_self_parent"))
                && item["id"].as_str() == Some(&a_id.to_string())
        }),
        "cycle must not double-report as stale or self parent for A: {drift:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, injects self-referential parent, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_self_parent_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;

    run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.write"],
    )
    .await;
    let intent_stdout = run_cli(
        &cli_exe,
        home.path(),
        &["intent", "--json", "verify self-parent fixture"],
    )
    .await;
    let intent: Value =
        serde_json::from_str(intent_stdout.trim()).expect("intent --json must be JSON");
    let memory_id: Uuid = intent["intent_id"]
        .as_str()
        .expect("intent_id")
        .parse()
        .expect("intent_id must be uuid");

    let _ = child.kill().await;

    let store = SqliteStore::open(&home.path().join("memory.db")).expect("open memory db");
    let mut record = store
        .get(memory_id)
        .await
        .expect("load memory")
        .expect("memory record exists");
    record.parent = Some(record.id);
    store.put(record).await.expect("inject self-parent");
    let reread = store
        .get(memory_id)
        .await
        .expect("reload memory")
        .expect("memory record persists");
    assert_eq!(
        reread.parent,
        Some(memory_id),
        "SqliteStore must persist self-parent through put; otherwise the live coverage is meaningless"
    );
    drop(store);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when self-parent drift exists: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let self_parent = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("memory_self_parent")
                && item["id"].as_str() == Some(&memory_id.to_string())
        })
        .unwrap_or_else(|| panic!("expected memory_self_parent drift for {memory_id}: {drift:?}"));
    assert!(
        self_parent["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("detach_parent")),
        "self-parent drift repair string should name detach_parent: {self_parent:?}"
    );
    assert!(
        stale_parent_drift_for(&drift, memory_id).is_none(),
        "self-parent must not double-report as memory_stale_parent: {drift:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, injects parent=Some(Uuid::nil()) via SqliteStore, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_memory_record_parent_nil_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;

    run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.write"],
    )
    .await;
    let intent_stdout = run_cli(
        &cli_exe,
        home.path(),
        &["intent", "--json", "verify nil-parent fixture"],
    )
    .await;
    let intent: Value =
        serde_json::from_str(intent_stdout.trim()).expect("intent --json must be JSON");
    let memory_id: Uuid = intent["intent_id"]
        .as_str()
        .expect("intent_id")
        .parse()
        .expect("intent_id must be uuid");

    let _ = child.kill().await;

    let store = SqliteStore::open(&home.path().join("memory.db")).expect("open memory db");
    let mut record = store
        .get(memory_id)
        .await
        .expect("load memory")
        .expect("memory record exists");
    record.parent = Some(Uuid::nil());
    store.put(record).await.expect("inject nil-parent");
    let reread = store
        .get(memory_id)
        .await
        .expect("reload memory")
        .expect("memory record persists");
    assert_eq!(
        reread.parent,
        Some(Uuid::nil()),
        "SqliteStore must persist nil-parent through put; otherwise the live coverage is meaningless"
    );
    drop(store);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when nil-parent drift exists: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let nil_parent = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("memory_record_parent_nil")
                && item["id"].as_str() == Some(&memory_id.to_string())
        })
        .unwrap_or_else(|| {
            panic!("expected memory_record_parent_nil drift for {memory_id}: {drift:?}")
        });
    assert!(
        nil_parent["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("detach_parent")),
        "nil-parent drift repair string should name detach_parent: {nil_parent:?}"
    );
    assert!(
        stale_parent_drift_for(&drift, memory_id).is_none(),
        "nil-parent must not double-report as memory_stale_parent: {drift:?}"
    );
    assert!(
        !drift["drift"].as_array().unwrap().iter().any(|item| {
            item["kind"].as_str() == Some("memory_self_parent")
                && item["id"].as_str() == Some(&memory_id.to_string())
        }),
        "nil-parent must not double-report as memory_self_parent: {drift:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, rewrites a memory record's embedding to contain f32::INFINITY, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_memory_record_embedding_infinite_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;

    run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.write"],
    )
    .await;
    let intent_stdout = run_cli(
        &cli_exe,
        home.path(),
        &["intent", "--json", "verify infinite-embedding fixture"],
    )
    .await;
    let intent: Value =
        serde_json::from_str(intent_stdout.trim()).expect("intent --json must be JSON");
    let memory_id: Uuid = intent["intent_id"]
        .as_str()
        .expect("intent_id")
        .parse()
        .expect("intent_id must be uuid");

    let _ = child.kill().await;

    let store = SqliteStore::open(&home.path().join("memory.db")).expect("open memory db");
    let mut record = store
        .get(memory_id)
        .await
        .expect("load memory")
        .expect("memory record exists");
    record.embedding = vec![1.0, f32::INFINITY, 0.5];
    store.put(record).await.expect("inject infinite-embedding");
    let reread = store
        .get(memory_id)
        .await
        .expect("reload memory")
        .expect("memory record persists");
    assert!(
        reread.embedding.iter().any(|v| v.is_infinite()),
        "SqliteStore must persist infinite-embedding through put; otherwise the live coverage is meaningless: embedding={:?}",
        reread.embedding
    );
    drop(store);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when infinite-embedding drift exists: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let infinite = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("memory_record_embedding_infinite")
                && item["id"].as_str() == Some(&memory_id.to_string())
        })
        .unwrap_or_else(|| {
            panic!("expected memory_record_embedding_infinite drift for {memory_id}: {drift:?}")
        });
    assert!(
        infinite["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("delete_record")),
        "infinite-embedding drift repair string should name delete_record: {infinite:?}"
    );
    assert!(
        !drift["drift"].as_array().unwrap().iter().any(|item| {
            item["kind"].as_str() == Some("memory_nan_embedding")
                && item["id"].as_str() == Some(&memory_id.to_string())
        }),
        "infinite-embedding must not double-report as memory_nan_embedding: {drift:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, rewrites a memory record's metadata to a JSON array, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_memory_record_metadata_non_object_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;

    run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.write"],
    )
    .await;
    let intent_stdout = run_cli(
        &cli_exe,
        home.path(),
        &["intent", "--json", "verify non-object metadata fixture"],
    )
    .await;
    let intent: Value =
        serde_json::from_str(intent_stdout.trim()).expect("intent --json must be JSON");
    let memory_id: Uuid = intent["intent_id"]
        .as_str()
        .expect("intent_id")
        .parse()
        .expect("intent_id must be uuid");

    let _ = child.kill().await;

    let store = SqliteStore::open(&home.path().join("memory.db")).expect("open memory db");
    let mut record = store
        .get(memory_id)
        .await
        .expect("load memory")
        .expect("memory record exists");
    record.metadata = serde_json::Value::Array(vec![]);
    store.put(record).await.expect("inject non-object metadata");
    let reread = store
        .get(memory_id)
        .await
        .expect("reload memory")
        .expect("memory record persists");
    assert!(
        reread.metadata.is_array(),
        "SqliteStore must persist non-object metadata through put; otherwise the live coverage is meaningless: metadata={:?}",
        reread.metadata
    );
    drop(store);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when non-object metadata drift exists: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let non_object = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("memory_record_metadata_non_object")
                && item["id"].as_str() == Some(&memory_id.to_string())
        })
        .unwrap_or_else(|| {
            panic!("expected memory_record_metadata_non_object drift for {memory_id}: {drift:?}")
        });
    assert!(
        non_object["message"]
            .as_str()
            .is_some_and(|message| message.contains("JSON type array")),
        "drift message should name the array shape: {non_object:?}"
    );
    assert!(
        non_object["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("previous_metadata")),
        "drift repair should name the provenance-merge wrapper: {non_object:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, rewrites a memory record's text to empty, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_empty_text_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;

    run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.write"],
    )
    .await;
    let intent_stdout = run_cli(
        &cli_exe,
        home.path(),
        &["intent", "--json", "verify empty-text fixture"],
    )
    .await;
    let intent: Value =
        serde_json::from_str(intent_stdout.trim()).expect("intent --json must be JSON");
    let memory_id: Uuid = intent["intent_id"]
        .as_str()
        .expect("intent_id")
        .parse()
        .expect("intent_id must be uuid");

    let _ = child.kill().await;

    let store = SqliteStore::open(&home.path().join("memory.db")).expect("open memory db");
    let mut record = store
        .get(memory_id)
        .await
        .expect("load memory")
        .expect("memory record exists");
    record.text = String::new();
    store.put(record).await.expect("inject empty text");
    let reread = store
        .get(memory_id)
        .await
        .expect("reload memory")
        .expect("memory record persists");
    assert!(
        reread.text.is_empty(),
        "SqliteStore must persist empty text through put; otherwise the live coverage is meaningless"
    );
    drop(store);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when empty-text drift exists: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let empty = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("memory_empty_text")
                && item["id"].as_str() == Some(&memory_id.to_string())
        })
        .unwrap_or_else(|| panic!("expected memory_empty_text drift for {memory_id}: {drift:?}"));
    assert!(
        empty["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("delete_record")),
        "empty-text drift repair string should name delete_record: {empty:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, rewrites a memory record's embedding with NaN, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_nan_embedding_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;

    run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.write"],
    )
    .await;
    let intent_stdout = run_cli(
        &cli_exe,
        home.path(),
        &["intent", "--json", "verify nan-embedding fixture"],
    )
    .await;
    let intent: Value =
        serde_json::from_str(intent_stdout.trim()).expect("intent --json must be JSON");
    let memory_id: Uuid = intent["intent_id"]
        .as_str()
        .expect("intent_id")
        .parse()
        .expect("intent_id must be uuid");

    let _ = child.kill().await;

    let store = SqliteStore::open(&home.path().join("memory.db")).expect("open memory db");
    let mut record = store
        .get(memory_id)
        .await
        .expect("load memory")
        .expect("memory record exists");
    record.embedding = vec![1.0, f32::NAN, 0.5];
    store.put(record).await.expect("inject NaN embedding");
    let reread = store
        .get(memory_id)
        .await
        .expect("reload memory")
        .expect("memory record persists");
    assert_eq!(
        reread.embedding.len(),
        3,
        "SqliteStore must persist the 3-value embedding through the BLOB round-trip: {reread:?}"
    );
    assert!(
        reread.embedding[1].is_nan(),
        "SqliteStore must preserve the NaN bit pattern through put/get; otherwise the live coverage is meaningless: {:?}",
        reread.embedding
    );
    drop(store);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when NaN-embedding drift exists: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let nan = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("memory_nan_embedding")
                && item["id"].as_str() == Some(&memory_id.to_string())
        })
        .unwrap_or_else(|| {
            panic!("expected memory_nan_embedding drift for {memory_id}: {drift:?}")
        });
    assert!(
        nan["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("delete_record")),
        "NaN-embedding drift repair string should name delete_record: {nan:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a confirmed-but-chain-less receipt, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_confirmed_without_chain_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let receipts_dir = home.path().join("receipts");
    std::fs::create_dir_all(&receipts_dir).expect("create receipts dir");
    let receipt_id = Uuid::new_v4();
    let detached = SettlementReceipt {
        id: receipt_id,
        payer: AgentId::new("user@local", [0u8; 32]),
        resource: ResourceKind::Compute,
        memory_record_id: None,
        credits_consumed: 1,
        settled_at: 1_000,
        chain: None,
        cluster: None,
        batch_id: None,
        merkle_root: None,
        tx_sig: None,
        slot: None,
        confirmed_at: Some(2_000),
        onchain_sig: None,
    };
    let receipts_path = receipts_dir.join("working.jsonl");
    use std::io::Write as _;
    let mut receipts = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&receipts_path)
        .expect("open receipts/working.jsonl for append");
    writeln!(receipts, "{}", serde_json::to_string(&detached).unwrap())
        .expect("append detached receipt");
    drop(receipts);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when confirmed-without-chain drift exists: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("receipt_confirmed_without_chain")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        })
        .unwrap_or_else(|| {
            panic!("expected receipt_confirmed_without_chain drift for {receipt_id}: {drift:?}")
        });
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("annotate_receipt")),
        "confirmed-without-chain drift repair string should name annotate_receipt: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an all-four-Some receipt with chain=Some(\"\"), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_receipt_chain_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let receipts_dir = home.path().join("receipts");
    std::fs::create_dir_all(&receipts_dir).expect("create receipts dir");
    let receipt_id = Uuid::new_v4();
    let empty_chain = SettlementReceipt {
        id: receipt_id,
        payer: AgentId::new("user@local", [0u8; 32]),
        resource: ResourceKind::Compute,
        memory_record_id: None,
        credits_consumed: 1,
        settled_at: 1_000,
        chain: Some(String::new()),
        cluster: Some("devnet".to_string()),
        batch_id: Some("b".repeat(64)),
        merkle_root: Some("m".repeat(64)),
        tx_sig: None,
        slot: None,
        confirmed_at: Some(2_000),
        onchain_sig: None,
    };
    let receipts_path = receipts_dir.join("working.jsonl");
    use std::io::Write as _;
    let mut receipts = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&receipts_path)
        .expect("open receipts/working.jsonl for append");
    writeln!(receipts, "{}", serde_json::to_string(&empty_chain).unwrap())
        .expect("append empty-chain receipt");
    drop(receipts);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when receipt has chain=Some(\"\"): status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("receipt_chain_empty")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        })
        .unwrap_or_else(|| {
            panic!("expected receipt_chain_empty drift for {receipt_id}: {drift:?}")
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("chain = Some(\"\")"),
        "drift message should name the Some(empty) invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("annotate_receipt")
            && repair.contains("\"solana\"")
            && repair.contains("receipt_chain_partial")),
        "empty-chain drift repair string should name annotate_receipt, the hardcoded \"solana\" literal, and the receipt_chain_partial bypass: {row:?}"
    );
    assert!(
        !drift["drift"].as_array().unwrap().iter().any(|item| {
            item["kind"].as_str() == Some("receipt_chain_partial")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        }),
        "all-four-Some bundle must not double-report under receipt_chain_partial: {drift:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an all-four-Some receipt with confirmed_at=Some(0), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_receipt_confirmed_at_zero_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let receipts_dir = home.path().join("receipts");
    std::fs::create_dir_all(&receipts_dir).expect("create receipts dir");
    let receipt_id = Uuid::new_v4();
    let zero_confirmed_at = SettlementReceipt {
        id: receipt_id,
        payer: AgentId::new("user@local", [1u8; 32]),
        resource: ResourceKind::Compute,
        memory_record_id: None,
        credits_consumed: 1,
        settled_at: 1_000,
        chain: Some("solana".to_string()),
        cluster: Some("devnet".to_string()),
        batch_id: Some("b".repeat(64)),
        merkle_root: Some("m".repeat(64)),
        tx_sig: Some("t".repeat(88)),
        slot: Some(42),
        confirmed_at: Some(0),
        onchain_sig: Some("t".repeat(88)),
    };
    let receipts_path = receipts_dir.join("working.jsonl");
    use std::io::Write as _;
    let mut receipts = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&receipts_path)
        .expect("open receipts/working.jsonl for append");
    writeln!(
        receipts,
        "{}",
        serde_json::to_string(&zero_confirmed_at).unwrap()
    )
    .expect("append zero-confirmed-at receipt");
    drop(receipts);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when receipt has confirmed_at=Some(0): status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("receipt_confirmed_at_zero")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        })
        .unwrap_or_else(|| {
            panic!("expected receipt_confirmed_at_zero drift for {receipt_id}: {drift:?}")
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("confirmed_at = Some(0)"),
        "drift message should name the Some(0) invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("annotate_receipt")),
        "zero-confirmed-at drift repair string should name annotate_receipt: {row:?}"
    );
    assert!(
        !drift["drift"].as_array().unwrap().iter().any(|item| {
            item["kind"].as_str() == Some("receipt_confirmed_without_chain")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        }),
        "all-four-Some bundle must not double-report as receipt_confirmed_without_chain: {drift:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a receipt with credits_consumed=0, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_receipt_credits_consumed_zero_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let receipts_dir = home.path().join("receipts");
    std::fs::create_dir_all(&receipts_dir).expect("create receipts dir");
    let receipt_id = Uuid::new_v4();
    let zero_credits = SettlementReceipt {
        id: receipt_id,
        payer: AgentId::new("user@local", [1u8; 32]),
        resource: ResourceKind::Compute,
        memory_record_id: None,
        credits_consumed: 0,
        settled_at: 1_000,
        chain: None,
        cluster: None,
        batch_id: None,
        merkle_root: None,
        tx_sig: None,
        slot: None,
        confirmed_at: None,
        onchain_sig: None,
    };
    let receipts_path = receipts_dir.join("working.jsonl");
    use std::io::Write as _;
    let mut receipts = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&receipts_path)
        .expect("open receipts/working.jsonl for append");
    writeln!(
        receipts,
        "{}",
        serde_json::to_string(&zero_credits).unwrap()
    )
    .expect("append zero-credits receipt");
    drop(receipts);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when receipt has credits_consumed=0: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("receipt_credits_consumed_zero")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        })
        .unwrap_or_else(|| {
            panic!("expected receipt_credits_consumed_zero drift for {receipt_id}: {drift:?}")
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("credits_consumed = 0"),
        "drift message should name the zero invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("memory_write_credits")),
        "zero-credits drift repair string should name memory_write_credits: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an all-four-Some receipt with batch_id=Some(\"\"), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_receipt_batch_id_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let receipts_dir = home.path().join("receipts");
    std::fs::create_dir_all(&receipts_dir).expect("create receipts dir");
    let receipt_id = Uuid::new_v4();
    let empty_batch_id = SettlementReceipt {
        id: receipt_id,
        payer: AgentId::new("user@local", [0u8; 32]),
        resource: ResourceKind::Compute,
        memory_record_id: None,
        credits_consumed: 1,
        settled_at: 1_000,
        chain: Some("solana".to_string()),
        cluster: Some("devnet".to_string()),
        batch_id: Some(String::new()),
        merkle_root: Some("m".repeat(64)),
        tx_sig: None,
        slot: None,
        confirmed_at: Some(2_000),
        onchain_sig: None,
    };
    let receipts_path = receipts_dir.join("working.jsonl");
    use std::io::Write as _;
    let mut receipts = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&receipts_path)
        .expect("open receipts/working.jsonl for append");
    writeln!(
        receipts,
        "{}",
        serde_json::to_string(&empty_batch_id).unwrap()
    )
    .expect("append empty-batch-id receipt");
    drop(receipts);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when receipt has batch_id=Some(\"\"): status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("receipt_batch_id_empty")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        })
        .unwrap_or_else(|| {
            panic!("expected receipt_batch_id_empty drift for {receipt_id}: {drift:?}")
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("batch_id = Some(\"\")"),
        "drift message should name the Some(empty) invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("build_receipt_batch")
            && repair.contains("hex32")
            && repair.contains("receipt_chain_partial")),
        "empty-batch-id drift repair string should name build_receipt_batch, hex32, and the receipt_chain_partial bypass: {row:?}"
    );
    assert!(
        !drift["drift"].as_array().unwrap().iter().any(|item| {
            item["kind"].as_str() == Some("receipt_chain_partial")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        }),
        "all-four-Some bundle must not double-report under receipt_chain_partial: {drift:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an all-four-Some receipt with onchain_sig=Some(\"\") and tx_sig=None (so receipt_tx_sig_onchain_sig_diverged and receipt_tx_sig_empty both stay silent), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_receipt_onchain_sig_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let receipts_dir = home.path().join("receipts");
    std::fs::create_dir_all(&receipts_dir).expect("create receipts dir");
    let receipt_id = Uuid::new_v4();
    let empty_onchain_sig = SettlementReceipt {
        id: receipt_id,
        payer: AgentId::new("user@local", [0u8; 32]),
        resource: ResourceKind::Compute,
        memory_record_id: None,
        credits_consumed: 1,
        settled_at: 1_000,
        chain: Some("solana".to_string()),
        cluster: Some("devnet".to_string()),
        batch_id: Some("b".repeat(64)),
        merkle_root: Some("m".repeat(64)),
        tx_sig: None,
        slot: None,
        confirmed_at: Some(2_000),
        onchain_sig: Some(String::new()),
    };
    let receipts_path = receipts_dir.join("working.jsonl");
    use std::io::Write as _;
    let mut receipts = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&receipts_path)
        .expect("open receipts/working.jsonl for append");
    writeln!(
        receipts,
        "{}",
        serde_json::to_string(&empty_onchain_sig).unwrap()
    )
    .expect("append empty-onchain-sig receipt");
    drop(receipts);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when receipt has onchain_sig=Some(\"\"): status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("receipt_onchain_sig_empty")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        })
        .unwrap_or_else(|| {
            panic!("expected receipt_onchain_sig_empty drift for {receipt_id}: {drift:?}")
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("onchain_sig = Some(\"\")"),
        "drift message should name the Some(empty) invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("annotate_receipt")
            && repair.contains("confirmation.tx_sig.clone()")
            && repair.contains("receipt_tx_sig_onchain_sig_diverged")),
        "empty-onchain-sig drift repair string should name annotate_receipt, confirmation.tx_sig.clone(), and the receipt_tx_sig_onchain_sig_diverged bypass: {row:?}"
    );
    assert!(
        !drift["drift"].as_array().unwrap().iter().any(|item| {
            item["kind"].as_str() == Some("receipt_tx_sig_onchain_sig_diverged")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        }),
        "tx_sig=None with onchain_sig=Some(\"\") must not double-report under receipt_tx_sig_onchain_sig_diverged: {drift:?}"
    );
    assert!(
        !drift["drift"].as_array().unwrap().iter().any(|item| {
            item["kind"].as_str() == Some("receipt_tx_sig_empty")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        }),
        "tx_sig=None must not double-report under receipt_tx_sig_empty: {drift:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an all-four-Some receipt with tx_sig=Some(\"\") (and onchain_sig=Some(\"\") so they remain equal), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_receipt_tx_sig_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let receipts_dir = home.path().join("receipts");
    std::fs::create_dir_all(&receipts_dir).expect("create receipts dir");
    let receipt_id = Uuid::new_v4();
    let empty_tx_sig = SettlementReceipt {
        id: receipt_id,
        payer: AgentId::new("user@local", [0u8; 32]),
        resource: ResourceKind::Compute,
        memory_record_id: None,
        credits_consumed: 1,
        settled_at: 1_000,
        chain: Some("solana".to_string()),
        cluster: Some("devnet".to_string()),
        batch_id: Some("b".repeat(64)),
        merkle_root: Some("m".repeat(64)),
        tx_sig: Some(String::new()),
        slot: None,
        confirmed_at: Some(2_000),
        onchain_sig: Some(String::new()),
    };
    let receipts_path = receipts_dir.join("working.jsonl");
    use std::io::Write as _;
    let mut receipts = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&receipts_path)
        .expect("open receipts/working.jsonl for append");
    writeln!(
        receipts,
        "{}",
        serde_json::to_string(&empty_tx_sig).unwrap()
    )
    .expect("append empty-tx-sig receipt");
    drop(receipts);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when receipt has tx_sig=Some(\"\"): status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("receipt_tx_sig_empty")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        })
        .unwrap_or_else(|| {
            panic!("expected receipt_tx_sig_empty drift for {receipt_id}: {drift:?}")
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("tx_sig = Some(\"\")"),
        "drift message should name the Some(empty) invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("annotate_receipt")
            && repair.contains("confirmation.tx_sig.clone()")
            && repair.contains("receipt_tx_sig_onchain_sig_diverged")),
        "empty-tx-sig drift repair string should name annotate_receipt, confirmation.tx_sig.clone(), and the receipt_tx_sig_onchain_sig_diverged bypass: {row:?}"
    );
    assert!(
        !drift["drift"].as_array().unwrap().iter().any(|item| {
            item["kind"].as_str() == Some("receipt_tx_sig_onchain_sig_diverged")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        }),
        "lockstep Some(\"\") tx_sig/onchain_sig must not double-report under receipt_tx_sig_onchain_sig_diverged: {drift:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an all-four-Some receipt with merkle_root=Some(\"\"), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_receipt_merkle_root_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let receipts_dir = home.path().join("receipts");
    std::fs::create_dir_all(&receipts_dir).expect("create receipts dir");
    let receipt_id = Uuid::new_v4();
    let empty_merkle_root = SettlementReceipt {
        id: receipt_id,
        payer: AgentId::new("user@local", [0u8; 32]),
        resource: ResourceKind::Compute,
        memory_record_id: None,
        credits_consumed: 1,
        settled_at: 1_000,
        chain: Some("solana".to_string()),
        cluster: Some("devnet".to_string()),
        batch_id: Some("b".repeat(64)),
        merkle_root: Some(String::new()),
        tx_sig: None,
        slot: None,
        confirmed_at: Some(2_000),
        onchain_sig: None,
    };
    let receipts_path = receipts_dir.join("working.jsonl");
    use std::io::Write as _;
    let mut receipts = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&receipts_path)
        .expect("open receipts/working.jsonl for append");
    writeln!(
        receipts,
        "{}",
        serde_json::to_string(&empty_merkle_root).unwrap()
    )
    .expect("append empty-merkle-root receipt");
    drop(receipts);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when receipt has merkle_root=Some(\"\"): status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("receipt_merkle_root_empty")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        })
        .unwrap_or_else(|| {
            panic!("expected receipt_merkle_root_empty drift for {receipt_id}: {drift:?}")
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("merkle_root = Some(\"\")"),
        "drift message should name the Some(empty) invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("build_receipt_batch")
            && repair.contains("hex32(level[0])")
            && repair.contains("receipt_chain_partial")),
        "empty-merkle-root drift repair string should name build_receipt_batch, hex32(level[0]), and the receipt_chain_partial bypass: {row:?}"
    );
    assert!(
        !drift["drift"].as_array().unwrap().iter().any(|item| {
            item["kind"].as_str() == Some("receipt_chain_partial")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        }),
        "all-four-Some bundle must not double-report under receipt_chain_partial: {drift:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a partial-chain-bundle receipt, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_chain_partial_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let receipts_dir = home.path().join("receipts");
    std::fs::create_dir_all(&receipts_dir).expect("create receipts dir");
    let receipt_id = Uuid::new_v4();
    let half_bundle = SettlementReceipt {
        id: receipt_id,
        payer: AgentId::new("user@local", [0u8; 32]),
        resource: ResourceKind::Compute,
        memory_record_id: None,
        credits_consumed: 1,
        settled_at: 1_000,
        chain: Some("solana".to_string()),
        cluster: Some("devnet".to_string()),
        batch_id: None,
        merkle_root: None,
        tx_sig: None,
        slot: None,
        confirmed_at: None,
        onchain_sig: None,
    };
    let receipts_path = receipts_dir.join("working.jsonl");
    use std::io::Write as _;
    let mut receipts = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&receipts_path)
        .expect("open receipts/working.jsonl for append");
    writeln!(receipts, "{}", serde_json::to_string(&half_bundle).unwrap())
        .expect("append half-bundle receipt");
    drop(receipts);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when chain-partial drift exists: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("receipt_chain_partial")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        })
        .unwrap_or_else(|| {
            panic!("expected receipt_chain_partial drift for {receipt_id}: {drift:?}")
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("chain=true")
            && message.contains("cluster=true")
            && message.contains("batch_id=false")
            && message.contains("merkle_root=false"),
        "drift message should record every bundle field's set state: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("annotate_receipt")),
        "chain-partial drift repair string should name annotate_receipt: {row:?}"
    );
    assert!(
        !drift["drift"].as_array().unwrap().iter().any(|item| {
            item["kind"].as_str() == Some("receipt_confirmed_without_chain")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        }),
        "chain=Some must not double-report under receipt_confirmed_without_chain: {drift:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a tx_sig/onchain_sig-diverged receipt, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_tx_sig_onchain_sig_diverged_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let receipts_dir = home.path().join("receipts");
    std::fs::create_dir_all(&receipts_dir).expect("create receipts dir");
    let receipt_id = Uuid::new_v4();
    let diverged = SettlementReceipt {
        id: receipt_id,
        payer: AgentId::new("user@local", [0u8; 32]),
        resource: ResourceKind::Compute,
        memory_record_id: None,
        credits_consumed: 1,
        settled_at: 1_000,
        chain: None,
        cluster: None,
        batch_id: None,
        merkle_root: None,
        tx_sig: Some("sig-from-annotate".to_string()),
        slot: None,
        confirmed_at: None,
        onchain_sig: Some("sig-rewritten-out-of-band".to_string()),
    };
    let receipts_path = receipts_dir.join("working.jsonl");
    use std::io::Write as _;
    let mut receipts = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&receipts_path)
        .expect("open receipts/working.jsonl for append");
    writeln!(receipts, "{}", serde_json::to_string(&diverged).unwrap())
        .expect("append diverged receipt");
    drop(receipts);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when tx_sig/onchain_sig divergence exists: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("receipt_tx_sig_onchain_sig_diverged")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        })
        .unwrap_or_else(|| {
            panic!("expected receipt_tx_sig_onchain_sig_diverged drift for {receipt_id}: {drift:?}")
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("tx_sig=sig-from-annotate")
            && message.contains("onchain_sig=sig-rewritten-out-of-band"),
        "drift message should record both signature values: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("annotate_receipt")),
        "diverged drift repair string should name annotate_receipt: {row:?}"
    );
    assert!(
        !drift["drift"].as_array().unwrap().iter().any(|item| {
            item["kind"].as_str() == Some("receipt_confirmed_without_chain")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        }),
        "confirmed_at=None must not co-fire receipt_confirmed_without_chain: {drift:?}"
    );
    assert!(
        !drift["drift"].as_array().unwrap().iter().any(|item| {
            item["kind"].as_str() == Some("receipt_chain_partial")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        }),
        "chain bundle fully unset must not co-fire receipt_chain_partial: {drift:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a timestamp_ms=0 audit event, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_event_timestamp_zero_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let zero_event = AuditEvent {
        id: event_id,
        timestamp_ms: 0,
        issuer: AgentId::new("user@local", [0u8; 32]),
        kind: AuditKind::AuthenticationFailed {
            transport: "ipc".into(),
            reason: "fixture".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(
        audit_file,
        "{}",
        serde_json::to_string(&zero_event).unwrap()
    )
    .expect("append zero-timestamp event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when audit event timestamp is zero: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_event_timestamp_zero")
                && item["id"].as_str() == Some(&event_id.to_string())
        })
        .unwrap_or_else(|| {
            panic!("expected audit_event_timestamp_zero drift for {event_id}: {drift:?}")
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("timestamp_ms = 0"),
        "drift message should name the zero invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("epoch_ms")),
        "zero-timestamp drift repair string should name epoch_ms: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a nil-id audit event, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_event_id_nil_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let nil_event = AuditEvent {
        id: Uuid::nil(),
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [0u8; 32]),
        kind: AuditKind::AuthenticationFailed {
            transport: "ipc".into(),
            reason: "fixture".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&nil_event).unwrap())
        .expect("append nil-id event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when audit event id is nil: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let nil_id_str = Uuid::nil().to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_event_id_nil")
                && item["id"].as_str() == Some(nil_id_str.as_str())
        })
        .unwrap_or_else(|| panic!("expected audit_event_id_nil drift for nil UUID: {drift:?}"));
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("Uuid::new_v4()"),
        "drift message should name the new_v4 invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("Uuid::new_v4()")),
        "nil-id drift repair string should name Uuid::new_v4(): {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a zeroed-issuer-pubkey audit event, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_event_issuer_pubkey_zeroed_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let zeroed = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [0u8; 32]),
        kind: AuditKind::AuthenticationFailed {
            transport: "ipc".into(),
            reason: "fixture".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&zeroed).unwrap())
        .expect("append zeroed-issuer-pubkey event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when audit event issuer pubkey is zeroed: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_event_issuer_pubkey_zeroed")
                && item["id"].as_str() == Some(&event_id.to_string())
        })
        .unwrap_or_else(|| {
            panic!("expected audit_event_issuer_pubkey_zeroed drift for {event_id}: {drift:?}")
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("[0u8; 32]"),
        "drift message should name the zeroed-pubkey invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("LocalIdentity::pubkey_bytes")),
        "zeroed-issuer drift repair string should name the identity source: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a settled_at=0 settlement receipt, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_receipt_settled_at_zero_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let receipts_dir = home.path().join("receipts");
    std::fs::create_dir_all(&receipts_dir).expect("create receipts dir");
    let receipt_id = Uuid::new_v4();
    let zero = SettlementReceipt {
        id: receipt_id,
        payer: AgentId::new("user@local", [0u8; 32]),
        resource: ResourceKind::Compute,
        memory_record_id: None,
        credits_consumed: 1,
        settled_at: 0,
        chain: None,
        cluster: None,
        batch_id: None,
        merkle_root: None,
        tx_sig: None,
        slot: None,
        confirmed_at: None,
        onchain_sig: None,
    };
    let receipts_path = receipts_dir.join("working.jsonl");
    use std::io::Write as _;
    let mut receipts = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&receipts_path)
        .expect("open receipts/working.jsonl for append");
    writeln!(receipts, "{}", serde_json::to_string(&zero).unwrap())
        .expect("append zero-settled-at receipt");
    drop(receipts);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when receipt settled_at is zero: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("receipt_settled_at_zero")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        })
        .unwrap_or_else(|| {
            panic!("expected receipt_settled_at_zero drift for {receipt_id}: {drift:?}")
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("settled_at = 0"),
        "drift message should name the zero invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("epoch_ms")),
        "zero-settled-at drift repair string should name epoch_ms: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a nil-id settlement receipt, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_receipt_id_nil_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let receipts_dir = home.path().join("receipts");
    std::fs::create_dir_all(&receipts_dir).expect("create receipts dir");
    let nil = SettlementReceipt {
        id: Uuid::nil(),
        payer: AgentId::new("user@local", [0u8; 32]),
        resource: ResourceKind::Compute,
        memory_record_id: None,
        credits_consumed: 1,
        settled_at: 1_700_000_000_000,
        chain: None,
        cluster: None,
        batch_id: None,
        merkle_root: None,
        tx_sig: None,
        slot: None,
        confirmed_at: None,
        onchain_sig: None,
    };
    let receipts_path = receipts_dir.join("working.jsonl");
    use std::io::Write as _;
    let mut receipts = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&receipts_path)
        .expect("open receipts/working.jsonl for append");
    writeln!(receipts, "{}", serde_json::to_string(&nil).unwrap()).expect("append nil-id receipt");
    drop(receipts);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when receipt id is nil: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let nil_id_str = Uuid::nil().to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("receipt_id_nil")
                && item["id"].as_str() == Some(nil_id_str.as_str())
        })
        .unwrap_or_else(|| panic!("expected receipt_id_nil drift for nil UUID: {drift:?}"));
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("Uuid::new_v4()"),
        "drift message should name the new_v4 invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("Uuid::new_v4()")),
        "nil-id drift repair string should name Uuid::new_v4(): {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a zeroed-payer-pubkey settlement receipt, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_receipt_payer_pubkey_zeroed_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let receipts_dir = home.path().join("receipts");
    std::fs::create_dir_all(&receipts_dir).expect("create receipts dir");
    let receipt_id = Uuid::new_v4();
    let zeroed = SettlementReceipt {
        id: receipt_id,
        payer: AgentId::new("user@local", [0u8; 32]),
        resource: ResourceKind::Compute,
        memory_record_id: None,
        credits_consumed: 1,
        settled_at: 1_700_000_000_000,
        chain: None,
        cluster: None,
        batch_id: None,
        merkle_root: None,
        tx_sig: None,
        slot: None,
        confirmed_at: None,
        onchain_sig: None,
    };
    let receipts_path = receipts_dir.join("working.jsonl");
    use std::io::Write as _;
    let mut receipts = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&receipts_path)
        .expect("open receipts/working.jsonl for append");
    writeln!(receipts, "{}", serde_json::to_string(&zeroed).unwrap())
        .expect("append zeroed-payer-pubkey receipt");
    drop(receipts);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when receipt payer pubkey is zeroed: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("receipt_payer_pubkey_zeroed")
                && item["id"].as_str() == Some(&receipt_id.to_string())
        })
        .unwrap_or_else(|| {
            panic!("expected receipt_payer_pubkey_zeroed drift for {receipt_id}: {drift:?}")
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("[0u8; 32]"),
        "drift message should name the zeroed-pubkey invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("LocalIdentity::pubkey_bytes")),
        "zeroed-payer drift repair string should name the identity source: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, inserts a nil-id memory record via SqliteStore, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_memory_record_id_nil_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let store = SqliteStore::open(&home.path().join("memory.db")).expect("open memory db");
    let nil_record = MemoryRecord {
        id: Uuid::nil(),
        tier: MemoryTier::Working,
        owner: AgentId::new("user@local", [0u8; 32]),
        text: "nil-id fixture".into(),
        embedding: vec![0.5; 8],
        metadata: serde_json::json!({}),
        created_at: 1_700_000_000_000,
        parent: None,
    };
    store
        .put(nil_record.clone())
        .await
        .expect("inject nil-id record");
    let reread = store
        .get(Uuid::nil())
        .await
        .expect("reload nil-id record")
        .expect("nil-id record persists");
    assert_eq!(
        reread.id,
        Uuid::nil(),
        "SqliteStore must persist the nil id through put/get; otherwise the live coverage is meaningless"
    );
    drop(store);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when memory record id is nil: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let nil_id_str = Uuid::nil().to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("memory_record_id_nil")
                && item["id"].as_str() == Some(nil_id_str.as_str())
        })
        .unwrap_or_else(|| panic!("expected memory_record_id_nil drift for nil UUID: {drift:?}"));
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("Uuid::new_v4()"),
        "drift message should name the new_v4 invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("Uuid::new_v4()")),
        "memory nil-id drift repair string should name Uuid::new_v4(): {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, inserts a created_at=0 memory record via SqliteStore, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_memory_record_created_at_zero_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let store = SqliteStore::open(&home.path().join("memory.db")).expect("open memory db");
    let memory_id = Uuid::new_v4();
    let zero_record = MemoryRecord {
        id: memory_id,
        tier: MemoryTier::Working,
        owner: AgentId::new("user@local", [0u8; 32]),
        text: "zero-created-at fixture".into(),
        embedding: vec![0.5; 8],
        metadata: serde_json::json!({}),
        created_at: 0,
        parent: None,
    };
    store
        .put(zero_record.clone())
        .await
        .expect("inject zero-created-at record");
    let reread = store
        .get(memory_id)
        .await
        .expect("reload record")
        .expect("record persists");
    assert_eq!(
        reread.created_at, 0,
        "SqliteStore must persist created_at == 0 through put/get; otherwise the live coverage is meaningless"
    );
    drop(store);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when memory record created_at is zero: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("memory_record_created_at_zero")
                && item["id"].as_str() == Some(&memory_id.to_string())
        })
        .unwrap_or_else(|| {
            panic!("expected memory_record_created_at_zero drift for {memory_id}: {drift:?}")
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("created_at = 0"),
        "drift message should name the zero invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("epoch_ms")),
        "zero-created-at drift repair string should name epoch_ms: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, injects a zeroed-owner-pubkey memory record via SqliteStore, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_memory_record_owner_pubkey_zeroed_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let store = SqliteStore::open(&home.path().join("memory.db")).expect("open memory db");
    let memory_id = Uuid::new_v4();
    let zeroed_owner_record = MemoryRecord {
        id: memory_id,
        tier: MemoryTier::Working,
        owner: AgentId::new("user@local", [0u8; 32]),
        text: "zeroed-owner fixture".into(),
        embedding: vec![0.5; 8],
        metadata: serde_json::json!({}),
        created_at: 1_700_000_000_000,
        parent: None,
    };
    store
        .put(zeroed_owner_record.clone())
        .await
        .expect("inject zeroed-owner-pubkey record");
    let reread = store
        .get(memory_id)
        .await
        .expect("reload record")
        .expect("record persists");
    assert_eq!(
        reread.owner.pubkey, [0u8; 32],
        "SqliteStore must persist owner.pubkey == [0u8; 32] through put/get; otherwise the live coverage is meaningless"
    );
    drop(store);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when memory record owner pubkey is zeroed: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("memory_record_owner_pubkey_zeroed")
                && item["id"].as_str() == Some(&memory_id.to_string())
        })
        .unwrap_or_else(|| {
            panic!("expected memory_record_owner_pubkey_zeroed drift for {memory_id}: {drift:?}")
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("[0u8; 32]"),
        "drift message should name the zeroed-pubkey invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("LocalIdentity::pubkey_bytes")),
        "zeroed-owner drift repair string should name the identity source: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, grants memory.read then rewrites granted.jsonl to set action=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_capability_action_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = run_cli_raw(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.read"],
    )
    .await;
    let _ = child.kill().await;

    let granted_path = home.path().join("capabilities").join("granted.jsonl");
    let granted_text = std::fs::read_to_string(&granted_path).expect("read granted.jsonl");
    let trimmed = granted_text.trim();
    assert!(
        !trimmed.is_empty(),
        "granted.jsonl must hold the daemon-granted memory.read capability"
    );
    let mut row: Value = serde_json::from_str(trimmed).expect("granted.jsonl row must be JSON");
    row["capability"]["action"] = Value::String(String::new());
    let signature_b58 = row["signature"]
        .as_str()
        .expect("signature must be a string in granted.jsonl row")
        .to_string();
    std::fs::write(&granted_path, format!("{}\n", row)).expect("rewrite granted.jsonl");

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when capability action is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("capability_action_empty")
                && item["id"].as_str() == Some(&signature_b58)
        })
        .unwrap_or_else(|| {
            panic!("expected capability_action_empty drift for {signature_b58}: {drift:?}")
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("empty action"),
        "drift message should name the empty-action invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("grant_capability")),
        "empty-action drift repair string should name the canonical grant source: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, grants memory.read then rewrites granted.jsonl to zero capability.subject.pubkey, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_capability_subject_pubkey_zeroed_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = run_cli_raw(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.read"],
    )
    .await;
    let _ = child.kill().await;

    let granted_path = home.path().join("capabilities").join("granted.jsonl");
    let granted_text = std::fs::read_to_string(&granted_path).expect("read granted.jsonl");
    let trimmed = granted_text.trim();
    assert!(
        !trimmed.is_empty(),
        "granted.jsonl must hold the daemon-granted memory.read capability"
    );
    let mut row: Value = serde_json::from_str(trimmed).expect("granted.jsonl row must be JSON");
    let zeroed_pubkey_b58 = bs58::encode([0u8; 32]).into_string();
    row["capability"]["subject"]["pubkey"] = Value::String(zeroed_pubkey_b58);
    let signature_b58 = row["signature"]
        .as_str()
        .expect("signature must be a string in granted.jsonl row")
        .to_string();
    std::fs::write(&granted_path, format!("{}\n", row)).expect("rewrite granted.jsonl");

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when capability subject pubkey is zeroed: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("capability_subject_pubkey_zeroed")
                && item["id"].as_str() == Some(&signature_b58)
        })
        .unwrap_or_else(|| {
            panic!("expected capability_subject_pubkey_zeroed drift for {signature_b58}: {drift:?}")
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("[0u8; 32]"),
        "drift message should name the zeroed-pubkey invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("LocalIdentity::pubkey_bytes")),
        "zeroed-subject drift repair string should name the identity source: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, grants memory.read then rewrites granted.jsonl to zero capability.granted_by.pubkey, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_capability_grantor_pubkey_zeroed_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = run_cli_raw(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.read"],
    )
    .await;
    let _ = child.kill().await;

    let granted_path = home.path().join("capabilities").join("granted.jsonl");
    let granted_text = std::fs::read_to_string(&granted_path).expect("read granted.jsonl");
    let trimmed = granted_text.trim();
    assert!(
        !trimmed.is_empty(),
        "granted.jsonl must hold the daemon-granted memory.read capability"
    );
    let mut row: Value = serde_json::from_str(trimmed).expect("granted.jsonl row must be JSON");
    let zeroed_pubkey_b58 = bs58::encode([0u8; 32]).into_string();
    row["capability"]["granted_by"]["pubkey"] = Value::String(zeroed_pubkey_b58);
    let signature_b58 = row["signature"]
        .as_str()
        .expect("signature must be a string in granted.jsonl row")
        .to_string();
    std::fs::write(&granted_path, format!("{}\n", row)).expect("rewrite granted.jsonl");

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when capability grantor pubkey is zeroed: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("capability_grantor_pubkey_zeroed")
                && item["id"].as_str() == Some(&signature_b58)
        })
        .unwrap_or_else(|| {
            panic!("expected capability_grantor_pubkey_zeroed drift for {signature_b58}: {drift:?}")
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("[0u8; 32]"),
        "drift message should name the zeroed-pubkey invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("self.identity.agent_id")),
        "zeroed-grantor drift repair string should name the trust-root source: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, grants memory.read then rewrites granted.jsonl to set expires_at=0, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_capability_expires_at_zero_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = run_cli_raw(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.read"],
    )
    .await;
    let _ = child.kill().await;

    let granted_path = home.path().join("capabilities").join("granted.jsonl");
    let granted_text = std::fs::read_to_string(&granted_path).expect("read granted.jsonl");
    let trimmed = granted_text.trim();
    assert!(
        !trimmed.is_empty(),
        "granted.jsonl must hold the daemon-granted memory.read capability"
    );
    let mut row: Value = serde_json::from_str(trimmed).expect("granted.jsonl row must be JSON");
    row["capability"]["expires_at"] = Value::from(0_u64);
    let signature_b58 = row["signature"]
        .as_str()
        .expect("signature must be a string in granted.jsonl row")
        .to_string();
    std::fs::write(&granted_path, format!("{}\n", row)).expect("rewrite granted.jsonl");

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when capability expires_at is zero: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("capability_expires_at_zero")
                && item["id"].as_str() == Some(&signature_b58)
        })
        .unwrap_or_else(|| {
            panic!("expected capability_expires_at_zero drift for {signature_b58}: {drift:?}")
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("expires_at = Some(0)"),
        "drift message should name the zero-expiry invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("revoke_capability")),
        "zero-expiry drift repair string should name revoke_capability: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, grants memory.read then rewrites granted.jsonl signature to bs58([0u8; 64]), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_capability_signature_zeroed_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = run_cli_raw(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.read"],
    )
    .await;
    let _ = child.kill().await;

    let granted_path = home.path().join("capabilities").join("granted.jsonl");
    let granted_text = std::fs::read_to_string(&granted_path).expect("read granted.jsonl");
    let trimmed = granted_text.trim();
    assert!(
        !trimmed.is_empty(),
        "granted.jsonl must hold the daemon-granted memory.read capability"
    );
    let mut row: Value = serde_json::from_str(trimmed).expect("granted.jsonl row must be JSON");
    let zeroed_signature_b58 = bs58::encode([0u8; 64]).into_string();
    row["signature"] = Value::String(zeroed_signature_b58.clone());
    std::fs::write(&granted_path, format!("{}\n", row)).expect("rewrite granted.jsonl");

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when capability signature is zeroed: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("capability_signature_zeroed")
                && item["id"].as_str() == Some(&zeroed_signature_b58)
        })
        .unwrap_or_else(|| {
            panic!(
                "expected capability_signature_zeroed drift for {zeroed_signature_b58}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("[0u8; 64]"),
        "drift message should name the zeroed-signature invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("sign_capability")),
        "zeroed-signature drift repair string should name sign_capability as the canonical source: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, grants memory.read then rewrites granted.jsonl scope to a JSON array, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_capability_scope_non_object_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = run_cli_raw(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.read"],
    )
    .await;
    let _ = child.kill().await;

    let granted_path = home.path().join("capabilities").join("granted.jsonl");
    let granted_text = std::fs::read_to_string(&granted_path).expect("read granted.jsonl");
    let trimmed = granted_text.trim();
    assert!(
        !trimmed.is_empty(),
        "granted.jsonl must hold the daemon-granted memory.read capability"
    );
    let mut row: Value = serde_json::from_str(trimmed).expect("granted.jsonl row must be JSON");
    row["capability"]["scope"] = Value::Array(vec![]);
    let signature_b58 = row["signature"]
        .as_str()
        .expect("signature must be a string in granted.jsonl row")
        .to_string();
    std::fs::write(&granted_path, format!("{}\n", row)).expect("rewrite granted.jsonl");

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when capability scope is non-object: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("capability_scope_non_object")
                && item["id"].as_str() == Some(&signature_b58)
        })
        .unwrap_or_else(|| {
            panic!("expected capability_scope_non_object drift for {signature_b58}: {drift:?}")
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("JSON type array"),
        "drift message should name the JSON shape: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("grant_capability")),
        "non-object-scope drift repair string should name the canonical grant source: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a CapabilityGranted audit event with empty signature_b58, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_capability_granted_signature_b58_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::CapabilityGranted {
            subject_display: "user@local".into(),
            action: "memory.read".into(),
            granted_by_display: "user@local".into(),
            signature_b58: String::new(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-signature CapabilityGranted event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when CapabilityGranted signature_b58 is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_capability_granted_signature_b58_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_capability_granted_signature_b58_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::CapabilityGranted"),
        "drift message should name the CapabilityGranted variant: {message:?}"
    );
    assert!(
        message.contains("signature_b58 = \"\""),
        "drift message should name the empty-signature invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("sign_capability")
                && repair.contains("bs58::encode")),
        "empty-signature CapabilityGranted drift repair string should name sign_capability and bs58::encode: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a CapabilityGranted audit event with subject_display=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_capability_granted_subject_display_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::CapabilityGranted {
            subject_display: String::new(),
            action: "memory.read".into(),
            granted_by_display: "user@local".into(),
            signature_b58: "fixturesig".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-subject_display CapabilityGranted event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when CapabilityGranted subject_display is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_capability_granted_subject_display_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_capability_granted_subject_display_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::CapabilityGranted"),
        "drift message should name the CapabilityGranted variant: {message:?}"
    );
    assert!(
        message.contains("subject_display = \"\""),
        "drift message should name the empty-subject-display invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("peer.display.clone()")
            && repair.contains("validate_agent_id_display")),
        "empty-subject-display CapabilityGranted drift repair string should name peer.display.clone() and validate_agent_id_display: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a CapabilityGranted audit event with granted_by_display=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_capability_granted_granted_by_display_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::CapabilityGranted {
            subject_display: "user@local".into(),
            action: "memory.read".into(),
            granted_by_display: String::new(),
            signature_b58: "fixturesig".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-granted_by_display CapabilityGranted event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when CapabilityGranted granted_by_display is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_capability_granted_granted_by_display_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_capability_granted_granted_by_display_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::CapabilityGranted"),
        "drift message should name the CapabilityGranted variant: {message:?}"
    );
    assert!(
        message.contains("granted_by_display = \"\""),
        "drift message should name the empty-granted-by-display invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("granted_by.display.clone()")
            && repair.contains("validate_agent_id_display")),
        "empty-granted-by-display CapabilityGranted drift repair string should name granted_by.display.clone() and validate_agent_id_display: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a CapabilityRevokeRejected audit event with empty signature_b58, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_capability_revoke_rejected_signature_b58_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::CapabilityRevokeRejected {
            signature_b58: String::new(),
            reason: "peer is not the subject of this capability".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-signature CapabilityRevokeRejected event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when CapabilityRevokeRejected signature_b58 is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_capability_revoke_rejected_signature_b58_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_capability_revoke_rejected_signature_b58_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::CapabilityRevokeRejected"),
        "drift message should name the CapabilityRevokeRejected variant: {message:?}"
    );
    assert!(
        message.contains("signature_b58 = \"\""),
        "drift message should name the empty-signature invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("revoke_capability")
                && repair.contains("bs58::decode")),
        "empty-signature CapabilityRevokeRejected drift repair string should name revoke_capability and bs58::decode: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an IntentDispatched audit event with empty result_hash_hex, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_intent_dispatched_result_hash_hex_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::IntentDispatched {
            intent_id: Uuid::new_v4(),
            intent_text: "drift fixture".into(),
            matched_agent: None,
            result_hash_hex: String::new(),
            status: "ok".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-result-hash IntentDispatched event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when IntentDispatched result_hash_hex is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_intent_dispatched_result_hash_hex_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_intent_dispatched_result_hash_hex_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::IntentDispatched"),
        "drift message should name the IntentDispatched variant: {message:?}"
    );
    assert!(
        message.contains("result_hash_hex = \"\""),
        "drift message should name the empty-hash invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("hash_hex") && repair.contains("result_hash_hex")),
        "empty-result-hash IntentDispatched drift repair string should name hash_hex and result_hash_hex: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an IntentDispatched audit event with intent_id=Uuid::nil(), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_intent_dispatched_intent_id_nil_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::IntentDispatched {
            intent_id: Uuid::nil(),
            intent_text: "drift fixture".into(),
            matched_agent: None,
            result_hash_hex: covenant_audit::hash_hex(b"drift fixture"),
            status: "ok".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append nil-intent-id IntentDispatched event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when IntentDispatched intent_id is nil: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_intent_dispatched_intent_id_nil")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_intent_dispatched_intent_id_nil drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::IntentDispatched"),
        "drift message should name the IntentDispatched variant: {message:?}"
    );
    assert!(
        message.contains("intent_id ="),
        "drift message should name the nil-intent-id invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("Uuid::new_v4") && repair.contains("intent_id")),
        "nil-intent-id IntentDispatched drift repair string should name Uuid::new_v4 and intent_id: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a HermesToolInvoked audit event with intent_id=Uuid::nil(), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_hermes_tool_invoked_intent_id_nil_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::HermesToolInvoked {
            intent_id: Uuid::nil(),
            run_id: "drift-run".into(),
            tool: "tools.echo".into(),
            preview_hash_hex: covenant_audit::hash_hex(b"drift preview"),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append nil-intent-id HermesToolInvoked event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when HermesToolInvoked intent_id is nil: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_hermes_tool_invoked_intent_id_nil")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_hermes_tool_invoked_intent_id_nil drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::HermesToolInvoked"),
        "drift message should name the HermesToolInvoked variant: {message:?}"
    );
    assert!(
        message.contains("intent_id ="),
        "drift message should name the nil-intent-id invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("Uuid::new_v4") && repair.contains("intent_id")),
        "nil-intent-id HermesToolInvoked drift repair string should name Uuid::new_v4 and intent_id: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a HermesToolInvoked audit event with empty preview_hash_hex, and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_hermes_tool_invoked_preview_hash_hex_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::HermesToolInvoked {
            intent_id: Uuid::new_v4(),
            run_id: "drift-run".into(),
            tool: "tools.echo".into(),
            preview_hash_hex: String::new(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-preview-hash HermesToolInvoked event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when HermesToolInvoked preview_hash_hex is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_hermes_tool_invoked_preview_hash_hex_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_hermes_tool_invoked_preview_hash_hex_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::HermesToolInvoked"),
        "drift message should name the HermesToolInvoked variant: {message:?}"
    );
    assert!(
        message.contains("preview_hash_hex = \"\""),
        "drift message should name the empty-hash invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("hash_hex")
                && repair.contains("preview_hash_hex")),
        "empty-preview-hash HermesToolInvoked drift repair string should name hash_hex and preview_hash_hex: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a settlement receipt with memory_record_id=Some(Uuid::nil()), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_receipt_memory_record_id_nil_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let receipts_dir = home.path().join("receipts");
    std::fs::create_dir_all(&receipts_dir).expect("create receipts dir");
    let receipt_id = Uuid::new_v4();
    let receipt = SettlementReceipt {
        id: receipt_id,
        payer: AgentId::new("user@local", [1u8; 32]),
        resource: ResourceKind::Memory,
        memory_record_id: Some(Uuid::nil()),
        credits_consumed: 1,
        settled_at: 1_700_000_000_000,
        chain: None,
        cluster: None,
        batch_id: None,
        merkle_root: None,
        tx_sig: None,
        slot: None,
        confirmed_at: None,
        onchain_sig: None,
    };
    let receipts_path = receipts_dir.join("working.jsonl");
    use std::io::Write as _;
    let mut receipts = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&receipts_path)
        .expect("open receipts/working.jsonl for append");
    writeln!(receipts, "{}", serde_json::to_string(&receipt).unwrap())
        .expect("append nil-memory-record-id receipt");
    drop(receipts);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when receipt memory_record_id is Some(Uuid::nil()): status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let receipt_id_str = receipt_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("receipt_memory_record_id_nil")
                && item["id"].as_str() == Some(receipt_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected receipt_memory_record_id_nil drift for receipt {receipt_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("Some(Uuid::nil())"),
        "drift message should name the Some(Uuid::nil()) invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("Uuid::new_v4()")),
        "nil-memory-record-id drift repair string should name Uuid::new_v4(): {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a HermesToolCompleted audit event with intent_id=Uuid::nil(), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_hermes_tool_completed_intent_id_nil_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::HermesToolCompleted {
            intent_id: Uuid::nil(),
            run_id: "drift-run".into(),
            tool: "tools.echo".into(),
            duration_ms: 17,
            error: false,
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append nil-intent-id HermesToolCompleted event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when HermesToolCompleted intent_id is nil: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_hermes_tool_completed_intent_id_nil")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_hermes_tool_completed_intent_id_nil drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::HermesToolCompleted"),
        "drift message should name the HermesToolCompleted variant: {message:?}"
    );
    assert!(
        message.contains("intent_id ="),
        "drift message should name the nil-intent-id invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("Uuid::new_v4") && repair.contains("intent_id")),
        "nil-intent-id HermesToolCompleted drift repair string should name Uuid::new_v4 and intent_id: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a HermesApprovalRequested audit event with intent_id=Uuid::nil(), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_hermes_approval_requested_intent_id_nil_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::HermesApprovalRequested {
            intent_id: Uuid::nil(),
            run_id: "drift-run".into(),
            choices: vec!["yes".into(), "no".into()],
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append nil-intent-id HermesApprovalRequested event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when HermesApprovalRequested intent_id is nil: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_hermes_approval_requested_intent_id_nil")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_hermes_approval_requested_intent_id_nil drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::HermesApprovalRequested"),
        "drift message should name the HermesApprovalRequested variant: {message:?}"
    );
    assert!(
        message.contains("intent_id ="),
        "drift message should name the nil-intent-id invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("Uuid::new_v4") && repair.contains("intent_id")),
        "nil-intent-id HermesApprovalRequested drift repair string should name Uuid::new_v4 and intent_id: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a HermesApprovalResolved audit event with intent_id=Uuid::nil(), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_hermes_approval_resolved_intent_id_nil_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::HermesApprovalResolved {
            intent_id: Uuid::nil(),
            run_id: "drift-run".into(),
            choice: "yes".into(),
            resolved: 1,
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append nil-intent-id HermesApprovalResolved event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when HermesApprovalResolved intent_id is nil: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_hermes_approval_resolved_intent_id_nil")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_hermes_approval_resolved_intent_id_nil drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::HermesApprovalResolved"),
        "drift message should name the HermesApprovalResolved variant: {message:?}"
    );
    assert!(
        message.contains("intent_id ="),
        "drift message should name the nil-intent-id invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("Uuid::new_v4") && repair.contains("intent_id")),
        "nil-intent-id HermesApprovalResolved drift repair string should name Uuid::new_v4 and intent_id: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a HermesFileWritten audit event with intent_id=Uuid::nil(), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_hermes_file_written_intent_id_nil_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::HermesFileWritten {
            intent_id: Uuid::nil(),
            run_id: "drift-run".into(),
            path: "src/lib.rs".into(),
            bytes: 42,
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append nil-intent-id HermesFileWritten event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when HermesFileWritten intent_id is nil: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_hermes_file_written_intent_id_nil")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_hermes_file_written_intent_id_nil drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::HermesFileWritten"),
        "drift message should name the HermesFileWritten variant: {message:?}"
    );
    assert!(
        message.contains("intent_id ="),
        "drift message should name the nil-intent-id invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("Uuid::new_v4") && repair.contains("intent_id")),
        "nil-intent-id HermesFileWritten drift repair string should name Uuid::new_v4 and intent_id: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an IntentIgnored audit event with intent_id=Uuid::nil(), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_intent_ignored_intent_id_nil_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::IntentIgnored {
            intent_id: Uuid::nil(),
            intent_text: "drift intent".into(),
            matched_pattern: "skip:drift".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append nil-intent-id IntentIgnored event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when IntentIgnored intent_id is nil: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_intent_ignored_intent_id_nil")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_intent_ignored_intent_id_nil drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::IntentIgnored"),
        "drift message should name the IntentIgnored variant: {message:?}"
    );
    assert!(
        message.contains("intent_id ="),
        "drift message should name the nil-intent-id invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("Uuid::new_v4") && repair.contains("intent_id")),
        "nil-intent-id IntentIgnored drift repair string should name Uuid::new_v4 and intent_id: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a BudgetExhausted audit event with intent_id=Uuid::nil(), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_budget_exhausted_intent_id_nil_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::BudgetExhausted {
            agent_display: "research@local".into(),
            intent_id: Uuid::nil(),
            intent_text: "drift intent".into(),
            requested: 10,
            tokens_remaining: 0,
            refill_eta_ms: 1_000,
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append nil-intent-id BudgetExhausted event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when BudgetExhausted intent_id is nil: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_budget_exhausted_intent_id_nil")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_budget_exhausted_intent_id_nil drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::BudgetExhausted"),
        "drift message should name the BudgetExhausted variant: {message:?}"
    );
    assert!(
        message.contains("intent_id ="),
        "drift message should name the nil-intent-id invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("Uuid::new_v4") && repair.contains("intent_id")),
        "nil-intent-id BudgetExhausted drift repair string should name Uuid::new_v4 and intent_id: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a BudgetPreempted audit event with intent_id=Uuid::nil(), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_budget_preempted_intent_id_nil_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::BudgetPreempted {
            agent_display: "research@local".into(),
            intent_id: Uuid::nil(),
            reason: "budget exhausted mid-step".into(),
            signal_sent: "sigterm".into(),
            exit_code: Some(143),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append nil-intent-id BudgetPreempted event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when BudgetPreempted intent_id is nil: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_budget_preempted_intent_id_nil")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_budget_preempted_intent_id_nil drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::BudgetPreempted"),
        "drift message should name the BudgetPreempted variant: {message:?}"
    );
    assert!(
        message.contains("intent_id ="),
        "drift message should name the nil-intent-id invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("Uuid::new_v4") && repair.contains("intent_id")),
        "nil-intent-id BudgetPreempted drift repair string should name Uuid::new_v4 and intent_id: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a BudgetPreemptFailed audit event with intent_id=Uuid::nil(), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_budget_preempt_failed_intent_id_nil_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::BudgetPreemptFailed {
            agent_display: "research@local".into(),
            intent_id: Uuid::nil(),
            reason: "signal-send rejected".into(),
            errno: 1,
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append nil-intent-id BudgetPreemptFailed event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when BudgetPreemptFailed intent_id is nil: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_budget_preempt_failed_intent_id_nil")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_budget_preempt_failed_intent_id_nil drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::BudgetPreemptFailed"),
        "drift message should name the BudgetPreemptFailed variant: {message:?}"
    );
    assert!(
        message.contains("intent_id ="),
        "drift message should name the nil-intent-id invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("Uuid::new_v4") && repair.contains("intent_id")),
        "nil-intent-id BudgetPreemptFailed drift repair string should name Uuid::new_v4 and intent_id: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a BudgetUnseeded audit event with intent_id=Uuid::nil(), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_budget_unseeded_intent_id_nil_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::BudgetUnseeded {
            agent_display: "research@local".into(),
            intent_id: Uuid::nil(),
            requested: 10,
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append nil-intent-id BudgetUnseeded event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when BudgetUnseeded intent_id is nil: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_budget_unseeded_intent_id_nil")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_budget_unseeded_intent_id_nil drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::BudgetUnseeded"),
        "drift message should name the BudgetUnseeded variant: {message:?}"
    );
    assert!(
        message.contains("intent_id ="),
        "drift message should name the nil-intent-id invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("Uuid::new_v4") && repair.contains("intent_id")),
        "nil-intent-id BudgetUnseeded drift repair string should name Uuid::new_v4 and intent_id: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a MemoryRepairApplied audit event with memory_id=Uuid::nil(), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_memory_repair_applied_memory_id_nil_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::MemoryRepairApplied {
            memory_id: Uuid::nil(),
            action: "delete".into(),
            mode: "exact".into(),
            changed: true,
            reason: "drift repair".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append nil-memory-id MemoryRepairApplied event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when MemoryRepairApplied memory_id is nil: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_memory_repair_applied_memory_id_nil")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_memory_repair_applied_memory_id_nil drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::MemoryRepairApplied"),
        "drift message should name the MemoryRepairApplied variant: {message:?}"
    );
    assert!(
        message.contains("memory_id ="),
        "drift message should name the nil-memory-id invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("Uuid::new_v4") && repair.contains("memory_id")),
        "nil-memory-id MemoryRepairApplied drift repair string should name Uuid::new_v4 and memory_id: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an ExternalPaymentSettled audit event with receipt_id=Uuid::nil(), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_external_payment_settled_receipt_id_nil_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::ExternalPaymentSettled {
            provider: "hyre".into(),
            endpoint: "https://api.hyre.example/v1/data".into(),
            network: "base-sepolia".into(),
            asset: "USDC".into(),
            amount: "10000".into(),
            receipt_id: Uuid::nil(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append nil-receipt-id ExternalPaymentSettled event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when ExternalPaymentSettled receipt_id is nil: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_external_payment_settled_receipt_id_nil")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_external_payment_settled_receipt_id_nil drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::ExternalPaymentSettled"),
        "drift message should name the ExternalPaymentSettled variant: {message:?}"
    );
    assert!(
        message.contains("receipt_id ="),
        "drift message should name the nil-receipt-id invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("Uuid::new_v4") && repair.contains("receipt_id")),
        "nil-receipt-id ExternalPaymentSettled drift repair string should name Uuid::new_v4 and receipt_id: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an AuthenticationFailed audit event with transport=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_authentication_failed_transport_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::AuthenticationFailed {
            transport: String::new(),
            reason: "unknown or revoked token".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-transport AuthenticationFailed event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when AuthenticationFailed transport is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_authentication_failed_transport_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_authentication_failed_transport_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::AuthenticationFailed"),
        "drift message should name the AuthenticationFailed variant: {message:?}"
    );
    assert!(
        message.contains("transport = \"\""),
        "drift message should name the empty-transport invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("record_auth_failure") && repair.contains("ipc")),
        "empty-transport AuthenticationFailed drift repair string should name record_auth_failure and the ipc literal: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a PeerRevoked audit event with peer_pubkey_b58=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_peer_revoked_peer_pubkey_b58_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::PeerRevoked {
            peer_display: "guest@local".into(),
            peer_pubkey_b58: String::new(),
            token_prefix: "abcdef".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-peer_pubkey_b58 PeerRevoked event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when PeerRevoked peer_pubkey_b58 is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_peer_revoked_peer_pubkey_b58_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_peer_revoked_peer_pubkey_b58_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::PeerRevoked"),
        "drift message should name the PeerRevoked variant: {message:?}"
    );
    assert!(
        message.contains("peer_pubkey_b58 = \"\""),
        "drift message should name the empty-peer_pubkey_b58 invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("bs58::encode")
                && repair.contains("agent_id.pubkey")),
        "empty-peer_pubkey_b58 PeerRevoked drift repair string should name bs58::encode and agent_id.pubkey: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a PeerRevoked audit event with token_prefix=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_peer_revoked_token_prefix_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::PeerRevoked {
            peer_display: "guest@local".into(),
            peer_pubkey_b58: "guestpubkeyb58".into(),
            token_prefix: String::new(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-token_prefix PeerRevoked event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when PeerRevoked token_prefix is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_peer_revoked_token_prefix_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_peer_revoked_token_prefix_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::PeerRevoked"),
        "drift message should name the PeerRevoked variant: {message:?}"
    );
    assert!(
        message.contains("token_prefix = \"\""),
        "drift message should name the empty-token_prefix invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("token_b58_prefix")
                && repair.contains("PeerToken")),
        "empty-token_prefix PeerRevoked drift repair string should name token_b58_prefix and PeerToken: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a PeerRevoked audit event with peer_display=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_peer_revoked_peer_display_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::PeerRevoked {
            peer_display: String::new(),
            peer_pubkey_b58: "guestpubkeyb58".into(),
            token_prefix: "abc123".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-peer_display PeerRevoked event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when PeerRevoked peer_display is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_peer_revoked_peer_display_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_peer_revoked_peer_display_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::PeerRevoked"),
        "drift message should name the PeerRevoked variant: {message:?}"
    );
    assert!(
        message.contains("peer_display = \"\""),
        "drift message should name the empty-peer-display invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("summary.agent_id.display.clone()")
            && repair.contains("validate_agent_id_display")),
        "empty-peer-display PeerRevoked drift repair string should name summary.agent_id.display.clone() and validate_agent_id_display: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an OperatorTokenRotationRejected audit event with peer_pubkey_b58=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_operator_token_rotation_rejected_peer_pubkey_b58_empty_drift(
) {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::OperatorTokenRotationRejected {
            peer_display: "guest@local".into(),
            peer_pubkey_b58: String::new(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-peer_pubkey_b58 OperatorTokenRotationRejected event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when OperatorTokenRotationRejected peer_pubkey_b58 is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str()
                == Some("audit_operator_token_rotation_rejected_peer_pubkey_b58_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_operator_token_rotation_rejected_peer_pubkey_b58_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::OperatorTokenRotationRejected"),
        "drift message should name the OperatorTokenRotationRejected variant: {message:?}"
    );
    assert!(
        message.contains("peer_pubkey_b58 = \"\""),
        "drift message should name the empty-peer_pubkey_b58 invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("bs58::encode") && repair.contains("peer.pubkey")),
        "empty-peer_pubkey_b58 OperatorTokenRotationRejected drift repair string should name bs58::encode and peer.pubkey: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an OperatorTokenRotationRejected audit event with peer_display=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_operator_token_rotation_rejected_peer_display_empty_drift(
) {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::OperatorTokenRotationRejected {
            peer_display: String::new(),
            peer_pubkey_b58: "guestpubkeyb58".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-peer_display OperatorTokenRotationRejected event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when OperatorTokenRotationRejected peer_display is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str()
                == Some("audit_operator_token_rotation_rejected_peer_display_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_operator_token_rotation_rejected_peer_display_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::OperatorTokenRotationRejected"),
        "drift message should name the OperatorTokenRotationRejected variant: {message:?}"
    );
    assert!(
        message.contains("peer_display = \"\""),
        "drift message should name the empty-peer-display invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("peer.display.clone()")
            && repair.contains("validate_agent_id_display")),
        "empty-peer-display OperatorTokenRotationRejected drift repair string should name peer.display.clone() and validate_agent_id_display: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an OperatorTokenRotated audit event with old_token_prefix=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_operator_token_rotated_old_token_prefix_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::OperatorTokenRotated {
            peer_display: "operator@local".into(),
            old_token_prefix: String::new(),
            new_token_prefix: "abcdef".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-old_token_prefix OperatorTokenRotated event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when OperatorTokenRotated old_token_prefix is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_operator_token_rotated_old_token_prefix_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_operator_token_rotated_old_token_prefix_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::OperatorTokenRotated"),
        "drift message should name the OperatorTokenRotated variant: {message:?}"
    );
    assert!(
        message.contains("old_token_prefix = \"\""),
        "drift message should name the empty-old_token_prefix invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("token_b58_prefix")
            && repair.contains("read_operator_token_b58")),
        "empty-old_token_prefix OperatorTokenRotated drift repair string should name token_b58_prefix and read_operator_token_b58: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an OperatorTokenRotated audit event with new_token_prefix=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_operator_token_rotated_new_token_prefix_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::OperatorTokenRotated {
            peer_display: "operator@local".into(),
            old_token_prefix: "abcdef".into(),
            new_token_prefix: String::new(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-new_token_prefix OperatorTokenRotated event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when OperatorTokenRotated new_token_prefix is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_operator_token_rotated_new_token_prefix_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_operator_token_rotated_new_token_prefix_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::OperatorTokenRotated"),
        "drift message should name the OperatorTokenRotated variant: {message:?}"
    );
    assert!(
        message.contains("new_token_prefix = \"\""),
        "drift message should name the empty-new_token_prefix invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("token_b58_prefix")
            && repair.contains("PeerToken::generate")),
        "empty-new_token_prefix OperatorTokenRotated drift repair string should name token_b58_prefix and PeerToken::generate: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an OperatorTokenRotated audit event with peer_display=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_operator_token_rotated_peer_display_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::OperatorTokenRotated {
            peer_display: String::new(),
            old_token_prefix: "abcdef".into(),
            new_token_prefix: "123456".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-peer_display OperatorTokenRotated event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when OperatorTokenRotated peer_display is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_operator_token_rotated_peer_display_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_operator_token_rotated_peer_display_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::OperatorTokenRotated"),
        "drift message should name the OperatorTokenRotated variant: {message:?}"
    );
    assert!(
        message.contains("peer_display = \"\""),
        "drift message should name the empty-peer-display invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("peer.display.clone()")
            && repair.contains("validate_agent_id_display")),
        "empty-peer-display OperatorTokenRotated drift repair string should name peer.display.clone() and validate_agent_id_display: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an OperatorPeersListRejected audit event with peer_pubkey_b58=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_operator_peers_list_rejected_peer_pubkey_b58_empty_drift(
) {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::OperatorPeersListRejected {
            peer_display: "guest@local".into(),
            peer_pubkey_b58: String::new(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-peer_pubkey_b58 OperatorPeersListRejected event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when OperatorPeersListRejected peer_pubkey_b58 is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str()
                == Some("audit_operator_peers_list_rejected_peer_pubkey_b58_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_operator_peers_list_rejected_peer_pubkey_b58_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::OperatorPeersListRejected"),
        "drift message should name the OperatorPeersListRejected variant: {message:?}"
    );
    assert!(
        message.contains("peer_pubkey_b58 = \"\""),
        "drift message should name the empty-peer_pubkey_b58 invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("bs58::encode") && repair.contains("peer.pubkey")),
        "empty-peer_pubkey_b58 OperatorPeersListRejected drift repair string should name bs58::encode and peer.pubkey: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an OperatorPeersListRejected audit event with peer_display=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_operator_peers_list_rejected_peer_display_empty_drift()
{
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::OperatorPeersListRejected {
            peer_display: String::new(),
            peer_pubkey_b58: "guestpubkeyb58".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-peer_display OperatorPeersListRejected event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when OperatorPeersListRejected peer_display is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_operator_peers_list_rejected_peer_display_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_operator_peers_list_rejected_peer_display_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::OperatorPeersListRejected"),
        "drift message should name the OperatorPeersListRejected variant: {message:?}"
    );
    assert!(
        message.contains("peer_display = \"\""),
        "drift message should name the empty-peer-display invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("peer.display.clone()")
            && repair.contains("validate_agent_id_display")),
        "empty-peer-display OperatorPeersListRejected drift repair string should name peer.display.clone() and validate_agent_id_display: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an OperatorPeerRevokeRejected audit event with peer_pubkey_b58=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_operator_peer_revoke_rejected_peer_pubkey_b58_empty_drift(
) {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::OperatorPeerRevokeRejected {
            peer_display: "guest@local".into(),
            peer_pubkey_b58: String::new(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-peer_pubkey_b58 OperatorPeerRevokeRejected event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when OperatorPeerRevokeRejected peer_pubkey_b58 is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str()
                == Some("audit_operator_peer_revoke_rejected_peer_pubkey_b58_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_operator_peer_revoke_rejected_peer_pubkey_b58_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::OperatorPeerRevokeRejected"),
        "drift message should name the OperatorPeerRevokeRejected variant: {message:?}"
    );
    assert!(
        message.contains("peer_pubkey_b58 = \"\""),
        "drift message should name the empty-peer_pubkey_b58 invariant: {message:?}"
    );
    assert!(
        row["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("bs58::encode") && repair.contains("peer.pubkey")),
        "empty-peer_pubkey_b58 OperatorPeerRevokeRejected drift repair string should name bs58::encode and peer.pubkey: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an OperatorPeerRevokeRejected audit event with peer_display=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_operator_peer_revoke_rejected_peer_display_empty_drift()
{
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::OperatorPeerRevokeRejected {
            peer_display: String::new(),
            peer_pubkey_b58: "guestpubkeyb58".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-peer_display OperatorPeerRevokeRejected event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when OperatorPeerRevokeRejected peer_display is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_operator_peer_revoke_rejected_peer_display_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_operator_peer_revoke_rejected_peer_display_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::OperatorPeerRevokeRejected"),
        "drift message should name the OperatorPeerRevokeRejected variant: {message:?}"
    );
    assert!(
        message.contains("peer_display = \"\""),
        "drift message should name the empty-peer-display invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("peer.display.clone()")
            && repair.contains("validate_agent_id_display")),
        "empty-peer-display OperatorPeerRevokeRejected drift repair string should name peer.display.clone() and validate_agent_id_display: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a PeerSelfRevokeBlocked audit event with peer_pubkey_b58=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_peer_self_revoke_blocked_peer_pubkey_b58_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::PeerSelfRevokeBlocked {
            peer_display: "operator@local".into(),
            peer_pubkey_b58: String::new(),
            token_prefix: "abcdef".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-peer_pubkey_b58 PeerSelfRevokeBlocked event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when PeerSelfRevokeBlocked peer_pubkey_b58 is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_peer_self_revoke_blocked_peer_pubkey_b58_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_peer_self_revoke_blocked_peer_pubkey_b58_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::PeerSelfRevokeBlocked"),
        "drift message should name the PeerSelfRevokeBlocked variant: {message:?}"
    );
    assert!(
        message.contains("peer_pubkey_b58 = \"\""),
        "drift message should name the empty-peer_pubkey_b58 invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("bs58::encode")
            && repair.contains("summary.agent_id.pubkey")),
        "empty-peer_pubkey_b58 PeerSelfRevokeBlocked drift repair string should name bs58::encode and summary.agent_id.pubkey: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a PeerSelfRevokeBlocked audit event with token_prefix=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_peer_self_revoke_blocked_token_prefix_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::PeerSelfRevokeBlocked {
            peer_display: "operator@local".into(),
            peer_pubkey_b58: "11111111111111111111111111111111".into(),
            token_prefix: String::new(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-token_prefix PeerSelfRevokeBlocked event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when PeerSelfRevokeBlocked token_prefix is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_peer_self_revoke_blocked_token_prefix_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_peer_self_revoke_blocked_token_prefix_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::PeerSelfRevokeBlocked"),
        "drift message should name the PeerSelfRevokeBlocked variant: {message:?}"
    );
    assert!(
        message.contains("token_prefix = \"\""),
        "drift message should name the empty-token_prefix invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("token_b58_prefix")
            && repair.contains("summary.token_prefix")),
        "empty-token_prefix PeerSelfRevokeBlocked drift repair string should name token_b58_prefix and summary.token_prefix: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a PeerSelfRevokeBlocked audit event with peer_display=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_peer_self_revoke_blocked_peer_display_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::PeerSelfRevokeBlocked {
            peer_display: String::new(),
            peer_pubkey_b58: "guestpubkeyb58".into(),
            token_prefix: "abc123".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-peer_display PeerSelfRevokeBlocked event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when PeerSelfRevokeBlocked peer_display is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_peer_self_revoke_blocked_peer_display_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_peer_self_revoke_blocked_peer_display_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::PeerSelfRevokeBlocked"),
        "drift message should name the PeerSelfRevokeBlocked variant: {message:?}"
    );
    assert!(
        message.contains("peer_display = \"\""),
        "drift message should name the empty-peer-display invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("summary.agent_id.display.clone()")
            && repair.contains("validate_agent_id_display")),
        "empty-peer-display PeerSelfRevokeBlocked drift repair string should name summary.agent_id.display.clone() and validate_agent_id_display: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an A2ASenderMismatch audit event with peer_display=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_a2a_sender_mismatch_peer_display_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::A2ASenderMismatch {
            peer_display: String::new(),
            claimed_sender_display: "victim@local".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-peer_display A2ASenderMismatch event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when A2ASenderMismatch peer_display is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_a2a_sender_mismatch_peer_display_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_a2a_sender_mismatch_peer_display_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::A2ASenderMismatch"),
        "drift message should name the A2ASenderMismatch variant: {message:?}"
    );
    assert!(
        message.contains("peer_display = \"\""),
        "drift message should name the empty-peer-display invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("peer.display.clone()")
            && repair.contains("validate_agent_id_display")),
        "empty-peer-display A2ASenderMismatch drift repair string should name peer.display.clone() and validate_agent_id_display: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an A2ASenderMismatch audit event with claimed_sender_display=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_a2a_sender_mismatch_claimed_sender_display_empty_drift()
{
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::A2ASenderMismatch {
            peer_display: "attacker@local".into(),
            claimed_sender_display: String::new(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-claimed_sender_display A2ASenderMismatch event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when A2ASenderMismatch claimed_sender_display is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str()
                == Some("audit_a2a_sender_mismatch_claimed_sender_display_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_a2a_sender_mismatch_claimed_sender_display_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::A2ASenderMismatch"),
        "drift message should name the A2ASenderMismatch variant: {message:?}"
    );
    assert!(
        message.contains("claimed_sender_display = \"\""),
        "drift message should name the empty-claimed-sender-display invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("task.sender.display.clone()")
            && repair.contains("validate_agent_id_display")),
        "empty-claimed-sender-display A2ASenderMismatch drift repair string should name task.sender.display.clone() and validate_agent_id_display: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an A2AResultRejected audit event with reason=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_a2a_result_rejected_reason_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::A2AResultRejected {
            task_id: Uuid::new_v4(),
            reason: String::new(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-reason A2AResultRejected event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when A2AResultRejected reason is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_a2a_result_rejected_reason_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_a2a_result_rejected_reason_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::A2AResultRejected"),
        "drift message should name the A2AResultRejected variant: {message:?}"
    );
    assert!(
        message.contains("reason = \"\""),
        "drift message should name the empty-reason invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("unknown_task")
            && repair.contains("post_a2a_result")),
        "empty-reason A2AResultRejected drift repair string should name unknown_task and post_a2a_result: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an A2ARepairApplied audit event with action=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_a2a_repair_applied_action_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::A2ARepairApplied {
            task_id: Uuid::new_v4(),
            action: String::new(),
            reason: "operator-initiated repair".into(),
            lease_id: Some(Uuid::new_v4()),
            duplicate_risk: Some("idempotent".into()),
            attempt: 1,
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-action A2ARepairApplied event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when A2ARepairApplied action is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_a2a_repair_applied_action_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_a2a_repair_applied_action_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::A2ARepairApplied"),
        "drift message should name the A2ARepairApplied variant: {message:?}"
    );
    assert!(
        message.contains("action = \"\""),
        "drift message should name the empty-action invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("a2a_repair_action")
            && repair.contains("retry_a2a_stale")
            && repair.contains("auto_requeue")),
        "empty-action A2ARepairApplied drift repair string should name a2a_repair_action, retry_a2a_stale, and auto_requeue: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an A2ARepairApplied audit event with reason=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_a2a_repair_applied_reason_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::A2ARepairApplied {
            task_id: Uuid::new_v4(),
            action: "requeue".into(),
            reason: String::new(),
            lease_id: Some(Uuid::new_v4()),
            duplicate_risk: Some("idempotent".into()),
            attempt: 1,
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-reason A2ARepairApplied event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when A2ARepairApplied reason is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_a2a_repair_applied_reason_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_a2a_repair_applied_reason_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::A2ARepairApplied"),
        "drift message should name the A2ARepairApplied variant: {message:?}"
    );
    assert!(
        message.contains("reason = \"\""),
        "drift message should name the empty-reason invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("validate_repair_request")
            && repair.contains("trim().is_empty()")
            && repair.contains("auto-requeue")),
        "empty-reason A2ARepairApplied drift repair string should name validate_repair_request, trim().is_empty(), and the auto-requeue write-site: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an A2ARecipientRejected audit event with action=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_a2a_recipient_rejected_action_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::A2ARecipientRejected {
            sender_display: "sender".into(),
            recipient_display: "recipient".into(),
            action: String::new(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-action A2ARecipientRejected event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when A2ARecipientRejected action is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_a2a_recipient_rejected_action_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_a2a_recipient_rejected_action_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::A2ARecipientRejected"),
        "drift message should name the A2ARecipientRejected variant: {message:?}"
    );
    assert!(
        message.contains("action = \"\""),
        "drift message should name the empty-action invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("scoped_action_alternatives")
            && repair.contains("a2a.recv.")
            && repair.contains("send_a2a_task")),
        "empty-action A2ARecipientRejected drift repair string should name scoped_action_alternatives, the a2a.recv. prefix, and send_a2a_task: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an A2ARecipientRejected audit event with sender_display=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_a2a_recipient_rejected_sender_display_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::A2ARecipientRejected {
            sender_display: String::new(),
            recipient_display: "recipient@local".into(),
            action: "a2a.recv.recipient@local".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-sender_display A2ARecipientRejected event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when A2ARecipientRejected sender_display is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_a2a_recipient_rejected_sender_display_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_a2a_recipient_rejected_sender_display_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::A2ARecipientRejected"),
        "drift message should name the A2ARecipientRejected variant: {message:?}"
    );
    assert!(
        message.contains("sender_display = \"\""),
        "drift message should name the empty-sender-display invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("peer.display.clone()")
            && repair.contains("validate_agent_id_display")),
        "empty-sender-display A2ARecipientRejected drift repair string should name peer.display.clone() and validate_agent_id_display: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an A2ARecipientRejected audit event with recipient_display=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_a2a_recipient_rejected_recipient_display_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::A2ARecipientRejected {
            sender_display: "sender@local".into(),
            recipient_display: String::new(),
            action: "a2a.recv.recipient@local".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-recipient_display A2ARecipientRejected event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when A2ARecipientRejected recipient_display is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_a2a_recipient_rejected_recipient_display_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_a2a_recipient_rejected_recipient_display_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::A2ARecipientRejected"),
        "drift message should name the A2ARecipientRejected variant: {message:?}"
    );
    assert!(
        message.contains("recipient_display = \"\""),
        "drift message should name the empty-recipient-display invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("task.recipient.display.clone()")
            && repair.contains("validate_agent_id_display")),
        "empty-recipient-display A2ARecipientRejected drift repair string should name task.recipient.display.clone() and validate_agent_id_display: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an AuthenticationFailed audit event with reason=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_authentication_failed_reason_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::AuthenticationFailed {
            transport: "ipc".into(),
            reason: String::new(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-reason AuthenticationFailed event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when AuthenticationFailed reason is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_authentication_failed_reason_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_authentication_failed_reason_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::AuthenticationFailed"),
        "drift message should name the AuthenticationFailed variant: {message:?}"
    );
    assert!(
        message.contains("reason = \"\""),
        "drift message should name the empty-reason invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("record_auth_failure")
            && repair.contains("reject()")
            && repair.contains("storage-outage")),
        "empty-reason AuthenticationFailed drift repair string should name record_auth_failure, reject(), and storage-outage: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a CapabilityGrantRejected audit event with reason=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_capability_grant_rejected_reason_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::CapabilityGrantRejected {
            subject_display: "user@local".into(),
            action: "memory.write".into(),
            reason: String::new(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-reason CapabilityGrantRejected event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when CapabilityGrantRejected reason is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_capability_grant_rejected_reason_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_capability_grant_rejected_reason_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::CapabilityGrantRejected"),
        "drift message should name the CapabilityGrantRejected variant: {message:?}"
    );
    assert!(
        message.contains("reason = \"\""),
        "drift message should name the empty-reason invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("grant_capability")
            && repair.contains("validate_scope")
            && repair.contains("PermissionError")),
        "empty-reason CapabilityGrantRejected drift repair string should name grant_capability, validate_scope, and PermissionError: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a CapabilityGrantRejected audit event with subject_display=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_capability_grant_rejected_subject_display_empty_drift()
{
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::CapabilityGrantRejected {
            subject_display: String::new(),
            action: "memory.read".into(),
            reason: "invalid capability scope: foo".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-subject_display CapabilityGrantRejected event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when CapabilityGrantRejected subject_display is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_capability_grant_rejected_subject_display_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_capability_grant_rejected_subject_display_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::CapabilityGrantRejected"),
        "drift message should name the CapabilityGrantRejected variant: {message:?}"
    );
    assert!(
        message.contains("subject_display = \"\""),
        "drift message should name the empty-subject-display invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("peer.display.clone()")
            && repair.contains("validate_agent_id_display")),
        "empty-subject-display CapabilityGrantRejected drift repair string should name peer.display.clone() and validate_agent_id_display: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a CapabilityCheck audit event with agent_id=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_capability_check_agent_id_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::CapabilityCheck {
            agent_id: String::new(),
            required_actions: vec!["tool.call.test".into()],
            missing_actions: vec![],
            passed: true,
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-agent_id CapabilityCheck event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when CapabilityCheck agent_id is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_capability_check_agent_id_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_capability_check_agent_id_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::CapabilityCheck"),
        "drift message should name the CapabilityCheck variant: {message:?}"
    );
    assert!(
        message.contains("agent_id = \"\""),
        "drift message should name the empty-agent_id invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("check_capabilities_any_of")
            && repair.contains("Manifest::validate")),
        "empty-agent_id CapabilityCheck drift repair string should name check_capabilities_any_of and Manifest::validate: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an IntentIgnored audit event with matched_pattern=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_intent_ignored_matched_pattern_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::IntentIgnored {
            intent_id: Uuid::new_v4(),
            intent_text: "test intent".into(),
            matched_pattern: String::new(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-matched-pattern IntentIgnored event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when IntentIgnored matched_pattern is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_intent_ignored_matched_pattern_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_intent_ignored_matched_pattern_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::IntentIgnored"),
        "drift message should name the IntentIgnored variant: {message:?}"
    );
    assert!(
        message.contains("matched_pattern = \"\""),
        "drift message should name the empty-matched_pattern invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("submit_intent")
            && repair.contains("IgnorePattern::parse")
            && repair.contains(".covenantignore")),
        "empty-matched-pattern IntentIgnored drift repair string should name submit_intent, IgnorePattern::parse, and .covenantignore: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a BudgetPreempted audit event with signal_sent=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_budget_preempted_signal_sent_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::BudgetPreempted {
            agent_display: "card@local".into(),
            intent_id: Uuid::new_v4(),
            reason: "budget_overshoot".into(),
            signal_sent: String::new(),
            exit_code: None,
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-signal-sent BudgetPreempted event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when BudgetPreempted signal_sent is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_budget_preempted_signal_sent_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_budget_preempted_signal_sent_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::BudgetPreempted"),
        "drift message should name the BudgetPreempted variant: {message:?}"
    );
    assert!(
        message.contains("signal_sent = \"\""),
        "drift message should name the empty-signal_sent invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("preempt_intent")
            && repair.contains("PreemptOutcome")
            && repair.contains("SIGTERM")
            && repair.contains("SIGKILL")),
        "empty-signal-sent BudgetPreempted drift repair string should name preempt_intent, PreemptOutcome, SIGTERM, and SIGKILL: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an IntentDispatched audit event with status=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_intent_dispatched_status_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::IntentDispatched {
            intent_id: Uuid::new_v4(),
            intent_text: "test intent".into(),
            matched_agent: None,
            result_hash_hex: covenant_audit::hash_hex(b"test"),
            status: String::new(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-status IntentDispatched event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when IntentDispatched status is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_intent_dispatched_status_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_intent_dispatched_status_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::IntentDispatched"),
        "drift message should name the IntentDispatched variant: {message:?}"
    );
    assert!(
        message.contains("status = \"\""),
        "drift message should name the empty-status invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("dispatch_intent")
            && repair.contains("\"ok\"")
            && repair.contains("IntentIgnored")),
        "empty-status IntentDispatched drift repair string should name dispatch_intent, the \"ok\" literal, and IntentIgnored: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an IntentDispatched audit event with matched_agent=Some(\"\"), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_intent_dispatched_matched_agent_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::IntentDispatched {
            intent_id: Uuid::new_v4(),
            intent_text: "test intent".into(),
            matched_agent: Some(String::new()),
            result_hash_hex: covenant_audit::hash_hex(b"test"),
            status: "ok".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append Some-empty matched_agent IntentDispatched event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when IntentDispatched matched_agent is Some(empty): status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_intent_dispatched_matched_agent_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_intent_dispatched_matched_agent_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::IntentDispatched"),
        "drift message should name the IntentDispatched variant: {message:?}"
    );
    assert!(
        message.contains("matched_agent = Some(\"\")"),
        "drift message should name the Some-empty matched_agent invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("card.map(|c| c.id.clone())")
            && repair.contains("Manifest::validate")),
        "Some-empty matched_agent IntentDispatched drift repair string should name card.map(|c| c.id.clone()) and Manifest::validate: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a MemoryRepairApplied audit event with action=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_memory_repair_applied_action_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::MemoryRepairApplied {
            memory_id: Uuid::new_v4(),
            action: String::new(),
            mode: "apply".into(),
            changed: true,
            reason: "test".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-action MemoryRepairApplied event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when MemoryRepairApplied action is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_memory_repair_applied_action_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_memory_repair_applied_action_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::MemoryRepairApplied"),
        "drift message should name the MemoryRepairApplied variant: {message:?}"
    );
    assert!(
        message.contains("action = \"\""),
        "drift message should name the empty-action invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("memory_repair_action")
            && repair.contains("detach_parent")
            && repair.contains("delete_record")
            && repair.contains("backfill_provenance")),
        "empty-action MemoryRepairApplied drift repair string should name memory_repair_action and the three MemoryRepairCommand literals: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a MemoryCompactionApplied audit event with mode=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_memory_compaction_applied_mode_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::MemoryCompactionApplied {
            mode: String::new(),
            changed: true,
            reason: "test".into(),
            deleted: Vec::new(),
            stale_marked: Vec::new(),
            parents_detached: Vec::new(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-mode MemoryCompactionApplied event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when MemoryCompactionApplied mode is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_memory_compaction_applied_mode_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_memory_compaction_applied_mode_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::MemoryCompactionApplied"),
        "drift message should name the MemoryCompactionApplied variant: {message:?}"
    );
    assert!(
        message.contains("mode = \"\""),
        "drift message should name the empty-mode invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("memory_repair_mode")
            && repair.contains("dry_run")
            && repair.contains("apply")),
        "empty-mode MemoryCompactionApplied drift repair string should name memory_repair_mode and the two MemoryRepairMode literals: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a MemoryCompactionApplied audit event with reason=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_memory_compaction_applied_reason_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::MemoryCompactionApplied {
            mode: "apply".into(),
            changed: true,
            reason: String::new(),
            deleted: Vec::new(),
            stale_marked: Vec::new(),
            parents_detached: Vec::new(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-reason MemoryCompactionApplied event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when MemoryCompactionApplied reason is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_memory_compaction_applied_reason_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_memory_compaction_applied_reason_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::MemoryCompactionApplied"),
        "drift message should name the MemoryCompactionApplied variant: {message:?}"
    );
    assert!(
        message.contains("reason = \"\""),
        "drift message should name the empty-reason invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("validate_compaction_request")
            && repair.contains("request.reason.clone()")
            && repair.contains("trim().is_empty()")),
        "empty-reason MemoryCompactionApplied drift repair string should name validate_compaction_request, request.reason.clone(), and trim().is_empty(): {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a MemoryRepairApplied audit event with mode=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_memory_repair_applied_mode_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::MemoryRepairApplied {
            memory_id: Uuid::new_v4(),
            action: "delete_record".into(),
            mode: String::new(),
            changed: true,
            reason: "test".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-mode MemoryRepairApplied event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when MemoryRepairApplied mode is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_memory_repair_applied_mode_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_memory_repair_applied_mode_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::MemoryRepairApplied"),
        "drift message should name the MemoryRepairApplied variant: {message:?}"
    );
    assert!(
        message.contains("mode = \"\""),
        "drift message should name the empty-mode invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("memory_repair_mode")
            && repair.contains("dry_run")
            && repair.contains("apply")),
        "empty-mode MemoryRepairApplied drift repair string should name memory_repair_mode and the two MemoryRepairMode literals: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a MemoryRepairApplied audit event with reason=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_memory_repair_applied_reason_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::MemoryRepairApplied {
            memory_id: Uuid::new_v4(),
            action: "detach_parent".into(),
            mode: "apply".into(),
            changed: true,
            reason: String::new(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-reason MemoryRepairApplied event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when MemoryRepairApplied reason is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_memory_repair_applied_reason_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_memory_repair_applied_reason_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::MemoryRepairApplied"),
        "drift message should name the MemoryRepairApplied variant: {message:?}"
    );
    assert!(
        message.contains("reason = \"\""),
        "drift message should name the empty-reason invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("validate_repair_request")
            && repair.contains("request.reason.clone()")
            && repair.contains("trim().is_empty()")),
        "empty-reason MemoryRepairApplied drift repair string should name validate_repair_request, request.reason.clone(), and trim().is_empty(): {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a BudgetPreempted audit event with reason=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_budget_preempted_reason_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::BudgetPreempted {
            agent_display: "card@local".into(),
            intent_id: Uuid::new_v4(),
            reason: String::new(),
            signal_sent: "SIGTERM".into(),
            exit_code: None,
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-reason BudgetPreempted event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when BudgetPreempted reason is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_budget_preempted_reason_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_budget_preempted_reason_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::BudgetPreempted"),
        "drift message should name the BudgetPreempted variant: {message:?}"
    );
    assert!(
        message.contains("reason = \"\""),
        "drift message should name the empty-reason invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("preempt_intent")
            && repair.contains("run_projection_tick_iteration")
            && repair.contains("budget_overshoot")),
        "empty-reason BudgetPreempted drift repair string should name preempt_intent, run_projection_tick_iteration, and budget_overshoot: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a BudgetPreempted audit event with agent_display=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_budget_preempted_agent_display_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::BudgetPreempted {
            agent_display: String::new(),
            intent_id: Uuid::new_v4(),
            reason: "budget_overshoot".into(),
            signal_sent: "SIGTERM".into(),
            exit_code: None,
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-agent-display BudgetPreempted event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when BudgetPreempted agent_display is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_budget_preempted_agent_display_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_budget_preempted_agent_display_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::BudgetPreempted"),
        "drift message should name the BudgetPreempted variant: {message:?}"
    );
    assert!(
        message.contains("agent_display = \"\""),
        "drift message should name the empty-agent-display invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("entry.agent_id.clone()")
            && repair.contains("Manifest::validate")
            && repair.contains("card.id")),
        "empty-agent-display BudgetPreempted drift repair string should name entry.agent_id.clone(), Manifest::validate, and card.id: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a BudgetPreemptFailed audit event with reason=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_budget_preempt_failed_reason_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::BudgetPreemptFailed {
            agent_display: "card@local".into(),
            intent_id: Uuid::new_v4(),
            reason: String::new(),
            errno: 1,
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-reason BudgetPreemptFailed event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when BudgetPreemptFailed reason is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_budget_preempt_failed_reason_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_budget_preempt_failed_reason_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::BudgetPreemptFailed"),
        "drift message should name the BudgetPreemptFailed variant: {message:?}"
    );
    assert!(
        message.contains("reason = \"\""),
        "drift message should name the empty-reason invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("preempt_intent")
            && repair.contains("PermissionDenied")
            && repair.contains("run_projection_tick_iteration")
            && repair.contains("budget_overshoot")),
        "empty-reason BudgetPreemptFailed drift repair string should name preempt_intent, PermissionDenied, run_projection_tick_iteration, and budget_overshoot: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a BudgetPreemptFailed audit event with agent_display=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_budget_preempt_failed_agent_display_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::BudgetPreemptFailed {
            agent_display: String::new(),
            intent_id: Uuid::new_v4(),
            reason: "budget_overshoot".into(),
            errno: 1,
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-agent-display BudgetPreemptFailed event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when BudgetPreemptFailed agent_display is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_budget_preempt_failed_agent_display_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_budget_preempt_failed_agent_display_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::BudgetPreemptFailed"),
        "drift message should name the BudgetPreemptFailed variant: {message:?}"
    );
    assert!(
        message.contains("agent_display = \"\""),
        "drift message should name the empty-agent-display invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("entry.agent_id.clone()")
            && repair.contains("Manifest::validate")
            && repair.contains("card.id")),
        "empty-agent-display BudgetPreemptFailed drift repair string should name entry.agent_id.clone(), Manifest::validate, and card.id: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a BudgetUnseeded audit event with agent_display=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_budget_unseeded_agent_display_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::BudgetUnseeded {
            agent_display: String::new(),
            intent_id: Uuid::new_v4(),
            requested: 1,
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-agent-display BudgetUnseeded event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when BudgetUnseeded agent_display is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_budget_unseeded_agent_display_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_budget_unseeded_agent_display_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::BudgetUnseeded"),
        "drift message should name the BudgetUnseeded variant: {message:?}"
    );
    assert!(
        message.contains("agent_display = \"\""),
        "drift message should name the empty-agent-display invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("agent_id_for_card")
            && repair.contains("Manifest::validate")
            && repair.contains("@agent")),
        "empty-agent-display BudgetUnseeded drift repair string should name agent_id_for_card, Manifest::validate, and the @agent suffix: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a BudgetExhausted audit event with agent_display=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_budget_exhausted_agent_display_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::BudgetExhausted {
            agent_display: String::new(),
            intent_id: Uuid::new_v4(),
            intent_text: "research the topic".into(),
            requested: 1,
            tokens_remaining: 0,
            refill_eta_ms: 60_000,
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-agent-display BudgetExhausted event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when BudgetExhausted agent_display is empty: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_budget_exhausted_agent_display_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_budget_exhausted_agent_display_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::BudgetExhausted"),
        "drift message should name the BudgetExhausted variant: {message:?}"
    );
    assert!(
        message.contains("agent_display = \"\""),
        "drift message should name the empty-agent-display invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("agent_id_for_card")
            && repair.contains("Manifest::validate")
            && repair.contains("@agent")),
        "empty-agent-display BudgetExhausted drift repair string should name agent_id_for_card, Manifest::validate, and the @agent suffix: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an A2AAutoRetrySchedulerScan audit event with skipped_by_reason key=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_a2a_auto_retry_scheduler_scan_skipped_by_reason_key_empty_drift(
) {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let mut skipped_by_reason = std::collections::BTreeMap::new();
    skipped_by_reason.insert(String::new(), 3u64);
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::A2AAutoRetrySchedulerScan {
            enabled: true,
            considered: 5,
            requeued: 2,
            skipped: 3,
            skipped_by_reason,
            min_lease_age_ms: 60_000,
            max_attempts: 3,
            max_requeues: 10,
            scan_limit: 100,
            error: None,
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-key A2AAutoRetrySchedulerScan event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when A2AAutoRetrySchedulerScan has an empty skipped_by_reason key: status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str()
                == Some("audit_a2a_auto_retry_scheduler_scan_skipped_by_reason_key_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_a2a_auto_retry_scheduler_scan_skipped_by_reason_key_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::A2AAutoRetrySchedulerScan"),
        "drift message should name the A2AAutoRetrySchedulerScan variant: {message:?}"
    );
    assert!(
        message.contains("empty key in skipped_by_reason"),
        "drift message should name the empty-key invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("A2AAutoRetrySkipReason::as_str")
            && repair.contains("missing_lease")
            && repair.contains("max_attempts_reached")
            && repair.contains("capability_scope_mismatch")),
        "empty-key A2AAutoRetrySchedulerScan drift repair string should name as_str and three of the nine skip-reason literals: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an A2AAutoRetrySchedulerScan audit event with error=Some(\"\"), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_a2a_auto_retry_scheduler_scan_error_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::A2AAutoRetrySchedulerScan {
            enabled: true,
            considered: 0,
            requeued: 0,
            skipped: 0,
            skipped_by_reason: std::collections::BTreeMap::new(),
            min_lease_age_ms: 60_000,
            max_attempts: 3,
            max_requeues: 10,
            scan_limit: 100,
            error: Some(String::new()),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-error A2AAutoRetrySchedulerScan event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when A2AAutoRetrySchedulerScan has error=Some(\"\"): status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_a2a_auto_retry_scheduler_scan_error_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_a2a_auto_retry_scheduler_scan_error_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::A2AAutoRetrySchedulerScan"),
        "drift message should name the A2AAutoRetrySchedulerScan variant: {message:?}"
    );
    assert!(
        message.contains("error = Some(\"\")"),
        "drift message should name the empty-error invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("Response::Error.message")
            && repair.contains("unexpected response")
            && repair.contains("non-empty")),
        "empty-error A2AAutoRetrySchedulerScan drift repair string should name both production write-sites and the non-empty guarantee: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a MemoryRecordBackfillApplied audit event with savepoint_name=Some(\"\"), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_memory_record_backfill_applied_savepoint_name_empty_drift(
) {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::MemoryRecordBackfillApplied {
            row_count: 7,
            savepoint_name: Some(String::new()),
            dry_run: false,
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-savepoint-name MemoryRecordBackfillApplied event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when MemoryRecordBackfillApplied has savepoint_name=Some(\"\"): status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str()
                == Some("audit_memory_record_backfill_applied_savepoint_name_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_memory_record_backfill_applied_savepoint_name_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::MemoryRecordBackfillApplied"),
        "drift message should name the MemoryRecordBackfillApplied variant: {message:?}"
    );
    assert!(
        message.contains("savepoint_name = Some(\"\")"),
        "drift message should name the empty-savepoint-name invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("MEMORY_BACKFILL_SAVEPOINT_NAME")
            && repair.contains("backfill_receipt_correlation")
            && repair.contains("SAVEPOINT")),
        "empty-savepoint-name MemoryRecordBackfillApplied drift repair string should name the const, its literal value, and the SAVEPOINT wrapper: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a SettlementReceiptBackfillApplied audit event with rollback_path=Some(\"\"), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_settlement_receipt_backfill_applied_rollback_path_empty_drift(
) {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::SettlementReceiptBackfillApplied {
            row_count: 12,
            rollback_path: Some(String::new()),
            dry_run: false,
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-rollback-path SettlementReceiptBackfillApplied event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when SettlementReceiptBackfillApplied has rollback_path=Some(\"\"): status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str()
                == Some("audit_settlement_receipt_backfill_applied_rollback_path_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_settlement_receipt_backfill_applied_rollback_path_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::SettlementReceiptBackfillApplied"),
        "drift message should name the SettlementReceiptBackfillApplied variant: {message:?}"
    );
    assert!(
        message.contains("rollback_path = Some(\"\")"),
        "drift message should name the empty-rollback-path invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("rollback_checkpoint_path")
            && repair.contains(".backfill-rollback-")
            && repair.contains("display")),
        "empty-rollback-path SettlementReceiptBackfillApplied drift repair string should name rollback_checkpoint_path, the literal suffix, and the Display fold: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends an A2ARepairApplied audit event with duplicate_risk=Some(\"\"), and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_a2a_repair_applied_duplicate_risk_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::A2ARepairApplied {
            task_id: Uuid::new_v4(),
            action: "requeue".into(),
            reason: "operator override".into(),
            lease_id: Some(Uuid::new_v4()),
            duplicate_risk: Some(String::new()),
            attempt: 1,
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-duplicate-risk A2ARepairApplied event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when A2ARepairApplied has duplicate_risk=Some(\"\"): status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_a2a_repair_applied_duplicate_risk_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_a2a_repair_applied_duplicate_risk_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::A2ARepairApplied"),
        "drift message should name the A2ARepairApplied variant: {message:?}"
    );
    assert!(
        message.contains("duplicate_risk = Some(\"\")"),
        "drift message should name the empty-duplicate-risk invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("a2a_duplicate_risk")
            && repair.contains("idempotent")
            && repair.contains("operator_accepted")
            && repair.contains("ForceError")),
        "empty-duplicate-risk A2ARepairApplied drift repair string should name a2a_duplicate_risk, both literals, and the ForceError None arm: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a CapabilityRevokeRejected audit event with reason=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_capability_revoke_rejected_reason_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::CapabilityRevokeRejected {
            signature_b58: "a".repeat(88),
            reason: String::new(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-reason CapabilityRevokeRejected event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when CapabilityRevokeRejected has reason=\"\": status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_capability_revoke_rejected_reason_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_capability_revoke_rejected_reason_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::CapabilityRevokeRejected"),
        "drift message should name the CapabilityRevokeRejected variant: {message:?}"
    );
    assert!(
        message.contains("reason = \"\""),
        "drift message should name the empty-reason invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("revoke_capability")
            && repair.contains("subject-mismatch")
            && repair.contains("peer is not the subject of this capability")),
        "empty-reason CapabilityRevokeRejected drift repair string should name revoke_capability, the subject-mismatch branch, and the literal reason: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a CapabilityScopeRejected audit event with agent_id=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_capability_scope_rejected_agent_id_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::CapabilityScopeRejected {
            agent_id: String::new(),
            action: "audit.purge".into(),
            reason: "before_ms 100 exceeds capability scope".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-agent_id CapabilityScopeRejected event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when CapabilityScopeRejected has agent_id=\"\": status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_capability_scope_rejected_agent_id_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_capability_scope_rejected_agent_id_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::CapabilityScopeRejected"),
        "drift message should name the CapabilityScopeRejected variant: {message:?}"
    );
    assert!(
        message.contains("agent_id = \"\""),
        "drift message should name the empty-agent_id invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("record_capability_scope_rejected")
            && repair.contains("audit:purge")
            && repair.contains("a2a-send:")),
        "empty-agent_id CapabilityScopeRejected drift repair string should name record_capability_scope_rejected and the daemon-supplied scope-key literals: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a CapabilityScopeRejected audit event with action=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_capability_scope_rejected_action_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::CapabilityScopeRejected {
            agent_id: "audit:purge".into(),
            action: String::new(),
            reason: "before_ms 100 exceeds capability scope".into(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-action CapabilityScopeRejected event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when CapabilityScopeRejected has action=\"\": status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_capability_scope_rejected_action_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_capability_scope_rejected_action_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::CapabilityScopeRejected"),
        "drift message should name the CapabilityScopeRejected variant: {message:?}"
    );
    assert!(
        message.contains("action = \"\""),
        "drift message should name the empty-action invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("audit.purge")
            && repair.contains("memory.read")
            && repair.contains("scoped_action_alternatives")),
        "empty-action CapabilityScopeRejected drift repair string should name the hardcoded action literals and scoped_action_alternatives: {row:?}"
    );

    let _ = restarted.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd, appends a CapabilityScopeRejected audit event with reason=\"\", and runs `covenant verify --json`"]
async fn live_cli_verify_json_reports_audit_capability_scope_rejected_reason_empty_drift() {
    let home = tempfile::tempdir().expect("tempdir");
    let cli_exe = covenant_cli_bin();

    let port = pick_free_port();
    let mut child = spawn_daemon(home.path(), port).await;
    wait_for_daemon(home.path(), &mut child).await;
    let _ = child.kill().await;

    let audit_dir = home.path().join("audit");
    std::fs::create_dir_all(&audit_dir).expect("create audit dir");
    let event_id = Uuid::new_v4();
    let event = AuditEvent {
        id: event_id,
        timestamp_ms: 1_700_000_000_000,
        issuer: AgentId::new("user@local", [1u8; 32]),
        kind: AuditKind::CapabilityScopeRejected {
            agent_id: "audit:purge".into(),
            action: "audit.purge".into(),
            reason: String::new(),
        },
    };
    let audit_path = audit_dir.join("events.jsonl");
    use std::io::Write as _;
    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .expect("open audit/events.jsonl for append");
    writeln!(audit_file, "{}", serde_json::to_string(&event).unwrap())
        .expect("append empty-reason CapabilityScopeRejected event");
    drop(audit_file);

    let restart_port = pick_free_port();
    let mut restarted = spawn_daemon(home.path(), restart_port).await;
    wait_for_daemon(home.path(), &mut restarted).await;

    let drift_output = run_cli_raw(
        &cli_exe,
        home.path(),
        &["verify", "--json", "--window", "25"],
    )
    .await;
    let drift_stdout = String::from_utf8_lossy(&drift_output.stdout).to_string();
    let drift_stderr = String::from_utf8_lossy(&drift_output.stderr).to_string();
    assert!(
        !drift_output.status.success(),
        "verify must exit non-zero when CapabilityScopeRejected has reason=\"\": status={:?} stdout={drift_stdout:?} stderr={drift_stderr:?}",
        drift_output.status
    );
    assert!(
        drift_stderr.trim().is_empty(),
        "verify --json must keep drift on stdout without stderr noise: {drift_stderr:?}"
    );
    let drift: Value =
        serde_json::from_str(drift_stdout.trim()).expect("verify drift stdout must be JSON");

    let event_id_str = event_id.to_string();
    let row = drift["drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .find(|item| {
            item["kind"].as_str() == Some("audit_capability_scope_rejected_reason_empty")
                && item["id"].as_str() == Some(event_id_str.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected audit_capability_scope_rejected_reason_empty drift for {event_id_str}: {drift:?}"
            )
        });
    let message = row["message"].as_str().unwrap_or("");
    assert!(
        message.contains("AuditKind::CapabilityScopeRejected"),
        "drift message should name the CapabilityScopeRejected variant: {message:?}"
    );
    assert!(
        message.contains("reason = \"\""),
        "drift message should name the empty-reason invariant: {message:?}"
    );
    assert!(
        row["repair"].as_str().is_some_and(|repair| repair
            .contains("does not match capability scope")
            && repair.contains("permission_*_scope_allows")
            && repair.contains("Ok(false)")
            && repair.contains("Err")),
        "empty-reason CapabilityScopeRejected drift repair string should name the value-mismatch literal pattern, the permission_*_scope_allows Err source, and the Ok(false)/Err distinction: {row:?}"
    );

    let _ = restarted.kill().await;
}
