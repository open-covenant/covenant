//! Live HTTP coverage for `GET /audit/verify` (`audit_verify` ->
//! `Request::VerifyAuditIntegrity` -> `verify_audit_integrity`).
//!
//! `covenant audit verify` is live-tested over the CLI/socket path by
//! `live_cli_audit_verify.rs`, but no test drives `/audit/verify` over the HTTP
//! gateway. That leaves the operator-identity gate (`verify_audit_integrity`
//! rejects any peer whose pubkey != `self.identity`, lib.rs:3318) and the
//! `AuditIntegrity` wire variant (`kind: "audit_integrity"` with the report
//! nested under `report`: `events`, `anchors`, `valid`, `root_hash_hex`,
//! `failures`) unexercised on the HTTP path.
//!
//! One scenario over the gateway exclusively, with audit state seeded through
//! the public API. The operator bearer gets a valid report on a fresh daemon
//! (proving the gate admits the daemon's own operator identity over HTTP — a
//! broken gate would answer `kind: "error"`). Granting a capability appends and
//! anchors audit rows; the next report must stay valid with `events == anchors`,
//! the `events` count must strictly exceed the empty baseline, and the integrity
//! root must advance off the baseline hash — so a frozen chain or a dropped
//! anchor cannot pass.
//!
//! Recorded uncovered cases: tamper detection (`valid: false` on a corrupted
//! chain) needs out-of-band file corruption against a live daemon, and the
//! non-operator rejection path needs a second authenticated identity.
//!
//! Hermetic — no external services. `#[ignore]`'d. Each test uses its own
//! tempdir to stay safe under `--test-threads`. Run with
//! `cargo test -p covenantd --test live_http_audit_verify -- --ignored live_`.

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

async fn audit_verify(client: &reqwest::Client, base: &str) -> Value {
    client
        .get(format!("{base}/audit/verify"))
        .send()
        .await
        .expect("get audit verify")
        .json()
        .await
        .expect("audit verify body")
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts GET /audit/verify reports a valid, advancing hash chain to the operator over HTTP"]
async fn live_http_audit_verify_reports_valid_advancing_chain_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    // Baseline: the operator bearer (the daemon's own identity) must be admitted
    // by the operator-identity gate and receive a valid report. A broken gate
    // would answer kind "error"; a wrong-variant regression is caught by the
    // kind slug and the nested report object.
    let baseline = audit_verify(&client, &base).await;
    assert_eq!(
        baseline["kind"], "audit_integrity",
        "the operator must be admitted with an audit_integrity report: {baseline:?}",
    );
    let r0 = &baseline["report"];
    assert_eq!(
        r0["valid"], true,
        "a fresh daemon's chain must verify clean: {baseline:?}",
    );
    assert_eq!(
        r0["events"], r0["anchors"],
        "every event must be anchored, so the counts match: {baseline:?}",
    );
    assert_eq!(
        r0["failures"].as_array().map(|f| f.len()),
        Some(0),
        "a clean chain reports no failures: {baseline:?}",
    );
    let baseline_events = r0["events"].as_u64().expect("events is u64");
    let baseline_root = r0["root_hash_hex"]
        .as_str()
        .expect("root_hash_hex is a string")
        .to_string();

    // Seed audit events through the public API: a capability grant appends and
    // anchors CapabilityGranted/CapabilityCheck rows.
    grant(&client, &base, "memory.read").await;

    let report = audit_verify(&client, &base).await;
    assert_eq!(
        report["kind"], "audit_integrity",
        "the populated read must still be an audit_integrity report: {report:?}",
    );
    let r1 = &report["report"];
    assert_eq!(
        r1["valid"], true,
        "the chain must stay valid after real events flow through it: {report:?}",
    );
    assert_eq!(
        r1["events"], r1["anchors"],
        "every newly appended event must be anchored: {report:?}",
    );
    assert_eq!(
        r1["failures"].as_array().map(|f| f.len()),
        Some(0),
        "a consistent chain reports no failures: {report:?}",
    );
    let events = r1["events"].as_u64().expect("events is u64");
    assert!(
        events > baseline_events,
        "granting a capability must add anchored audit events ({baseline_events} -> {events}): {report:?}",
    );
    assert_eq!(
        r1["root_hash_hex"].as_str().map(|h| h.len()),
        Some(64),
        "the integrity root is a 64-hex sha256 chain hash: {report:?}",
    );
    assert_ne!(
        r1["root_hash_hex"].as_str(),
        Some(baseline_root.as_str()),
        "the integrity root must advance as events are appended, not stay frozen: {report:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
