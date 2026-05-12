//! Live CLI coverage for `covenant receipts recent --since-ms`.
//!
//! Spawns the real daemon, grants the capabilities needed to produce
//! receipts (`memory.write`, `memory.read`, `chain.receipts`), submits
//! two intents 60 ms apart so the settlement layer records two
//! receipts with strictly different `settled_at` timestamps, then
//! queries the unfiltered list once to learn each receipt's
//! `settled_at`. The midpoint between the two timestamps is the
//! `--since-ms` threshold so the boundary assertion cannot flake on a
//! slow CI host.
//!
//! Also asserts:
//!
//! - JSON envelope echoes the active `since_ms` so machine consumers
//!   can distinguish a filtered result from a pre-filter result.
//! - With a `--since-ms` value greater than the newest receipt's
//!   `settled_at`, the receipts array is empty (no false positives
//!   past the threshold).
//! - With a `--since-ms` value equal to the older receipt's
//!   `settled_at`, the older receipt is preserved (boundary is
//!   inclusive at `>=`).
//! - A non-integer `--since-ms` value fails the CLI invocation before
//!   the daemon sees the frame.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_cli_receipts_recent_since_ms -- --ignored live_`.

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

async fn run_cli_ok(cli: &Path, home: &Path, args: &[&str]) -> String {
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
    stdout
}

async fn run_cli_json(cli: &Path, home: &Path, args: &[&str]) -> Value {
    let stdout = run_cli_ok(cli, home, args).await;
    serde_json::from_str(stdout.trim()).expect("CLI stdout must be a single JSON object")
}

fn receipt_settled_at(value: &Value, index: usize) -> u64 {
    value["receipts"][index]["settled_at"]
        .as_u64()
        .unwrap_or_else(|| panic!("receipts[{index}].settled_at missing in {value:?}"))
}

#[tokio::test]
#[ignore = "live: spawns covenantd + runs covenant receipts recent --since-ms against two settlement receipts"]
async fn live_cli_receipts_recent_since_ms_drops_older_receipts() {
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

    for action in ["memory.write", "memory.read", "chain.receipts"] {
        run_cli_json(
            &cli,
            home.path(),
            &["capabilities", "grant", action, "--json"],
        )
        .await;
    }

    run_cli_ok(&cli, home.path(), &["intent", "receipt since-ms probe one"]).await;
    sleep(Duration::from_millis(60)).await;
    run_cli_ok(&cli, home.path(), &["intent", "receipt since-ms probe two"]).await;

    let unfiltered = run_cli_json(
        &cli,
        home.path(),
        &["receipts", "recent", "--limit", "20", "--json"],
    )
    .await;
    assert_eq!(unfiltered["kind"], "receipt_list");
    assert!(
        unfiltered["since_ms"].is_null(),
        "unfiltered receipt list must report since_ms=null so machine consumers know no filter was active: {unfiltered:?}",
    );
    let unfiltered_receipts = unfiltered["receipts"]
        .as_array()
        .expect("receipts array")
        .clone();
    assert_eq!(
        unfiltered_receipts.len(),
        2,
        "no-filter baseline must surface both settlement receipts so a since_ms regression cannot pass by matching the unfiltered shape: {unfiltered:?}",
    );

    let ts_a = receipt_settled_at(&unfiltered, 0);
    let ts_b = receipt_settled_at(&unfiltered, 1);
    let (older_ts, newer_ts) = if ts_a <= ts_b {
        (ts_a, ts_b)
    } else {
        (ts_b, ts_a)
    };
    assert!(
        newer_ts > older_ts,
        "settlement receipts must be monotonically timestamped for this test to discriminate: ts_a={ts_a} ts_b={ts_b}",
    );

    let midpoint = older_ts + (newer_ts - older_ts) / 2 + 1;
    assert!(midpoint > older_ts && midpoint <= newer_ts);

    let filtered = run_cli_json(
        &cli,
        home.path(),
        &[
            "receipts",
            "recent",
            "--limit",
            "20",
            "--since-ms",
            &midpoint.to_string(),
            "--json",
        ],
    )
    .await;
    assert_eq!(filtered["kind"], "receipt_list");
    assert_eq!(
        filtered["since_ms"].as_u64(),
        Some(midpoint),
        "JSON envelope must echo the active since_ms threshold",
    );
    let filtered_receipts = filtered["receipts"]
        .as_array()
        .expect("filtered receipts array")
        .clone();
    let filtered_timestamps: Vec<u64> = filtered_receipts
        .iter()
        .map(|r| r["settled_at"].as_u64().unwrap_or(0))
        .collect();
    assert!(
        filtered_timestamps.iter().all(|ts| *ts >= midpoint),
        "every surviving receipt must be at or above the midpoint threshold: {filtered_timestamps:?}",
    );
    assert!(
        filtered_timestamps.contains(&newer_ts),
        "newer receipt must survive since_ms midpoint: timestamps={filtered_timestamps:?}",
    );
    assert!(
        !filtered_timestamps.contains(&older_ts),
        "older receipt must be filtered out by since_ms midpoint: timestamps={filtered_timestamps:?}",
    );

    let past = run_cli_json(
        &cli,
        home.path(),
        &[
            "receipts",
            "recent",
            "--limit",
            "20",
            "--since-ms",
            &(newer_ts + 1).to_string(),
            "--json",
        ],
    )
    .await;
    let past_receipts = past["receipts"]
        .as_array()
        .expect("past receipts array")
        .clone();
    assert!(
        past_receipts.is_empty(),
        "--since-ms past the newest receipt's settled_at must drop both rows; got receipts={past_receipts:?}",
    );

    let inclusive = run_cli_json(
        &cli,
        home.path(),
        &[
            "receipts",
            "recent",
            "--limit",
            "20",
            "--since-ms",
            &older_ts.to_string(),
            "--json",
        ],
    )
    .await;
    let inclusive_timestamps: Vec<u64> = inclusive["receipts"]
        .as_array()
        .expect("inclusive receipts array")
        .iter()
        .map(|r| r["settled_at"].as_u64().unwrap_or(0))
        .collect();
    assert!(
        inclusive_timestamps.contains(&older_ts) && inclusive_timestamps.contains(&newer_ts),
        "--since-ms equal to the older receipt's settled_at must preserve it (boundary inclusive): {inclusive_timestamps:?}",
    );

    let typo = Command::new(&cli)
        .args(["receipts", "recent", "--since-ms", "not-a-number", "--json"])
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
