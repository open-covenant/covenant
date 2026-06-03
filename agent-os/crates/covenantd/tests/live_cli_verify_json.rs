//! Live CLI coverage for `covenant verify --json`.
//!
//! Spawns a real daemon against a temp home, runs the state verifier
//! through the CLI, and asserts stdout is one stable JSON object. Opt-in
//! because it crosses process and socket boundaries. Run from `agent-os/`
//! after `cargo build -p covenant`.

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
