//! Live CLI coverage for `covenant memory plan-receipt-backfill --json`.
//!
//! Spawns a real daemon, writes one memory receipt through the public CLI,
//! then verifies the receipt-backfill command stays read-only and reports a
//! stable dry-run envelope over the real socket boundary.

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

async fn run_cli_output(
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

#[tokio::test]
#[ignore = "live: spawns covenantd + runs `covenant memory plan-receipt-backfill --json` subprocess"]
async fn live_cli_memory_receipt_backfill_plan_is_read_only() {
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
    for action in ["memory.write", "memory.read", "chain.receipts"] {
        run_cli(&cli_exe, home.path(), &["capabilities", "grant", action]).await;
    }
    run_cli(
        &cli_exe,
        home.path(),
        &["intent", "receipt backfill plan probe"],
    )
    .await;

    let stdout = run_cli(
        &cli_exe,
        home.path(),
        &["memory", "plan-receipt-backfill", "--limit", "10", "--json"],
    )
    .await;
    let value: Value = serde_json::from_str(stdout.trim())
        .expect("memory plan-receipt-backfill --json must be valid JSON");
    assert_eq!(value["kind"].as_str(), Some("memory_receipt_backfill_plan"));
    assert_eq!(value["mode"].as_str(), Some("dry_run"));
    assert_eq!(value["limit"].as_u64(), Some(10));
    assert_eq!(value["mutation_supported"].as_bool(), Some(false));
    assert_eq!(value["refusal"]["apply_supported"].as_bool(), Some(false));
    assert!(
        value["records"].as_array().is_some(),
        "records must be an array: {value:?}"
    );
    assert!(
        value["unmatched_legacy_receipts"].as_array().is_some(),
        "unmatched legacy receipts must be an array: {value:?}"
    );
    assert!(
        value["unmatched_memory_records"].as_array().is_some(),
        "unmatched memory records must be an array: {value:?}"
    );

    let apply = run_cli_output(
        &cli_exe,
        home.path(),
        &["memory", "plan-receipt-backfill", "--apply", "--json"],
    )
    .await;
    let apply_stdout = String::from_utf8_lossy(&apply.stdout);
    let apply_stderr = String::from_utf8_lossy(&apply.stderr);
    assert!(
        !apply.status.success(),
        "--apply must be rejected: status={:?} stdout={apply_stdout:?} stderr={apply_stderr:?}",
        apply.status
    );
    assert!(
        apply_stderr.contains("read-only") || apply_stderr.contains("does not accept --apply"),
        "--apply rejection must name read-only boundary: {apply_stderr:?}"
    );

    let _ = child.kill().await;
}
