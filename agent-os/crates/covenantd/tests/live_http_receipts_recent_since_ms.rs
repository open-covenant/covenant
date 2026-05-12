//! Live HTTP coverage for `GET /receipts/recent?since_ms`.
//!
//! Authenticates as the operator, grants `memory.write`, `memory.read`,
//! and `chain.receipts`, and submits two intents 60 ms apart so the
//! settlement layer records two receipts with strictly different
//! `settled_at` values. The HTTP path is then exercised four times:
//!
//! 1. No filter — both settlement rows present (no-filter baseline so
//!    a regression where `since_ms` is silently ignored cannot pass by
//!    matching the unfiltered shape).
//! 2. `since_ms=<midpoint>` where midpoint is derived from the actual
//!    `settled_at` values captured from the unfiltered feed — not a
//!    wall-clock guess — so a slow CI host cannot flake the boundary.
//!    The older row must be dropped, the newer row must survive.
//! 3. `since_ms=<newer_ts + 1>` — receipts array must contain neither
//!    settlement row (the post-newest no-match edge).
//! 4. `since_ms=<older_ts>` — boundary is inclusive at `>=`, so the
//!    older row must remain.
//!
//! Receipts are identified by `settled_at` directly since two intents
//! dispatched 60 ms apart produce distinct timestamps that
//! disambiguate cleanly.
//!
//! The HTTP envelope serializes `Response::Receipts` as
//! `{ "kind": "receipts", "receipts": [...] }` — the `since_ms` echo
//! lives on the CLI-side `receipt_list_json` wrapper, not the IPC
//! response, so this test deliberately does not assert on it.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_receipts_recent_since_ms -- --ignored live_`.

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

fn settled_timestamps(value: &Value) -> Vec<u64> {
    value["receipts"]
        .as_array()
        .expect("receipts array")
        .iter()
        .map(|r| r["settled_at"].as_u64().expect("receipt.settled_at"))
        .collect()
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts GET /receipts/recent?since_ms drops older rows by actual settled_at"]
async fn live_http_receipts_recent_since_ms_drops_older_receipts() {
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

    for action in ["memory.write", "memory.read", "chain.receipts"] {
        let grant: Value = client
            .post(format!("{base}/capabilities/grant"))
            .json(&json!({ "action": action }))
            .send()
            .await
            .expect("grant request")
            .json()
            .await
            .expect("grant json");
        assert_eq!(
            grant["kind"], "capability_granted",
            "grant for {action} failed: {grant:?}",
        );
    }

    let _: Value = client
        .post(format!("{base}/intent"))
        .json(&json!({ "text": "receipt since-ms http probe one" }))
        .send()
        .await
        .expect("first intent request")
        .json()
        .await
        .expect("first intent json");
    sleep(Duration::from_millis(60)).await;
    let _: Value = client
        .post(format!("{base}/intent"))
        .json(&json!({ "text": "receipt since-ms http probe two" }))
        .send()
        .await
        .expect("second intent request")
        .json()
        .await
        .expect("second intent json");

    let unfiltered: Value = client
        .get(format!("{base}/receipts/recent?limit=20"))
        .send()
        .await
        .expect("unfiltered receipts request")
        .json()
        .await
        .expect("unfiltered receipts json");
    assert_eq!(unfiltered["kind"], "receipts");
    let unfiltered_ts = settled_timestamps(&unfiltered);
    assert_eq!(
        unfiltered_ts.len(),
        2,
        "no-filter baseline must surface both settlement receipts so a since_ms regression cannot pass by matching the unfiltered shape: {unfiltered:?}",
    );

    let (older_ts, newer_ts) = {
        let mut ts = unfiltered_ts.clone();
        ts.sort();
        (ts[0], ts[1])
    };
    assert!(
        newer_ts > older_ts,
        "settlement receipts must be monotonically timestamped for this test to discriminate: timestamps={unfiltered_ts:?}",
    );

    let midpoint = older_ts + (newer_ts - older_ts) / 2 + 1;
    assert!(midpoint > older_ts && midpoint <= newer_ts);

    let filtered: Value = client
        .get(format!(
            "{base}/receipts/recent?limit=20&since_ms={midpoint}"
        ))
        .send()
        .await
        .expect("filtered receipts request")
        .json()
        .await
        .expect("filtered receipts json");
    assert_eq!(filtered["kind"], "receipts");
    let filtered_ts = settled_timestamps(&filtered);
    assert!(
        filtered_ts.iter().all(|ts| *ts >= midpoint),
        "every surviving receipt must be at or above the midpoint threshold: {filtered_ts:?}",
    );
    assert!(
        filtered_ts.contains(&newer_ts),
        "newer receipt must survive since_ms midpoint over HTTP: {filtered_ts:?}",
    );
    assert!(
        !filtered_ts.contains(&older_ts),
        "older receipt must be filtered out by since_ms midpoint over HTTP: {filtered_ts:?}",
    );

    let past: Value = client
        .get(format!(
            "{base}/receipts/recent?limit=20&since_ms={}",
            newer_ts + 1
        ))
        .send()
        .await
        .expect("past receipts request")
        .json()
        .await
        .expect("past receipts json");
    assert_eq!(past["kind"], "receipts");
    let past_ts = settled_timestamps(&past);
    assert!(
        past_ts.is_empty(),
        "since_ms past the newest settled_at must drop both rows over HTTP: {past_ts:?}",
    );

    let inclusive: Value = client
        .get(format!(
            "{base}/receipts/recent?limit=20&since_ms={older_ts}"
        ))
        .send()
        .await
        .expect("inclusive receipts request")
        .json()
        .await
        .expect("inclusive receipts json");
    assert_eq!(inclusive["kind"], "receipts");
    let inclusive_ts = settled_timestamps(&inclusive);
    assert!(
        inclusive_ts.contains(&older_ts) && inclusive_ts.contains(&newer_ts),
        "since_ms equal to the older row's settled_at must preserve it (boundary inclusive) over HTTP: {inclusive_ts:?}",
    );

    let _ = child.kill().await;
}
