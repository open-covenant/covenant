//! Live CLI coverage for `covenant memory repair`.

use covenant_memory::{MemoryStore, SqliteStore};
use covenant_types::{AgentId, MemoryRecord, MemoryTier};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;
use uuid::Uuid;

struct RepairFixture {
    parent: Uuid,
    child: Uuid,
    delete: Uuid,
    backfill: Uuid,
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

fn operator(home: &Path) -> AgentId {
    let identity = covenant_identity::LocalIdentity::load_or_create(
        &home.join("identity").join("local.key"),
        "user@local",
    )
    .expect("load operator identity");
    AgentId::new("user@local", identity.pubkey_bytes())
}

fn spawn_daemon(home: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_covenantd"))
        .env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", pick_free_port().to_string())
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd")
}

async fn wait_for_sock(path: &Path) -> bool {
    for _ in 0..100 {
        if UnixStream::connect(path).await.is_ok() {
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

async fn seed_memory(home: &Path, owner: AgentId) -> RepairFixture {
    let store = SqliteStore::open(&home.join("memory.db")).expect("open memory db");
    let fixture = RepairFixture {
        parent: Uuid::new_v4(),
        child: Uuid::new_v4(),
        delete: Uuid::new_v4(),
        backfill: Uuid::new_v4(),
    };
    let records = [
        MemoryRecord {
            id: fixture.parent,
            tier: MemoryTier::Working,
            owner: owner.clone(),
            text: "repair parent".into(),
            embedding: vec![1.0],
            metadata: json!({}),
            created_at: 10,
            parent: None,
        },
        MemoryRecord {
            id: fixture.child,
            tier: MemoryTier::Working,
            owner: owner.clone(),
            text: "repair child".into(),
            embedding: vec![1.0],
            metadata: json!({}),
            created_at: 11,
            parent: Some(fixture.parent),
        },
        MemoryRecord {
            id: fixture.delete,
            tier: MemoryTier::Episodic,
            owner: owner.clone(),
            text: "repair delete".into(),
            embedding: vec![1.0],
            metadata: json!({}),
            created_at: 12,
            parent: None,
        },
        MemoryRecord {
            id: fixture.backfill,
            tier: MemoryTier::LongTerm,
            owner,
            text: "repair backfill".into(),
            embedding: vec![1.0],
            metadata: json!({ "source": "legacy" }),
            created_at: 13,
            parent: None,
        },
    ];
    for record in records {
        store.put(record).await.expect("seed memory record");
    }
    fixture
}

fn parse_json(stdout: &str, args: &[&str]) -> Value {
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("{args:?} stdout must be JSON: {err}; stdout={stdout:?}"))
}

#[tokio::test]
#[ignore = "live: spawns covenantd + runs `covenant memory repair` subprocesses"]
async fn live_cli_memory_repair_round_trip() {
    let home = tempfile::tempdir().expect("tempdir");
    let owner = operator(home.path());
    let fixture = seed_memory(home.path(), owner).await;
    let sock = home.path().join("sock");
    let cli = covenant_cli_bin();
    let mut child = spawn_daemon(home.path());

    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon never created its socket at {}", sock.display());
    }
    wait_for_operator_token(home.path()).await;

    run_cli(
        &cli,
        home.path(),
        &["capabilities", "grant", "memory.repair.dry_run"],
    )
    .await;
    run_cli(
        &cli,
        home.path(),
        &["capabilities", "grant", "memory.repair.apply"],
    )
    .await;

    let child_id = fixture.child.to_string();
    let parent_id = fixture.parent.to_string();
    let detach_args = [
        "memory",
        "repair",
        "detach-parent",
        child_id.as_str(),
        "--expected-parent",
        parent_id.as_str(),
        "--reason",
        "live repair dry run",
    ];
    let detach_dry_run = parse_json(
        &run_cli(&cli, home.path(), &detach_args).await,
        &detach_args,
    );
    assert_eq!(detach_dry_run["action"].as_str(), Some("detach_parent"));
    assert_eq!(detach_dry_run["mode"].as_str(), Some("dry_run"));
    assert_eq!(detach_dry_run["would_change"].as_bool(), Some(true));
    assert_eq!(detach_dry_run["changed"].as_bool(), Some(false));
    assert_eq!(
        detach_dry_run["before"]["parent"].as_str(),
        Some(parent_id.as_str())
    );
    assert!(detach_dry_run["after"]["parent"].is_null());

    let detach_apply_args = [
        "memory",
        "repair",
        "detach-parent",
        child_id.as_str(),
        "--expected-parent",
        parent_id.as_str(),
        "--reason",
        "live repair apply",
        "--apply",
    ];
    let detach_apply = parse_json(
        &run_cli(&cli, home.path(), &detach_apply_args).await,
        &detach_apply_args,
    );
    assert_eq!(detach_apply["mode"].as_str(), Some("apply"));
    assert_eq!(detach_apply["changed"].as_bool(), Some(true));
    assert!(detach_apply["after"]["parent"].is_null());

    let backfill_id = fixture.backfill.to_string();
    let backfill_args = [
        "memory",
        "repair",
        "backfill-provenance",
        backfill_id.as_str(),
        "--provenance",
        r#"{"source":"live_cli_memory_repair"}"#,
        "--reason",
        "live provenance backfill",
        "--apply",
    ];
    let backfill = parse_json(
        &run_cli(&cli, home.path(), &backfill_args).await,
        &backfill_args,
    );
    assert_eq!(backfill["action"].as_str(), Some("backfill_provenance"));
    assert_eq!(backfill["mode"].as_str(), Some("apply"));
    assert_eq!(backfill["changed"].as_bool(), Some(true));
    assert_eq!(
        backfill["after"]["metadata"]["provenance"]["source"].as_str(),
        Some("live_cli_memory_repair")
    );

    let delete_id = fixture.delete.to_string();
    let delete_args = [
        "memory",
        "repair",
        "delete",
        delete_id.as_str(),
        "--reason",
        "live repair delete",
        "--apply",
    ];
    let deleted = parse_json(
        &run_cli(&cli, home.path(), &delete_args).await,
        &delete_args,
    );
    assert_eq!(deleted["action"].as_str(), Some("delete_record"));
    assert_eq!(deleted["mode"].as_str(), Some("apply"));
    assert_eq!(deleted["changed"].as_bool(), Some(true));
    assert!(deleted.get("after").is_none());

    let store = SqliteStore::open(&home.path().join("memory.db")).expect("open memory db");
    assert_eq!(
        store
            .get(fixture.child)
            .await
            .expect("read child")
            .and_then(|record| record.parent),
        None
    );
    assert_eq!(
        store
            .get(fixture.backfill)
            .await
            .expect("read backfill")
            .and_then(|record| {
                record
                    .metadata
                    .pointer("/provenance/source")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }),
        Some("live_cli_memory_repair".to_string())
    );
    assert!(store
        .get(fixture.delete)
        .await
        .expect("read deleted")
        .is_none());

    let _ = child.kill().await;
}
