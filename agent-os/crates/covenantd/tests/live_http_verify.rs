//! Live HTTP coverage for `GET /verify` (`verify` -> `Request::Verify` ->
//! `verify_recent`).
//!
//! The CLI's hand-rolled `verify_report_json` shape is unit-tested, but no test
//! drives the daemon serializing `Response::VerifyReport` over the HTTP gateway.
//! That leaves the `VerifyParams` `window` query threading, the bearer-auth peer
//! identity, the `verify_report` wire variant (`window`, `checks`, `drift`,
//! `orphans_total`), the four named integrity checks, and — most importantly —
//! the report's drift *detection* unexercised on the HTTP path.
//!
//! One scenario over the gateway exclusively, with every bit of state driven
//! through the public HTTP API rather than reaching around it. A fresh daemon's
//! report is clean: `kind: "verify_report"`, the default `window` of 100, the
//! four checks (`memory ↔ audit`, `memory parent references`,
//! `capability ↔ audit`, `memory ↔ receipts`) all `passed`, an empty `drift`,
//! and `orphans_total` 0. A `?window=7` read must echo 7, proving the query is
//! threaded rather than hardcoded to the default. Then drift is induced without
//! an out-of-band write: granting `audit.purge` writes a `CapabilityGranted`
//! audit row (the `capability ↔ audit` check still passes, correlating the live
//! grant against its row), and purging the audit log removes that row while the
//! `SignedCapability` persists — orphaning it. The next report must flip
//! `capability ↔ audit` to `passed: false` with a `capability_without_audit`
//! drift entry whose `id` is the granted `signature_b58`, `orphans_total >= 1`,
//! and the three unrelated checks still `passed` (the failure is targeted, not a
//! blanket regression).
//!
//! Hermetic — no external services. `#[ignore]`'d. Each test uses its own
//! tempdir to stay safe under `--test-threads`. Run with
//! `cargo test -p covenantd --test live_http_verify -- --ignored live_`.

use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::process::{Child, Command};
use tokio::time::sleep;

const CHECK_NAMES: [&str; 4] = [
    "memory ↔ audit",
    "memory parent references",
    "capability ↔ audit",
    "memory ↔ receipts",
];

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
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

async fn verify_report(client: &reqwest::Client, base: &str, query: &str) -> Value {
    client
        .get(format!("{base}/verify{query}"))
        .send()
        .await
        .expect("get verify")
        .json()
        .await
        .expect("verify body json")
}

fn check<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .expect("checks array")
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("verify report is missing the {name:?} check: {report:?}"))
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts GET /verify renders verify_report, threads ?window, and detects capability↔audit drift over HTTP"]
async fn live_http_verify_reports_clean_state_then_detects_capability_audit_drift_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    // Clean baseline: a fresh daemon owns no orphaned provenance, so the report
    // is the verify_report variant with the default window, all four named
    // checks passing, empty drift, and zero orphans. Pinning the variant slug
    // and the exact check-name set is the forcing function against a silent
    // schema drift between the HTTP serializer and the documented shape.
    let clean = verify_report(&client, &base, "").await;
    assert_eq!(
        clean["kind"], "verify_report",
        "the report must be the verify_report variant: {clean:?}",
    );
    assert_eq!(
        clean["window"], 100,
        "an omitted ?window must default to 100: {clean:?}",
    );
    assert_eq!(
        clean["drift"].as_array().map(|d| d.len()),
        Some(0),
        "a clean daemon has no drift: {clean:?}",
    );
    assert_eq!(
        clean["orphans_total"], 0,
        "a clean daemon has no orphans: {clean:?}",
    );
    for name in CHECK_NAMES {
        assert_eq!(
            check(&clean, name)["passed"],
            true,
            "the {name:?} check must pass on a clean daemon: {clean:?}",
        );
    }

    // Window threading: a regression that ignores the query and always reports
    // the hardcoded default would pass the baseline above; requesting ?window=7
    // and asserting the echo proves the parameter reaches verify_recent.
    let windowed = verify_report(&client, &base, "?window=7").await;
    assert_eq!(
        windowed["window"], 7,
        "the ?window=7 query must thread through to the report, not stay at the default 100: {windowed:?}",
    );

    // Grant audit.purge over HTTP. This is the capability the next step needs,
    // and the grant writes a CapabilityGranted audit row, so the
    // capability ↔ audit check must still pass — proving the correlation runs
    // over a real granted capability rather than vacuously on an empty set.
    let grant: Value = client
        .post(format!("{base}/capabilities/grant"))
        .json(&json!({ "action": "audit.purge" }))
        .send()
        .await
        .expect("grant audit.purge")
        .json()
        .await
        .expect("grant body json");
    assert_eq!(
        grant["kind"], "capability_granted",
        "the operator grant must succeed: {grant:?}",
    );
    let signature_b58 = grant["signature_b58"]
        .as_str()
        .expect("grant carries signature_b58")
        .to_string();

    let correlated = verify_report(&client, &base, "").await;
    assert_eq!(
        check(&correlated, "capability ↔ audit")["passed"],
        true,
        "the freshly granted capability still has its grant audit row, so the check passes: {correlated:?}",
    );
    assert_eq!(
        correlated["drift"].as_array().map(|d| d.len()),
        Some(0),
        "a consistent grant introduces no drift: {correlated:?}",
    );

    // Purge the audit log. The grant's CapabilityGranted row is removed while
    // the SignedCapability persists in the capability store — the capability is
    // now orphaned with no provenance, exactly the out-of-band-mutation shape
    // the report exists to surface during incident triage.
    let purged: Value = client
        .post(format!("{base}/audit/purge"))
        .json(&json!({ "before_ms": epoch_ms().saturating_add(1_000) }))
        .send()
        .await
        .expect("purge audit")
        .json()
        .await
        .expect("purge body json");
    assert_eq!(
        purged["kind"], "audit_purged",
        "the granted audit.purge must let the purge through: {purged:?}",
    );
    assert!(
        purged["purged"].as_u64().unwrap_or(0) > 0,
        "the purge must remove at least the grant audit row: {purged:?}",
    );

    // The report must now detect the orphan: capability ↔ audit flips to false
    // with a capability_without_audit drift entry naming the granted signature,
    // orphans_total rises, and the three unrelated checks stay green so the
    // failure is provably targeted, not a blanket short-circuit.
    let drifted = verify_report(&client, &base, "").await;
    assert_eq!(
        check(&drifted, "capability ↔ audit")["passed"],
        false,
        "the orphaned capability must flip its check to failed: {drifted:?}",
    );
    for name in [
        "memory ↔ audit",
        "memory parent references",
        "memory ↔ receipts",
    ] {
        assert_eq!(
            check(&drifted, name)["passed"],
            true,
            "the {name:?} check is unrelated to the orphan and must stay passing: {drifted:?}",
        );
    }
    let drift = drifted["drift"].as_array().expect("drift array");
    let orphan = drift
        .iter()
        .find(|d| d["kind"] == "capability_without_audit")
        .unwrap_or_else(|| panic!("expected a capability_without_audit drift entry: {drifted:?}"));
    assert_eq!(
        orphan["id"].as_str(),
        Some(signature_b58.as_str()),
        "the drift entry must name the orphaned capability's signature: {drifted:?}",
    );
    assert!(
        drifted["orphans_total"].as_u64().unwrap_or(0) >= 1,
        "orphans_total must count the orphaned capability: {drifted:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
