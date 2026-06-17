//! Live CLI coverage for audit tamper-detection: a corrupted on-disk audit
//! chain must make `covenant audit verify` report valid=false.
//!
//! `live_cli_audit_verify` covers only the healthy valid=true path. The
//! valid=false verdict is unit-tested in process (verify_integrity's
//! length-mismatch / missing-anchor / payload-tamper checks) but never driven
//! through the real daemon + CLI. Because `verify_integrity` re-reads
//! events.jsonl and the events.chain.jsonl anchor sidecar fresh on every call,
//! corrupting the sidecar out-of-band is detected with no restart.
//!
//! This test establishes a valid=true baseline, empties the anchor sidecar so
//! the daemon sees zero anchors against a non-empty event log, and asserts the
//! next verify reports valid=false with a non-empty failures list and an
//! events/anchors disagreement.
//!
//! Hermetic and ignored by default. Build the CLI first, then run:
//! `cargo build -p covenant && cargo test -p covenantd --test live_cli_audit_verify_tamper -- --ignored live_`.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
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

async fn run_ok(home: &Path, cli: &Path, args: &[&str]) -> String {
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
        "CLI failed: args={args:?} status={:?} stdout={stdout:?} stderr={stderr:?}",
        out.status,
    );
    stdout
}

/// `audit verify --json` wraps the report in {kind, report}; return the report.
async fn verify_report(home: &Path, cli: &Path) -> Value {
    let stdout = run_ok(home, cli, &["audit", "verify", "--json"]).await;
    let envelope: Value =
        serde_json::from_str(stdout.trim()).expect("audit verify --json stdout must be JSON");
    assert_eq!(envelope["kind"], "audit_integrity", "envelope={envelope:?}");
    envelope["report"].clone()
}

#[tokio::test]
#[ignore = "live: spawns covenantd, corrupts the on-disk audit chain, and asserts `covenant audit verify` reports valid=false"]
async fn live_cli_audit_verify_detects_tampered_chain() {
    let home = tempfile::tempdir().expect("tempdir");
    let sock = home.path().join("sock");
    let cli = covenant_cli_bin();

    let mut child = spawn_daemon(home.path());
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon never created its socket at {}", sock.display());
    }
    wait_for_operator_token(home.path()).await;

    // Seed at least one audit event (and its chain anchor).
    run_ok(home.path(), &cli, &["capabilities", "grant", "tool.call.echo"]).await;

    // Baseline: a healthy chain verifies. Without this a daemon that always
    // reports invalid would pass, so the corruption below would prove nothing.
    let baseline = verify_report(home.path(), &cli).await;
    assert_eq!(baseline["valid"], true, "baseline must be valid: {baseline:?}");
    assert_eq!(
        baseline["failures"].as_array().map(Vec::len),
        Some(0),
        "baseline must carry no failures: {baseline:?}",
    );
    let baseline_events = baseline["events"].as_u64().expect("events count");
    assert!(baseline_events > 0, "baseline must have events: {baseline:?}");
    assert_eq!(
        baseline["anchors"].as_u64(),
        Some(baseline_events),
        "baseline events and anchors must agree: {baseline:?}",
    );

    // Tamper out-of-band: empty the anchor sidecar so the reloaded chain has
    // zero anchors against a non-empty event log. verify_integrity re-reads
    // both files on the next call, so no restart is needed. No audit-generating
    // command runs between here and the re-verify, so the corruption stands.
    let chain_path = home.path().join("audit").join("events.chain.jsonl");
    assert!(chain_path.exists(), "anchor sidecar missing at {}", chain_path.display());
    std::fs::write(&chain_path, b"").expect("truncate anchor sidecar");

    let tampered = verify_report(home.path(), &cli).await;
    assert_eq!(
        tampered["valid"], false,
        "a corrupted chain must verify as invalid: {tampered:?}",
    );
    assert!(
        tampered["failures"]
            .as_array()
            .map(|f| !f.is_empty())
            .unwrap_or(false),
        "an invalid verdict must name at least one failure: {tampered:?}",
    );
    assert_eq!(
        tampered["anchors"].as_u64(),
        Some(0),
        "the emptied sidecar must report zero anchors: {tampered:?}",
    );
    assert_eq!(
        tampered["events"].as_u64(),
        Some(baseline_events),
        "the event log is untouched, so events must be unchanged: {tampered:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
