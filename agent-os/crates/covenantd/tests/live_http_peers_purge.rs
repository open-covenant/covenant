//! Live HTTP coverage for `POST /peers/purge` (`peers_purge`).
//!
//! Peer purge is live-tested over the CLI by `live_cli_peers_purge_json.rs`,
//! and its delegated-denial edge over HTTP by
//! `live_http_peers_lifecycle_delegated_denial.rs`, but no test drives the
//! operator allow-path over the gateway — leaving the `peers.purge` gate, the
//! `before_ms` scope check, and the actual tombstone drop unexercised on the
//! HTTP path for an authorised operator.
//!
//! A revoked tombstone is created hermetically by rotating the operator token:
//! `rotate_operator_token` revokes the old token, so a single `/peers/rotate`
//! leaves exactly one revoked entry for the purge to reap. Rotation also
//! revokes the bootstrap bearer, so each test swaps to the freshly minted
//! token before continuing.
//!
//! Two scenarios:
//! - purge round-trip: an operator holding `peers.purge` POSTs a purge that
//!   covers every revoked tombstone and receives `kind: "peers_purged"` with
//!   `purged >= 1`.
//! - scope denial: the operator without the grant is rejected by name
//!   (`peers.purge`) and the tombstone survives — proven by a follow-up
//!   granted purge that still reaps it.
//!
//! Hermetic — no external services. `#[ignore]`'d. Each test uses its own
//! tempdir to stay safe under `--test-threads`. Run with
//! `cargo test -p covenantd --test live_http_peers_purge -- --ignored live_`.

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

fn bearer_client(token: &str) -> reqwest::Client {
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

/// Rotate the operator token over HTTP, leaving one revoked tombstone (the old
/// token) for the purge to reap, and return the freshly minted bearer. The
/// `client` must still carry the pre-rotation bearer.
async fn rotate_for_tombstone(client: &reqwest::Client, base: &str) -> String {
    let rotated: Value = client
        .post(format!("{base}/peers/rotate"))
        .send()
        .await
        .expect("post rotate")
        .json()
        .await
        .expect("rotate body");
    assert_eq!(
        rotated["kind"], "operator_token_rotated",
        "rotation must succeed to create a revoked tombstone: {rotated:?}",
    );
    rotated["token_b58"]
        .as_str()
        .expect("rotated body must carry token_b58")
        .to_string()
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

// Purge every revoked tombstone regardless of age. A bare grant carries an
// empty scope, which accepts any `before_ms`, so the maximum reaps all.
const PURGE_ALL: u64 = u64::MAX;

async fn purge_peers(client: &reqwest::Client, base: &str) -> Value {
    client
        .post(format!("{base}/peers/purge"))
        .json(&json!({ "before_ms": PURGE_ALL }))
        .send()
        .await
        .expect("post peers purge")
        .json()
        .await
        .expect("peers purge body")
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts POST /peers/purge reaps a revoked tombstone over HTTP"]
async fn live_http_peers_purge_round_trips_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let bootstrap = read_operator_token(home.path()).await;

    let new_token = rotate_for_tombstone(&bearer_client(&bootstrap), &base).await;
    let client = bearer_client(&new_token);

    grant(&client, &base, "peers.purge").await;

    let purged = purge_peers(&client, &base).await;
    assert_eq!(
        purged["kind"], "peers_purged",
        "granted purge must be accepted over HTTP: {purged:?}",
    );
    assert!(
        purged["purged"].as_u64().unwrap_or(0) >= 1,
        "purge must reap the revoked tombstone the rotation created: {purged:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts a no-grant POST /peers/purge is rejected and leaves the tombstone"]
async fn live_http_peers_purge_requires_grant_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let bootstrap = read_operator_token(home.path()).await;

    let new_token = rotate_for_tombstone(&bearer_client(&bootstrap), &base).await;
    let client = bearer_client(&new_token);

    // No grant. The operator authenticates but lacks peers.purge, so the purge
    // must be rejected by name before any tombstone is reaped.
    let rejected = purge_peers(&client, &base).await;
    assert_eq!(
        rejected["kind"], "error",
        "an ungranted purge must be rejected: {rejected:?}",
    );
    assert!(
        rejected["message"]
            .as_str()
            .unwrap_or_default()
            .contains("peers.purge"),
        "rejection must name the missing capability: {rejected:?}",
    );

    // The denied purge must have left the tombstone intact: granting the
    // capability and retrying still reaps it.
    grant(&client, &base, "peers.purge").await;
    let purged = purge_peers(&client, &base).await;
    assert_eq!(
        purged["kind"], "peers_purged",
        "the granted retry must be accepted: {purged:?}",
    );
    assert!(
        purged["purged"].as_u64().unwrap_or(0) >= 1,
        "the tombstone must survive a denied purge and be reaped once granted: {purged:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
