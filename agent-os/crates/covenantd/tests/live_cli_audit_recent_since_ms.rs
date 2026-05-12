//! Live CLI coverage for `covenant audit recent --since-ms`.
//!
//! Spawns the real daemon, creates two distinct audit rows by granting two
//! capabilities back-to-back, then queries the unfiltered audit feed once
//! to learn each row's `timestamp_ms`. The midpoint between the two
//! timestamps is the `--since-ms` threshold: the older row's timestamp is
//! strictly less than the midpoint, the newer row's timestamp is greater
//! than or equal, so the filter must keep the newer row and drop the
//! older one regardless of wall-clock drift.
//!
//! Also asserts:
//!
//! - JSON envelope echoes the active `since_ms` so machine consumers can
//!   distinguish a filtered result from a pre-filter result.
//! - With a `--since-ms` value greater than the newest row's timestamp,
//!   the events array is empty (no false positives past the threshold).
//! - With a `--since-ms` value equal to the older row's timestamp, the
//!   older row is preserved (boundary is inclusive at `>=`).
//! - A non-integer `--since-ms` value fails the CLI invocation before the
//!   daemon sees the frame.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_cli_audit_recent_since_ms -- --ignored live_`.

use serde_json::Value;
use std::path::{Path, PathBuf};
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

async fn wait_for_sock(path: &Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
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

async fn run_cli_ok(cli: &Path, home: &Path, args: &[&str]) -> Value {
    let out = Command::new(cli)
        .args(args)
        .env("COVENANT_HOME", home)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "CLI {args:?} failed: status={:?} stdout={stdout:?} stderr={stderr:?}",
        out.status,
    );
    serde_json::from_str(stdout.trim()).expect("CLI stdout must be a single JSON object")
}

fn timestamp_for_action(value: &Value, action: &str) -> u64 {
    value["events"]
        .as_array()
        .expect("events array")
        .iter()
        .find(|event| {
            event["kind"]["type"].as_str() == Some("capability_granted")
                && event["kind"]["action"].as_str() == Some(action)
        })
        .and_then(|event| event["timestamp_ms"].as_u64())
        .unwrap_or_else(|| {
            panic!("audit row for {action} not found in audit feed: {value:?}");
        })
}

fn action_present(value: &Value, action: &str) -> bool {
    value["events"]
        .as_array()
        .expect("events array")
        .iter()
        .any(|event| {
            event["kind"]["type"].as_str() == Some("capability_granted")
                && event["kind"]["action"].as_str() == Some(action)
        })
}

#[tokio::test]
#[ignore = "live: spawns covenantd + runs covenant audit recent --since-ms against two audit rows"]
async fn live_cli_audit_recent_since_ms_drops_older_events() {
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

    let cli = covenant_cli_bin();

    let older_action = "tool.call.echo";
    let newer_action = "tool.call.ping";

    run_cli_ok(
        &cli,
        home.path(),
        &["capabilities", "grant", older_action, "--json"],
    )
    .await;
    sleep(Duration::from_millis(50)).await;
    run_cli_ok(
        &cli,
        home.path(),
        &["capabilities", "grant", newer_action, "--json"],
    )
    .await;

    let unfiltered = run_cli_ok(
        &cli,
        home.path(),
        &["audit", "recent", "--limit", "20", "--json"],
    )
    .await;
    assert_eq!(unfiltered["kind"], "audit_recent");
    assert!(
        unfiltered["since_ms"].is_null(),
        "unfiltered audit recent must report since_ms=null so machine consumers know no filter was active: {unfiltered:?}",
    );

    let older_ts = timestamp_for_action(&unfiltered, older_action);
    let newer_ts = timestamp_for_action(&unfiltered, newer_action);
    assert!(
        newer_ts > older_ts,
        "audit rows must be monotonically timestamped for this test to discriminate: older={older_ts} newer={newer_ts}",
    );

    let midpoint = older_ts + (newer_ts - older_ts) / 2 + 1;
    assert!(midpoint > older_ts && midpoint <= newer_ts);

    let filtered = run_cli_ok(
        &cli,
        home.path(),
        &[
            "audit",
            "recent",
            "--limit",
            "20",
            "--since-ms",
            &midpoint.to_string(),
            "--json",
        ],
    )
    .await;
    assert_eq!(filtered["kind"], "audit_recent");
    assert_eq!(
        filtered["since_ms"].as_u64(),
        Some(midpoint),
        "JSON envelope must echo the active since_ms threshold",
    );
    assert!(
        action_present(&filtered, newer_action),
        "newer row must survive --since-ms midpoint; got {filtered:?}",
    );
    assert!(
        !action_present(&filtered, older_action),
        "older row must be filtered out by --since-ms midpoint; got {filtered:?}",
    );

    let past = run_cli_ok(
        &cli,
        home.path(),
        &[
            "audit",
            "recent",
            "--limit",
            "20",
            "--since-ms",
            &(newer_ts + 1).to_string(),
            "--json",
        ],
    )
    .await;
    let past_events = past["events"]
        .as_array()
        .expect("past events array")
        .clone();
    assert!(
        !past_events.iter().any(|event| {
            event["kind"]["type"].as_str() == Some("capability_granted")
                && (event["kind"]["action"].as_str() == Some(older_action)
                    || event["kind"]["action"].as_str() == Some(newer_action))
        }),
        "--since-ms past the newest grant row must drop both grants; got events={past_events:?}",
    );

    let inclusive = run_cli_ok(
        &cli,
        home.path(),
        &[
            "audit",
            "recent",
            "--limit",
            "20",
            "--since-ms",
            &older_ts.to_string(),
            "--json",
        ],
    )
    .await;
    assert!(
        action_present(&inclusive, older_action) && action_present(&inclusive, newer_action),
        "--since-ms equal to the older row's timestamp must preserve it (boundary inclusive): {inclusive:?}",
    );

    let typo = Command::new(&cli)
        .args(["audit", "recent", "--since-ms", "not-a-number", "--json"])
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI for typo");
    assert!(
        !typo.status.success(),
        "non-integer --since-ms must fail the CLI invocation before the daemon sees it",
    );
    let stderr = String::from_utf8_lossy(&typo.stderr);
    assert!(
        stderr.contains("--since-ms must be an integer"),
        "stderr must explain the parse rejection: {stderr:?}",
    );

    let _ = child.kill().await;
}
