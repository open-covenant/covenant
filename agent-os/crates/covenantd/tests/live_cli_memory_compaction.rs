//! Live CLI coverage for `covenant memory compact` and `plan-compaction`.
//!
//! Seeds deterministic memory records, drives dry-run and apply through
//! the public CLI, and verifies persisted memory state plus audit evidence.

use covenant_memory::{MemoryStore, SqliteStore};
use covenant_types::{AgentId, MemoryRecord, MemoryTier};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::time::sleep;
use uuid::Uuid;

struct CompactionFixture {
    old_working: Uuid,
    old_episodic: Uuid,
    child: Uuid,
    longterm: Uuid,
}

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

async fn run_cli(cli_exe: &std::path::Path, home: &std::path::Path, args: &[&str]) -> String {
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

async fn seed_memory(home: &std::path::Path) -> CompactionFixture {
    let store = SqliteStore::open(&home.join("memory.db")).expect("open memory db");
    let owner = AgentId::new("operator@local", [9; 32]);
    let fixture = CompactionFixture {
        old_working: Uuid::new_v4(),
        old_episodic: Uuid::new_v4(),
        child: Uuid::new_v4(),
        longterm: Uuid::new_v4(),
    };
    let records = [
        MemoryRecord {
            id: fixture.old_working,
            tier: MemoryTier::Working,
            owner: owner.clone(),
            text: "old working".into(),
            embedding: vec![1.0],
            metadata: json!({}),
            created_at: 10,
            parent: None,
        },
        MemoryRecord {
            id: fixture.old_episodic,
            tier: MemoryTier::Episodic,
            owner: owner.clone(),
            text: "old episodic".into(),
            embedding: vec![1.0],
            metadata: json!({}),
            created_at: 10,
            parent: None,
        },
        MemoryRecord {
            id: fixture.child,
            tier: MemoryTier::Episodic,
            owner: owner.clone(),
            text: "child".into(),
            embedding: vec![1.0],
            metadata: json!({}),
            created_at: 50,
            parent: Some(fixture.old_working),
        },
        MemoryRecord {
            id: fixture.longterm,
            tier: MemoryTier::LongTerm,
            owner,
            text: "durable context".into(),
            embedding: vec![1.0],
            metadata: json!({}),
            created_at: 10,
            parent: None,
        },
    ];
    for record in records {
        store.put(record).await.expect("seed memory record");
    }
    fixture
}

fn assert_ids(value: &Value, key: &str, expected: &[Uuid]) {
    let mut actual = value[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} must be an array: {value:?}"))
        .iter()
        .map(|item| item.as_str().expect("uuid string").to_string())
        .collect::<Vec<_>>();
    let mut expected = expected.iter().map(Uuid::to_string).collect::<Vec<_>>();
    actual.sort();
    expected.sort();
    assert_eq!(actual, expected, "{key} mismatch in {value:?}");
}

#[tokio::test]
#[ignore = "live: spawns covenantd + runs `covenant memory compact` subprocesses"]
async fn live_cli_memory_compaction_round_trip() {
    let home = tempfile::tempdir().expect("tempdir");
    let fixture = seed_memory(home.path()).await;

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
        &["capabilities", "grant", "memory.compact.dry_run"],
    )
    .await;
    run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.compact.apply"],
    )
    .await;

    let base_args = [
        "memory",
        "compact",
        "--delete-working-before-ms",
        "20",
        "--delete-episodic-before-ms",
        "20",
        "--mark-longterm-stale-before-ms",
        "20",
        "--marked-at-ms",
        "99",
        "--detach-stale-parents",
    ];
    let mut dry_run_args = base_args.to_vec();
    dry_run_args.extend(["--reason", "live compaction dry run"]);
    let dry_run_stdout = run_cli(&cli_exe, home.path(), &dry_run_args).await;
    let dry_run: Value =
        serde_json::from_str(dry_run_stdout.trim()).expect("dry-run stdout must be JSON");
    assert_eq!(dry_run["mode"].as_str(), Some("dry_run"));
    assert_eq!(dry_run["would_change"].as_bool(), Some(true));
    assert_eq!(dry_run["changed"].as_bool(), Some(false));
    assert_ids(
        &dry_run,
        "deleted",
        &[fixture.old_working, fixture.old_episodic],
    );
    assert_ids(&dry_run, "stale_marked", &[fixture.longterm]);
    assert_ids(&dry_run, "parents_detached", &[fixture.child]);

    let plan_args = [
        "memory",
        "plan-compaction",
        "--delete-working-before-ms",
        "20",
        "--delete-episodic-before-ms",
        "20",
        "--mark-longterm-stale-before-ms",
        "20",
        "--marked-at-ms",
        "99",
        "--detach-stale-parents",
        "--reason",
        "live compaction plan",
    ];
    let plan_stdout = run_cli(&cli_exe, home.path(), &plan_args).await;
    let plan: Value = serde_json::from_str(plan_stdout.trim()).expect("plan stdout must be JSON");
    assert_eq!(plan["kind"], "memory_compaction_plan");
    assert_eq!(plan["outcome"]["mode"].as_str(), Some("dry_run"));
    assert_eq!(plan["outcome"]["would_change"].as_bool(), Some(true));
    assert_eq!(plan["outcome"]["changed"].as_bool(), Some(false));
    assert_eq!(plan["expected_receipt_changes"]["mode"], "none");
    assert_ids(
        &plan["outcome"],
        "deleted",
        &[fixture.old_working, fixture.old_episodic],
    );
    assert_ids(&plan["outcome"], "stale_marked", &[fixture.longterm]);
    assert_ids(&plan["outcome"], "parents_detached", &[fixture.child]);

    let mut apply_args = base_args.to_vec();
    apply_args.extend(["--reason", "live compaction apply", "--apply"]);
    let apply_stdout = run_cli(&cli_exe, home.path(), &apply_args).await;
    let applied: Value =
        serde_json::from_str(apply_stdout.trim()).expect("apply stdout must be JSON");
    assert_eq!(applied["mode"].as_str(), Some("apply"));
    assert_eq!(applied["would_change"].as_bool(), Some(true));
    assert_eq!(applied["changed"].as_bool(), Some(true));
    assert_ids(
        &applied,
        "deleted",
        &[fixture.old_working, fixture.old_episodic],
    );
    assert_ids(&applied, "stale_marked", &[fixture.longterm]);
    assert_ids(&applied, "parents_detached", &[fixture.child]);

    let mut json_args = base_args.to_vec();
    json_args.extend(["--reason", "live compaction json envelope", "--json"]);
    let json_stdout = run_cli(&cli_exe, home.path(), &json_args).await;
    let envelope: Value =
        serde_json::from_str(json_stdout.trim()).expect("envelope stdout must be JSON");
    assert_eq!(
        envelope["kind"], "memory_compacted",
        "envelope={envelope:?}"
    );
    assert_eq!(
        envelope["outcome"]["mode"].as_str(),
        Some("dry_run"),
        "envelope={envelope:?}"
    );
    assert!(
        envelope["outcome"]["would_change"].is_boolean(),
        "envelope={envelope:?}"
    );

    let store = SqliteStore::open(&home.path().join("memory.db")).expect("open memory db");
    assert!(store.get(fixture.old_working).await.unwrap().is_none());
    assert!(store.get(fixture.old_episodic).await.unwrap().is_none());
    let child_record = store
        .get(fixture.child)
        .await
        .unwrap()
        .expect("child remains");
    assert_eq!(child_record.parent, None);
    let longterm = store
        .get(fixture.longterm)
        .await
        .unwrap()
        .expect("longterm remains");
    assert_eq!(longterm.metadata["stale_context"]["marked_at_ms"], 99);
    assert_eq!(
        longterm.metadata["stale_context"]["reason"],
        "live compaction apply"
    );

    let audit_stdout = run_cli(
        &cli_exe,
        home.path(),
        &["audit", "recent", "--json", "--limit", "50"],
    )
    .await;
    let audit: Value =
        serde_json::from_str(audit_stdout.trim()).expect("audit recent --json must be JSON");
    let compaction = audit["events"]
        .as_array()
        .expect("audit events")
        .iter()
        .find(|event| {
            event["kind"]["type"].as_str() == Some("memory_compaction_applied")
                && event["kind"]["mode"].as_str() == Some("apply")
                && event["kind"]["reason"].as_str() == Some("live compaction apply")
        })
        .unwrap_or_else(|| panic!("missing apply compaction audit row: {audit:?}"));
    assert_eq!(compaction["kind"]["changed"].as_bool(), Some(true));
    assert_ids(
        &compaction["kind"],
        "deleted",
        &[fixture.old_working, fixture.old_episodic],
    );
    assert_ids(&compaction["kind"], "stale_marked", &[fixture.longterm]);
    assert_ids(&compaction["kind"], "parents_detached", &[fixture.child]);

    let _ = child.kill().await;
}
