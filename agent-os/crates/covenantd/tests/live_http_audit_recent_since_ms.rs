//! Live HTTP coverage for `GET /audit/recent?since_ms`.
//!
//! Authenticates as the operator and grants two distinct capabilities
//! so that the audit feed contains two `capability_granted` rows with
//! strictly different `timestamp_ms` values. The HTTP path is then
//! exercised four times:
//!
//! 1. No filter — both grant rows present (no-filter baseline so a
//!    regression where `since_ms` is silently ignored cannot pass by
//!    matching the unfiltered shape).
//! 2. `since_ms=<midpoint>` where midpoint is derived from the actual
//!    timestamps captured from the unfiltered feed — not a wall-clock
//!    guess — so a slow CI host cannot flake the boundary. The older
//!    row must be dropped, the newer row must survive.
//! 3. `since_ms=<newer_ts + 1>` — events array must contain neither
//!    grant row (the post-newest no-match edge).
//! 4. `since_ms=<older_ts>` — boundary is inclusive at `>=`, so the
//!    older row must remain.
//!
//! Assertions key off `(kind, action)` identity from the response's
//! `events[].kind` payload rather than counts, so a wrong-row leaking
//! across filters fails the test instead of passing vacuously.
//!
//! The HTTP envelope serializes `Response::AuditEvents` as
//! `{ "kind": "audit_events", "events": [...] }` — the `since_ms`
//! echo lives on the CLI-side `audit_recent` wrapper, not the IPC
//! response, so this test deliberately does not assert on it.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_audit_recent_since_ms -- --ignored live_`.

use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().unwrap().port()
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
#[ignore = "live: spawns covenantd + asserts GET /audit/recent?since_ms drops older rows by actual timestamp"]
async fn live_http_audit_recent_since_ms_drops_older_events() {
    let home = tempfile::tempdir().expect("tempdir");
    let port = pick_free_port();
    let base = format!("http://127.0.0.1:{port}");
    let exe = env!("CARGO_BIN_EXE_covenantd");

    let mut child = Command::new(exe)
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
    wait_for_http(&base).await;

    let token = read_operator_token(home.path()).await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .expect("reqwest client");

    let older_action = "tool.call.echo";
    let newer_action = "tool.call.ping";

    let older_grant: Value = client
        .post(format!("{base}/capabilities/grant"))
        .json(&json!({ "action": older_action }))
        .send()
        .await
        .expect("older grant request")
        .json()
        .await
        .expect("older grant json");
    assert_eq!(older_grant["kind"], "capability_granted");

    sleep(Duration::from_millis(50)).await;

    let newer_grant: Value = client
        .post(format!("{base}/capabilities/grant"))
        .json(&json!({ "action": newer_action }))
        .send()
        .await
        .expect("newer grant request")
        .json()
        .await
        .expect("newer grant json");
    assert_eq!(newer_grant["kind"], "capability_granted");

    let unfiltered: Value = client
        .get(format!("{base}/audit/recent?limit=20"))
        .send()
        .await
        .expect("unfiltered audit recent request")
        .json()
        .await
        .expect("unfiltered audit recent json");
    assert_eq!(unfiltered["kind"], "audit_events");
    assert!(
        action_present(&unfiltered, older_action) && action_present(&unfiltered, newer_action),
        "no-filter baseline must surface both grant rows so a regression that ignores since_ms cannot pass: {unfiltered:?}",
    );

    let older_ts = timestamp_for_action(&unfiltered, older_action);
    let newer_ts = timestamp_for_action(&unfiltered, newer_action);
    assert!(
        newer_ts > older_ts,
        "audit rows must be monotonically timestamped for this test to discriminate: older={older_ts} newer={newer_ts}",
    );

    let midpoint = older_ts + (newer_ts - older_ts) / 2 + 1;
    assert!(midpoint > older_ts && midpoint <= newer_ts);

    let filtered: Value = client
        .get(format!("{base}/audit/recent?limit=20&since_ms={midpoint}"))
        .send()
        .await
        .expect("filtered audit recent request")
        .json()
        .await
        .expect("filtered audit recent json");
    assert_eq!(filtered["kind"], "audit_events");
    assert!(
        action_present(&filtered, newer_action),
        "newer row must survive since_ms midpoint over HTTP: {filtered:?}",
    );
    assert!(
        !action_present(&filtered, older_action),
        "older row must be filtered out by since_ms midpoint over HTTP: {filtered:?}",
    );

    let past: Value = client
        .get(format!(
            "{base}/audit/recent?limit=20&since_ms={}",
            newer_ts + 1
        ))
        .send()
        .await
        .expect("past audit recent request")
        .json()
        .await
        .expect("past audit recent json");
    assert_eq!(past["kind"], "audit_events");
    assert!(
        !action_present(&past, older_action) && !action_present(&past, newer_action),
        "since_ms past the newest grant timestamp must drop both grant rows over HTTP: {past:?}",
    );

    let inclusive: Value = client
        .get(format!("{base}/audit/recent?limit=20&since_ms={older_ts}"))
        .send()
        .await
        .expect("inclusive audit recent request")
        .json()
        .await
        .expect("inclusive audit recent json");
    assert_eq!(inclusive["kind"], "audit_events");
    assert!(
        action_present(&inclusive, older_action) && action_present(&inclusive, newer_action),
        "since_ms equal to the older row's timestamp must preserve it (boundary inclusive) over HTTP: {inclusive:?}",
    );

    let _ = child.kill().await;
}
