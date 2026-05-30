//! Live HTTP coverage for `POST /chain/flush-receipts`
//! (`chain_flush_receipts`).
//!
//! Receipt batch flushing is live-tested over the CLI by
//! `live_cli_chain_flush_receipts_json.rs`, but no test drives it over the
//! HTTP gateway, leaving the `LimitParams` deserialization, the bearer-auth
//! operator identity, the `chain.flush` scope gate, and the local merkle-batch
//! confirmation unexercised on the HTTP path.
//!
//! Two scenarios:
//! - flush round-trip: an operator holding `memory.write` + `chain.flush`
//!   submits one intent (minting one settlement receipt) then POSTs a flush and
//!   receives `kind: "receipt_batch_flushed"` with `receipts_updated=1` and a
//!   local batch — `receipt_count=1`, non-empty `batch_id` + `merkle_root`, and
//!   `tx_sig`/`slot` null because nothing was anchored on-chain.
//! - scope denial: an operator holding only `memory.write` is rejected by name;
//!   a subsequent `chain.flush` grant then flushes the still-unbatched receipt
//!   (`receipts_updated=1`), proving the denied flush was a true no-op.
//!
//! Opt-in: crosses process and socket boundaries. The receipt mint is
//! independent of the embedder, so the flush count holds whether the intent
//! embeds against a local model or falls back to the zero vector. `#[ignore]`'d.
//! Each test uses its own tempdir to stay safe under `--test-threads`. Run with
//! `cargo test -p covenantd --test live_http_chain_flush_receipts -- --ignored live_`.

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

/// Spawn covenantd on a free HTTP port against `home`, waiting for both
/// its unix socket and the HTTP gateway to come up. Returns the child and
/// the gateway base URL.
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

/// Submit one intent over HTTP, minting a single operator-owned settlement
/// receipt. No `Accept: text/event-stream`, so the daemon answers with the
/// unary `intent_result` envelope rather than an SSE stream.
async fn submit_intent(client: &reqwest::Client, base: &str) {
    let intent: Value = client
        .post(format!("{base}/intent"))
        .json(&json!({ "text": "flush receipts http probe" }))
        .send()
        .await
        .expect("post intent")
        .json()
        .await
        .expect("intent body");
    assert_eq!(
        intent["kind"], "intent_result",
        "intent submission must succeed so a receipt exists to flush: {intent:?}",
    );
}

async fn flush(client: &reqwest::Client, base: &str) -> Value {
    client
        .post(format!("{base}/chain/flush-receipts"))
        .json(&json!({ "limit": 10 }))
        .send()
        .await
        .expect("post flush-receipts")
        .json()
        .await
        .expect("flush body")
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts POST /chain/flush-receipts batches one receipt into a local merkle batch over HTTP"]
async fn live_http_chain_flush_receipts_round_trips_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    grant(&client, &base, "memory.write").await;
    grant(&client, &base, "chain.flush").await;
    submit_intent(&client, &base).await;

    let flushed = flush(&client, &base).await;
    assert_eq!(
        flushed["kind"], "receipt_batch_flushed",
        "granted flush must be accepted over HTTP: {flushed:?}",
    );
    assert_eq!(
        flushed["receipts_updated"].as_u64(),
        Some(1),
        "the one minted receipt must be batched: {flushed:?}",
    );
    assert_eq!(
        flushed["batch"]["receipt_count"].as_u64(),
        Some(1),
        "the batch must cover the one receipt: {flushed:?}",
    );
    assert!(
        flushed["batch"]["batch_id"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "batch must carry an id: {flushed:?}",
    );
    assert!(
        flushed["batch"]["merkle_root"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "batch must carry a merkle root: {flushed:?}",
    );
    assert!(
        flushed["batch"]["tx_sig"].is_null(),
        "a local flush must not claim an on-chain signature: {flushed:?}",
    );
    assert!(
        flushed["batch"]["slot"].is_null(),
        "a local flush must not claim an on-chain slot: {flushed:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts a chain.flush-less POST /chain/flush-receipts is rejected and batches nothing"]
async fn live_http_chain_flush_receipts_requires_grant_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    // Mint a receipt but withhold chain.flush. The flush must be rejected by
    // name — a generic auth failure or a silent no-op would give false
    // assurance about scope discipline on the HTTP path.
    grant(&client, &base, "memory.write").await;
    submit_intent(&client, &base).await;

    let rejected = flush(&client, &base).await;
    assert_eq!(
        rejected["kind"], "error",
        "an ungranted flush must be rejected: {rejected:?}",
    );
    let message = rejected["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("chain.flush"),
        "rejection must name the missing capability: {rejected:?}",
    );
    assert!(
        message.contains("requires capability"),
        "rejection must surface the requires-capability prefix: {rejected:?}",
    );

    // Granting chain.flush now must still find the receipt unbatched — proving
    // the denied flush touched no settlement state.
    grant(&client, &base, "chain.flush").await;
    let flushed = flush(&client, &base).await;
    assert_eq!(
        flushed["kind"], "receipt_batch_flushed",
        "the granted flush must now succeed: {flushed:?}",
    );
    assert_eq!(
        flushed["receipts_updated"].as_u64(),
        Some(1),
        "the denied flush must have left the receipt unbatched: {flushed:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
