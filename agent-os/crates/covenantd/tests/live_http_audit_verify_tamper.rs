//! Live HTTP coverage for audit tamper-detection: a corrupted on-disk audit
//! chain must make `GET /audit/verify` report valid=false over the gateway.
//!
//! `live_http_audit_verify.rs` covers only the healthy valid=true path over
//! HTTP and records the uncovered case in its own header: tamper detection
//! needs out-of-band file corruption against a live daemon. The CLI/socket
//! boundary is covered by `live_cli_audit_verify_tamper.rs`; this mirrors it on
//! the HTTP boundary so the gateway's `AuditIntegrity` variant is exercised on a
//! broken chain, not just a healthy one. `verify_audit_integrity` is a pure
//! operator-identity gate over `audit.verify_integrity()`, which re-reads
//! events.jsonl and the events.chain.jsonl anchor sidecar fresh on every call
//! and appends nothing, so corrupting the sidecar out-of-band flips the verdict
//! with no restart and no risk of a rebuild.
//!
//! Grant a capability to seed audit events and their anchors, establish a
//! valid=true baseline over HTTP, empty the anchor sidecar so the daemon sees
//! zero anchors against a non-empty event log, and assert the next
//! `GET /audit/verify` reports valid=false with a non-empty failures list and an
//! events/anchors disagreement. No audit-generating request runs between the
//! corruption and the re-verify, so the emptied sidecar stands.
//!
//! Hermetic — no external services. `#[ignore]`'d. Uses its own tempdir to stay
//! safe under `--test-threads`. Run with
//! `cargo test -p covenantd --test live_http_audit_verify_tamper -- --ignored live_`.

use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
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

async fn read_operator_token(home: &Path) -> String {
    let path = home.join("peers").join("operator.token");
    for _ in 0..50 {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let token = text.trim();
            if !token.is_empty() {
                return token.to_string();
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("operator token never appeared at {}", path.display());
}

async fn wait_for_http(base: &str) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .expect("reqwest client");
    for _ in 0..80 {
        match client.get(format!("{base}/health")).send().await {
            Ok(response) if response.status().is_success() => return,
            _ => sleep(Duration::from_millis(50)).await,
        }
    }
    panic!("http gateway never became healthy at {base}/health");
}

async fn spawn_http_daemon(home: &Path) -> (Child, String) {
    let port = pick_free_port();
    let base = format!("http://127.0.0.1:{port}");
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let child = Command::new(exe)
        .env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");
    if !wait_for_sock(&home.join("sock")).await {
        panic!("daemon never created its socket");
    }
    wait_for_http(&base).await;
    (child, base)
}

async fn operator_client(home: &Path) -> reqwest::Client {
    let token = read_operator_token(home).await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .expect("reqwest client")
}

async fn grant(client: &reqwest::Client, base: &str, action: &str) {
    let granted: Value = client
        .post(format!("{base}/capabilities/grant"))
        .json(&json!({ "action": action }))
        .send()
        .await
        .expect("send capability grant")
        .json()
        .await
        .expect("grant body json");
    assert_eq!(
        granted["kind"], "capability_granted",
        "grant {action} must succeed: {granted:?}",
    );
}

/// `GET /audit/verify` answers `{kind: "audit_integrity", report: {...}}`; pull
/// the report out after asserting the envelope kind.
async fn audit_verify_report(client: &reqwest::Client, base: &str) -> Value {
    let envelope: Value = client
        .get(format!("{base}/audit/verify"))
        .send()
        .await
        .expect("get audit verify")
        .json()
        .await
        .expect("audit verify body");
    assert_eq!(
        envelope["kind"], "audit_integrity",
        "audit verify must answer an audit_integrity report: {envelope:?}",
    );
    envelope["report"].clone()
}

#[tokio::test]
#[ignore = "live: spawns covenantd, corrupts the on-disk audit chain, and asserts GET /audit/verify reports valid=false over HTTP"]
async fn live_http_audit_verify_detects_tampered_chain_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    // Seed at least one audit event (and its chain anchor) over HTTP.
    grant(&client, &base, "tool.call.echo").await;

    // Baseline: a healthy chain verifies over the gateway. Without this a daemon
    // that always reports invalid would pass, so the corruption below would
    // prove nothing about tamper detection.
    let baseline = audit_verify_report(&client, &base).await;
    assert_eq!(
        baseline["valid"], true,
        "baseline must be valid: {baseline:?}"
    );
    assert_eq!(
        baseline["failures"].as_array().map(Vec::len),
        Some(0),
        "baseline must carry no failures: {baseline:?}",
    );
    let baseline_events = baseline["events"].as_u64().expect("events count");
    assert!(
        baseline_events > 0,
        "baseline must have events: {baseline:?}"
    );
    assert_eq!(
        baseline["anchors"].as_u64(),
        Some(baseline_events),
        "baseline events and anchors must agree: {baseline:?}",
    );

    // Tamper out-of-band: empty the anchor sidecar so the reloaded chain has
    // zero anchors against a non-empty event log. verify_integrity re-reads both
    // files on the next call, so no restart is needed. No audit-generating
    // request runs between here and the re-verify, so the corruption stands.
    let chain_path = home.path().join("audit").join("events.chain.jsonl");
    assert!(
        chain_path.exists(),
        "anchor sidecar missing at {}",
        chain_path.display(),
    );
    std::fs::write(&chain_path, b"").expect("truncate anchor sidecar");

    let tampered = audit_verify_report(&client, &base).await;
    assert_eq!(
        tampered["valid"], false,
        "a corrupted chain must verify as invalid over HTTP: {tampered:?}",
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
    assert_ne!(
        tampered["events"].as_u64(),
        tampered["anchors"].as_u64(),
        "tamper detection rests on the events/anchors disagreement: {tampered:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
