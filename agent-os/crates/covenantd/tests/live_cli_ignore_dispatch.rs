//! Live CLI coverage for ignored intent dispatch.
//!
//! Runs a real daemon with a deterministic `.covenantignore`, submits an
//! ignored intent through the public CLI, and asserts the gate leaves no
//! memory, receipt, or dispatch side effects.

use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
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

fn json(stdout: &str, command: &str) -> Value {
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("{command} stdout must be valid JSON: {e}; stdout={stdout:?}"))
}

#[tokio::test]
#[ignore = "live: spawns covenantd + proves ignored intent dispatch has no memory/receipt side effects"]
async fn live_cli_ignore_dispatch_has_no_state_side_effects() {
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::write(home.path().join(".covenantignore"), "id_rsa\n")
        .expect("write deterministic ignore file");

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
        panic!("daemon never accepted connections at {}", sock.display());
    }
    wait_for_operator_token(home.path()).await;

    let cli_exe = covenant_cli_bin();
    for action in ["memory.write", "memory.read", "chain.receipts"] {
        run_cli(&cli_exe, home.path(), &["capabilities", "grant", action]).await;
    }

    let intent_text = "summarise ~/.ssh/id_rsa";
    let intent_stdout = run_cli(&cli_exe, home.path(), &["intent", "--json", intent_text]).await;
    let intent = json(&intent_stdout, "intent --json");
    assert_eq!(intent["kind"].as_str(), Some("intent_result"));
    assert_eq!(intent["status"].as_str(), Some("ignored"));
    assert!(intent["settlement"].is_null());
    assert!(
        intent["text"]
            .as_str()
            .is_some_and(|text| text.contains("id_rsa")),
        "ignored response should name the matched rule: {intent:?}"
    );
    let intent_id = intent["intent_id"]
        .as_str()
        .expect("ignored response must include an intent_id");

    let memory_stdout = run_cli(
        &cli_exe,
        home.path(),
        &["memory", "recent", "--json", "--limit", "10"],
    )
    .await;
    let memory = json(&memory_stdout, "memory recent --json");
    assert_eq!(memory["kind"].as_str(), Some("memory_read"));
    assert!(
        memory["records"]
            .as_array()
            .expect("memory records")
            .is_empty(),
        "ignored dispatch must not write memory: {memory:?}"
    );

    let receipts_stdout = run_cli(
        &cli_exe,
        home.path(),
        &["receipts", "recent", "--json", "--limit", "10"],
    )
    .await;
    let receipts = json(&receipts_stdout, "receipts recent --json");
    assert_eq!(receipts["kind"].as_str(), Some("receipt_list"));
    assert!(
        receipts["receipts"]
            .as_array()
            .expect("receipt list")
            .is_empty(),
        "ignored dispatch must not write settlement receipts: {receipts:?}"
    );

    let audit_stdout = run_cli(
        &cli_exe,
        home.path(),
        &["audit", "recent", "--json", "--limit", "50"],
    )
    .await;
    let audit = json(&audit_stdout, "audit recent --json");
    let events = audit["events"].as_array().expect("audit events");
    assert!(
        events.iter().any(|event| {
            event["kind"]["type"].as_str() == Some("intent_ignored")
                && event["kind"]["intent_id"].as_str() == Some(intent_id)
                && event["kind"]["intent_text"].as_str() == Some(intent_text)
                && event["kind"]["matched_pattern"].as_str() == Some("id_rsa")
        }),
        "ignored dispatch should leave intent_ignored audit evidence: {audit:?}"
    );
    assert!(
        !events.iter().any(|event| {
            event["kind"]["type"].as_str() == Some("intent_dispatched")
                && event["kind"]["intent_id"].as_str() == Some(intent_id)
        }),
        "ignored dispatch must not leave intent_dispatched audit evidence: {audit:?}"
    );

    let _ = child.kill().await;
}
