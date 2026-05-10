//! Live CLI coverage for `covenant a2a retry-stale --json`.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;

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

#[tokio::test]
#[ignore = "live: spawns covenantd + runs `covenant a2a retry-stale --json` subprocess"]
async fn live_cli_a2a_retry_stale_json_round_trip() {
    let home = tempfile::tempdir().expect("tempdir");
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
        &["capabilities", "grant", "a2a.repair.requeue"],
    )
    .await;

    let stdout = run_cli(
        &cli,
        home.path(),
        &[
            "a2a",
            "retry-stale",
            "--enable",
            "--min-lease-age-ms",
            "0",
            "--max-attempts",
            "2",
            "--max-requeues",
            "1",
            "--scan-limit",
            "5",
            "--json",
        ],
    )
    .await;
    let value: Value =
        serde_json::from_str(stdout.trim()).expect("a2a retry-stale --json must be JSON");

    assert_eq!(value["kind"].as_str(), Some("a2a_auto_retry"));
    assert_eq!(value["report"]["policy"]["enabled"].as_bool(), Some(true));
    assert_eq!(
        value["report"]["policy"]["min_lease_age_ms"].as_u64(),
        Some(0)
    );
    assert_eq!(value["report"]["policy"]["max_attempts"].as_u64(), Some(2));
    assert_eq!(value["report"]["policy"]["max_requeues"].as_u64(), Some(1));
    assert_eq!(value["report"]["policy"]["scan_limit"].as_u64(), Some(5));
    assert_eq!(value["report"]["considered"].as_u64(), Some(0));
    assert!(value["report"]["requeued"]
        .as_array()
        .is_some_and(Vec::is_empty));
    assert!(value["report"]["skipped"]
        .as_array()
        .is_some_and(Vec::is_empty));

    let _ = child.kill().await;
}
