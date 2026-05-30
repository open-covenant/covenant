//! Live HTTP coverage for `GET /chain/receipt-batches` (`chain_receipt_batches`).
//!
//! Receipt-batch listing is live-tested over the CLI by
//! `live_cli_receipt_batches_json.rs`, but no test drives it over the HTTP
//! gateway. The two wire shapes diverge on purpose: the CLI wraps the daemon
//! response as `{ "kind": "receipt_batch_list", "limit": N, "batches": [...] }`,
//! whereas the raw daemon `Response::ReceiptBatches` serializes as
//! `{ "kind": "receipt_batches", "batches": [...] }` with no top-level `limit`.
//! That leaves the `LimitParams` query deserialization, the bearer-auth peer
//! identity, the `chain.batches` scope gate, the payer-pubkey row scoping, and
//! the batch aggregation unexercised on the HTTP path.
//!
//! Two scenarios:
//! - read round-trip: an operator holding `memory.write` + `chain.flush` +
//!   `chain.batches` submits one intent (minting one receipt), flushes it into a
//!   local merkle batch, then GETs `/chain/receipt-batches` and receives
//!   `kind: "receipt_batches"` carrying exactly that batch — `receipt_count=1`,
//!   a `batch_id` matching the flush response, a non-empty `merkle_root`, and
//!   `tx_sig`/`slot` null because nothing was anchored on-chain. The body must
//!   carry no top-level `limit`, pinning the raw daemon wire against the CLI
//!   remap so a dispatch collapse cannot pass by matching the CLI shape.
//! - scope denial: an operator holding `memory.write` + `chain.flush` but not
//!   `chain.batches` flushes a receipt, then is rejected by name on the read.
//!   Granting `chain.batches` reveals the same batch, proving the denial was an
//!   authorization block rather than absent settlement state.
//!
//! Hermetic — no external services. The receipt mint is independent of the
//! embedder, so the batch holds whether the intent embeds against a local model
//! or falls back to the zero vector. `#[ignore]`'d. Each test uses its own
//! tempdir to stay safe under `--test-threads`. Run with
//! `cargo test -p covenantd --test live_http_chain_receipt_batches -- --ignored live_`.

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

/// Spawn covenantd on a free HTTP port against `home`, waiting for both its
/// unix socket and the HTTP gateway to come up. Returns the child and the
/// gateway base URL.
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
        .json(&json!({ "text": "receipt batches http probe" }))
        .send()
        .await
        .expect("post intent")
        .json()
        .await
        .expect("intent body");
    assert_eq!(
        intent["kind"], "intent_result",
        "intent submission must succeed so a receipt exists to batch: {intent:?}",
    );
}

/// Flush pending receipts into a local merkle batch and return the response.
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

/// Read recent receipt batches over the gateway.
async fn list_batches(client: &reqwest::Client, base: &str) -> Value {
    client
        .get(format!("{base}/chain/receipt-batches?limit=10"))
        .send()
        .await
        .expect("get receipt-batches")
        .json()
        .await
        .expect("receipt-batches body")
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts GET /chain/receipt-batches lists the flushed batch over HTTP"]
async fn live_http_chain_receipt_batches_lists_flushed_batch_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    grant(&client, &base, "memory.write").await;
    grant(&client, &base, "chain.flush").await;
    grant(&client, &base, "chain.batches").await;
    submit_intent(&client, &base).await;

    // A freshly minted receipt is unbatched (`batch_id` is None) and the read
    // handler skips it; flushing assigns the batch the read must surface.
    let flushed = flush(&client, &base).await;
    assert_eq!(
        flushed["kind"], "receipt_batch_flushed",
        "the receipt must batch so a summary exists to read: {flushed:?}",
    );
    let batch_id = flushed["batch"]["batch_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .expect("flush must return a non-empty batch id")
        .to_string();

    let listed = list_batches(&client, &base).await;
    assert_eq!(
        listed["kind"], "receipt_batches",
        "the raw daemon wire must be `receipt_batches`, not the CLI's `receipt_batch_list`: {listed:?}",
    );
    assert!(
        listed.get("limit").is_none(),
        "the HTTP body must not carry the CLI-only top-level `limit` field: {listed:?}",
    );
    let batches = listed["batches"].as_array().expect("batches array");
    assert_eq!(
        batches.len(),
        1,
        "the one flushed batch must be the only summary returned: {listed:?}",
    );
    let batch = &batches[0];
    assert_eq!(
        batch["batch_id"].as_str(),
        Some(batch_id.as_str()),
        "the listed batch must be the batch the flush created: {listed:?}",
    );
    assert_eq!(
        batch["receipt_count"].as_u64(),
        Some(1),
        "the batch must aggregate the one minted receipt: {listed:?}",
    );
    assert!(
        batch["merkle_root"].as_str().is_some_and(|s| !s.is_empty()),
        "the batch must carry a merkle root: {listed:?}",
    );
    assert!(
        batch["tx_sig"].is_null(),
        "a local batch must not claim an on-chain signature: {listed:?}",
    );
    assert!(
        batch["slot"].is_null(),
        "a local batch must not claim an on-chain slot: {listed:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts a chain.batches-less GET /chain/receipt-batches is rejected by name then reads after grant"]
async fn live_http_chain_receipt_batches_requires_grant_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    // Flush a receipt into a batch, then read it back without chain.batches. A
    // readable batch exists, so a rejection proves the gate blocks the read
    // rather than the batch simply being absent.
    grant(&client, &base, "memory.write").await;
    grant(&client, &base, "chain.flush").await;
    submit_intent(&client, &base).await;
    let flushed = flush(&client, &base).await;
    assert_eq!(
        flushed["kind"], "receipt_batch_flushed",
        "setup flush must batch the receipt so there is state to withhold: {flushed:?}",
    );

    let rejected = list_batches(&client, &base).await;
    assert_eq!(
        rejected["kind"], "error",
        "an ungranted batch read must be rejected: {rejected:?}",
    );
    let message = rejected["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("chain.batches"),
        "rejection must name the missing capability: {rejected:?}",
    );
    assert!(
        message.contains("receipt batch reads require capability"),
        "rejection must surface the receipt-batch require-capability message: {rejected:?}",
    );

    // Granting chain.batches must reveal the same batch — proving the denied
    // read was an authorization block, not missing settlement state.
    grant(&client, &base, "chain.batches").await;
    let listed = list_batches(&client, &base).await;
    assert_eq!(
        listed["kind"], "receipt_batches",
        "the granted batch read must now succeed: {listed:?}",
    );
    assert_eq!(
        listed["batches"].as_array().map(|b| b.len()),
        Some(1),
        "the batch withheld by the denied read must now be visible: {listed:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
