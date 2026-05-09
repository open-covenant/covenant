//! Live CLI coverage for `covenant memory recent/search --json`.

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

fn assert_memory_read_envelope(
    value: &Value,
    mode: &str,
    tier: Option<&str>,
    limit: u64,
    query: Option<&str>,
) {
    assert_eq!(value["kind"].as_str(), Some("memory_read"));
    assert_eq!(value["mode"].as_str(), Some(mode));
    assert_eq!(value["limit"].as_u64(), Some(limit));
    assert_eq!(value["tier"].as_str(), tier);
    assert_eq!(value["query"].as_str(), query);
    assert!(
        value["records"].as_array().is_some(),
        "records must be an array: {value:?}"
    );
}

#[tokio::test]
#[ignore = "live: spawns covenantd + runs `covenant memory recent/search --json` subprocesses"]
async fn live_cli_memory_read_json_round_trip() {
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
    run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.read"],
    )
    .await;
    run_cli(
        &cli_exe,
        home.path(),
        &["capabilities", "grant", "memory.write"],
    )
    .await;

    let fixture = "memory read json fixture";
    run_cli(&cli_exe, home.path(), &["intent", fixture]).await;

    let recent_stdout = run_cli(
        &cli_exe,
        home.path(),
        &[
            "memory", "recent", "--tier", "working", "--limit", "5", "--json",
        ],
    )
    .await;
    let recent: Value =
        serde_json::from_str(recent_stdout.trim()).expect("memory recent --json must be JSON");
    assert_memory_read_envelope(&recent, "recent", Some("working"), 5, None);
    let recent_records = recent["records"].as_array().unwrap();
    assert!(
        recent_records.iter().any(|record| record["text"]
            .as_str()
            .is_some_and(|text| text.contains(fixture))),
        "recent memory should include seeded fixture: {recent:?}"
    );

    let search_stdout = run_cli(
        &cli_exe,
        home.path(),
        &[
            "memory", "search", fixture, "--tier", "working", "--limit", "5", "--json",
        ],
    )
    .await;
    let search: Value =
        serde_json::from_str(search_stdout.trim()).expect("memory search --json must be JSON");
    assert_memory_read_envelope(&search, "search", Some("working"), 5, Some(fixture));
    let search_records = search["records"].as_array().unwrap();
    assert!(
        search_records.iter().any(|record| record["text"]
            .as_str()
            .is_some_and(|text| text.contains(fixture))),
        "memory search should include seeded fixture: {search:?}"
    );

    let _ = child.kill().await;
}
