//! Live CLI coverage for `covenant capabilities revoke --json`.

use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
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

#[tokio::test]
#[ignore = "live: spawns covenantd + runs `covenant capabilities revoke --json` subprocesses"]
async fn live_cli_capabilities_revoke_json_round_trip() {
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
    let grant_stdout = run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "tool.call.echo", "--json"],
    )
    .await;
    let grant: Value =
        serde_json::from_str(grant_stdout.trim()).expect("grant --json must be JSON");
    let signature = grant["signature_b58"]
        .as_str()
        .expect("grant output must include signature_b58");

    let revoke_stdout = run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "revoke", signature, "--json"],
    )
    .await;
    let revoked: Value =
        serde_json::from_str(revoke_stdout.trim()).expect("revoke --json must be JSON");
    assert_eq!(revoked["kind"].as_str(), Some("capability_revoked"));
    assert_eq!(revoked["signature_b58"].as_str(), Some(signature));
    assert_eq!(revoked["removed"].as_bool(), Some(true));

    let second_revoke_stdout = run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "revoke", signature, "--json"],
    )
    .await;
    let second_revoke: Value =
        serde_json::from_str(second_revoke_stdout.trim()).expect("second revoke must be JSON");
    assert_eq!(second_revoke["kind"].as_str(), Some("capability_revoked"));
    assert_eq!(second_revoke["signature_b58"].as_str(), Some(signature));
    assert_eq!(second_revoke["removed"].as_bool(), Some(false));

    let recent_stdout = run_cli(&cli_exe, home.path(), &["capabilities", "recent", "--json"]).await;
    let recent: Value =
        serde_json::from_str(recent_stdout.trim()).expect("recent --json must be JSON");
    let capabilities = recent["capabilities"]
        .as_array()
        .expect("recent output must include capabilities array");
    assert!(
        !capabilities.iter().any(|cap| {
            cap["signature"].as_str() == Some(signature)
                || cap["capability"]["action"].as_str() == Some("tool.call.echo")
        }),
        "revoked capability must not remain active: {recent:?}"
    );

    let _ = child.kill().await;
}
