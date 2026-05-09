//! Live CLI coverage for `covenant capabilities purge --json`.
//!
//! Spawns a real daemon against a temp home, creates one revoked
//! capability, grants the purge authority, and asserts purge emits one
//! stable JSON object.

use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
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

fn signature_from_grant(stdout: &str) -> &str {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("signature: "))
        .expect("grant stdout must contain a signature line")
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

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_string()).collect()
}

async fn run_cli_output(
    cli_exe: &std::path::Path,
    home: &std::path::Path,
    args: &[String],
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

async fn run_cli_ok(cli_exe: &std::path::Path, home: &std::path::Path, args: &[String]) -> String {
    let output = run_cli_output(cli_exe, home, args).await;

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
#[ignore = "live: spawns covenantd + runs `covenant capabilities purge --json` subprocess"]
async fn live_cli_capabilities_purge_json_round_trip() {
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
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("unix time")
        .as_millis() as u64;
    let allowed_before_ms = now_ms + 30_000;
    let denied_before_ms = allowed_before_ms + 30_000;

    let scope = format!(r#"{{"version":1,"before_ms":{}}}"#, allowed_before_ms);
    let mut grant_purge = args(&["capabilities", "grant", "capabilities.purge", "--scope"]);
    grant_purge.push(scope);
    run_cli_ok(&cli_exe, home.path(), &grant_purge).await;
    let grant = run_cli_ok(
        &cli_exe,
        home.path(),
        &args(&["capabilities", "grant", "tool.call.echo"]),
    )
    .await;
    let signature = signature_from_grant(&grant);
    run_cli_ok(
        &cli_exe,
        home.path(),
        &args(&["capabilities", "revoke", signature]),
    )
    .await;

    let mut denied_cmd = args(&["capabilities", "purge", "--before-ms"]);
    denied_cmd.push(denied_before_ms.to_string());
    denied_cmd.push("--json".into());
    let denied = run_cli_output(&cli_exe, home.path(), &denied_cmd).await;
    let denied_stdout = String::from_utf8_lossy(&denied.stdout).to_string();
    let denied_stderr = String::from_utf8_lossy(&denied.stderr).to_string();
    assert!(
        !denied.status.success(),
        "capabilities purge should fail when before_ms exceeds granted scope: status={:?} stdout={denied_stdout:?} stderr={denied_stderr:?}",
        denied.status
    );

    let mut allowed_cmd = args(&["capabilities", "purge", "--before-ms"]);
    allowed_cmd.push(allowed_before_ms.to_string());
    allowed_cmd.push("--json".into());
    let stdout = run_cli_ok(&cli_exe, home.path(), &allowed_cmd).await;

    let value: Value =
        serde_json::from_str(stdout.trim()).expect("capabilities purge --json must be valid JSON");
    assert_eq!(value["kind"].as_str(), Some("capabilities_purged"));
    assert_eq!(value["before_ms"].as_u64(), Some(allowed_before_ms));
    assert_eq!(value["purged"].as_u64(), Some(1));

    let _ = child.kill().await;
}
