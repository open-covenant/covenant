//! Live HTTP coverage for `GET /capabilities/recent` (`capabilities_recent` ->
//! `Request::RecentCapabilities` -> `recent_capabilities`).
//!
//! Recent-capability listing ships and is unit-tested in-process, but no test
//! drives it over the HTTP gateway. That leaves the `LimitParams` query, the
//! bearer-auth peer identity, the `subject == peer || granted_by == peer` row
//! scoping (`recent_capabilities` at lib.rs:6449), the non-draining listing
//! semantics, and the `Capabilities` wire variant (`kind: "capabilities"`, each
//! row a `SignedCapability` with a `capability` object and a base58 `signature`)
//! unexercised on the HTTP path.
//!
//! One scenario over the gateway exclusively, with every capability seeded
//! through the public grant API. A fresh daemon lists nothing. Granting
//! `memory.read` as the operator must surface exactly that capability — the
//! listed row's `signature` equals the grant's `signature_b58` and its
//! `capability.action` is `memory.read` — proving both the wire shape and that
//! the operator sees its own grant (the keep-own side of the scoping filter; an
//! inverted filter would hide it and the count would be 0). A second read must
//! still carry it (a non-draining view, not a queue pop). Granting a second
//! action then reading `?limit=1` must return one row while `?limit=10` returns
//! both, pinning the limit bound.
//!
//! The cross-peer leak side of the scoping filter (a second identity's
//! capability must NOT appear) needs a second authenticated peer and is left as
//! the recorded uncovered case.
//!
//! Hermetic — no external services. `#[ignore]`'d. Each test uses its own
//! tempdir to stay safe under `--test-threads`. Run with
//! `cargo test -p covenantd --test live_http_capabilities_recent -- --ignored live_`.

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

async fn grant(client: &reqwest::Client, base: &str, action: &str) -> Value {
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
    granted
}

async fn recent_caps(client: &reqwest::Client, base: &str, query: &str) -> Value {
    client
        .get(format!("{base}/capabilities/recent{query}"))
        .send()
        .await
        .expect("get capabilities recent")
        .json()
        .await
        .expect("recent capabilities body")
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts GET /capabilities/recent lists granted caps, scopes to the peer, and bounds by ?limit over HTTP"]
async fn live_http_capabilities_recent_lists_granted_caps_without_draining_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    // Empty baseline: a `capabilities` envelope with no rows before any grant,
    // so a regression that always returns the populated shape cannot pass by
    // matching it, and a wrong-variant regression is caught by the kind slug.
    let empty = recent_caps(&client, &base, "?limit=10").await;
    assert_eq!(
        empty["kind"], "capabilities",
        "recent capabilities must be a capabilities envelope: {empty:?}",
    );
    assert_eq!(
        empty["capabilities"].as_array().map(|c| c.len()),
        Some(0),
        "no capability has been granted yet, so the listing must be empty: {empty:?}",
    );

    // Grant memory.read as the operator. The operator is the capability's
    // subject, so the keep-own side of the scoping filter must surface it.
    let granted = grant(&client, &base, "memory.read").await;
    let signature_b58 = granted["signature_b58"]
        .as_str()
        .expect("grant carries signature_b58")
        .to_string();

    let listed = recent_caps(&client, &base, "?limit=10").await;
    let rows = listed["capabilities"]
        .as_array()
        .expect("capabilities array");
    assert_eq!(
        rows.len(),
        1,
        "the operator's own grant must be listed (an inverted scope filter would hide it): {listed:?}",
    );
    assert_eq!(
        rows[0]["signature"].as_str(),
        Some(signature_b58.as_str()),
        "the listed row's base58 signature must equal the grant's signature_b58: {listed:?}",
    );
    assert_eq!(
        rows[0]["capability"]["action"], "memory.read",
        "the listed capability must carry the granted action: {listed:?}",
    );

    // Reading again must still surface the row: the recent listing is a
    // non-draining view, not a consuming read.
    let relisted = recent_caps(&client, &base, "?limit=10").await;
    assert_eq!(
        relisted["capabilities"].as_array().map(|c| c.len()),
        Some(1),
        "the listing must not drain capability state on read: {relisted:?}",
    );

    // A second grant lets the limit bound be exercised: ?limit=1 returns one
    // row, ?limit=10 returns both. A regression that ignores the limit would
    // return both for ?limit=1.
    grant(&client, &base, "audit.read").await;
    let limited = recent_caps(&client, &base, "?limit=1").await;
    assert_eq!(
        limited["capabilities"].as_array().map(|c| c.len()),
        Some(1),
        "?limit=1 must bound the listing to one row: {limited:?}",
    );
    let both = recent_caps(&client, &base, "?limit=10").await;
    assert_eq!(
        both["capabilities"].as_array().map(|c| c.len()),
        Some(2),
        "?limit=10 must surface both grants: {both:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
