//! Live CLI coverage for `covenant chain receipt-batches --json`.
//!
//! Spawns a real daemon against a temp home, grants the capabilities
//! needed to write, flush, and read local receipt batches, submits one
//! intent through the CLI, flushes local receipts, then asserts batch
//! summaries are emitted as one stable JSON object. Opt-in because it
//! crosses process and socket boundaries. Run from `agent-os/` after
//! `cargo build -p covenant`.

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
#[ignore = "live: spawns covenantd + runs `covenant chain receipt-batches --json` subprocess"]
async fn live_cli_receipt_batches_json_round_trip() {
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
    for action in ["memory.write", "chain.flush", "chain.batches"] {
        run_cli(&cli_exe, home.path(), &["capabilities", "grant", action]).await;
    }
    run_cli(
        &cli_exe,
        home.path(),
        &["intent", "receipt batch json probe"],
    )
    .await;
    run_cli(
        &cli_exe,
        home.path(),
        &["chain", "flush-receipts", "--limit", "10"],
    )
    .await;

    let stdout = run_cli(
        &cli_exe,
        home.path(),
        &["chain", "receipt-batches", "--json", "--limit", "10"],
    )
    .await;
    let value: Value =
        serde_json::from_str(stdout.trim()).expect("receipt-batches --json must be valid JSON");
    assert_eq!(value["kind"].as_str(), Some("receipt_batch_list"));
    assert_eq!(value["limit"].as_u64(), Some(10));

    let batches = value["batches"]
        .as_array()
        .expect("receipt_batch_list must include batches array");
    assert_eq!(batches.len(), 1, "expected one receipt batch: {batches:?}");
    assert_eq!(batches[0]["receipt_count"].as_u64(), Some(1));
    assert!(
        batches[0]["batch_id"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "batch must include id: {:?}",
        batches[0]
    );
    assert!(
        batches[0]["merkle_root"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "batch must include merkle root: {:?}",
        batches[0]
    );
    assert!(batches[0]["tx_sig"].is_null());
    assert!(batches[0]["slot"].is_null());

    let _ = child.kill().await;
}
