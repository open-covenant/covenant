//! Live CLI coverage for `covenant ignore check --json`.

use serde_json::Value;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;
use tokio::process::Command;
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

async fn wait_for_sock(path: &std::path::Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
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

async fn run_cli(
    cli_exe: &std::path::Path,
    home: &std::path::Path,
    args: &[&str],
) -> (ExitStatus, String) {
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
        stderr.trim().is_empty(),
        "CLI command {args:?} must not emit stderr for a completed ignore check: {stderr:?}"
    );
    (output.status, stdout)
}

#[tokio::test]
#[ignore = "live: spawns covenantd + runs `covenant ignore check --json` subprocesses"]
async fn live_cli_ignore_check_json_preserves_exit_codes() {
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
    let (status, stdout) = run_cli(
        &cli_exe,
        home.path(),
        &["ignore", "check", "--json", "summarise ~/.ssh/id_rsa"],
    )
    .await;
    assert_eq!(status.code(), Some(1), "ignored text must exit 1");

    let ignored: Value =
        serde_json::from_str(stdout.trim()).expect("ignored check stdout must be JSON");
    assert_eq!(ignored["kind"].as_str(), Some("ignore_report"));
    assert_eq!(ignored["ignored"].as_bool(), Some(true));
    assert!(
        ignored["matched_pattern"]
            .as_str()
            .is_some_and(|pattern| !pattern.is_empty()),
        "ignored response must include the matched pattern: {ignored:?}"
    );
    assert!(
        ignored["rules_loaded"]
            .as_u64()
            .is_some_and(|count| count > 0),
        "ignored response must include rule count: {ignored:?}"
    );

    let (status, stdout) = run_cli(
        &cli_exe,
        home.path(),
        &["ignore", "check", "--json", "summarise public roadmap"],
    )
    .await;
    assert!(status.success(), "allowed text must exit 0");

    let allowed: Value =
        serde_json::from_str(stdout.trim()).expect("allowed check stdout must be JSON");
    assert_eq!(allowed["kind"].as_str(), Some("ignore_report"));
    assert_eq!(allowed["ignored"].as_bool(), Some(false));
    assert!(allowed["matched_pattern"].is_null());
    assert!(
        allowed["rules_loaded"]
            .as_u64()
            .is_some_and(|count| count > 0),
        "allowed response must include rule count: {allowed:?}"
    );

    let _ = child.kill().await;
}
