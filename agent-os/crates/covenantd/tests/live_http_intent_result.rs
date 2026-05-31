//! Live HTTP coverage for `GET /intents/:id/result` (`intent_result`).
//!
//! This route is HTTP-only: the handler reads `Server::intent_outcome` (an
//! in-process snapshot) directly rather than forwarding an IPC request, so no
//! CLI/socket test exercises it. It had no live coverage at all.
//!
//! The route serves the async-dispatch `OutcomeStore`, which is populated
//! *only* when a routed agent runs on the Hermes runtime and the slow build is
//! moved to a spawned task (`OutcomeStore::insert_running`, gated on
//! `Runtime::Hermes`). A no-match intent — or any non-Hermes match — runs
//! synchronously and is never stored, and a hermetic daemon bundles no agents,
//! so the populated `kind: "intent_outcome"` success body and its idempotent
//! refetch are not reachable without a registered Hermes agent plus its sandbox
//! runtime. Those (and cross-peer authorization, and `result_hash_hex` tamper
//! detection) are left uncovered here; this test pins the contract that *is*
//! hermetically reachable — the found-vs-missing boundary:
//! - unknown id: `GET /intents/<nil-uuid>/result` answers HTTP 404 with
//!   `kind: "error"` / `"unknown intent"`, not a 500 or a synthesized outcome.
//! - synchronous intent absent: a no-match intent dispatched over HTTP returns
//!   a terminal `intent_result`, yet its id is absent from the async outcome
//!   store, so `GET /intents/<that-id>/result` also 404s — guarding against a
//!   regression that leaks or synthesizes outcomes for synchronous intents.
//!
//! Hermetic — no external services, no embedder dependency (the echo dispatch
//! stores without a vector when no model is present). `#[ignore]`'d, own
//! tempdir per test for `--test-threads` safety. Run with
//! `cargo test -p covenantd --test live_http_intent_result -- --ignored live_`.

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

async fn get_result(
    client: &reqwest::Client,
    base: &str,
    id: &str,
) -> (reqwest::StatusCode, Value) {
    let response = client
        .get(format!("{base}/intents/{id}/result"))
        .send()
        .await
        .expect("get intent result");
    let status = response.status();
    let body = response.json().await.expect("intent result body");
    (status, body)
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts GET /intents/<unknown>/result is a clean 404 error over HTTP"]
async fn live_http_intent_result_unknown_id_errors_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    let (status, body) = get_result(&client, &base, "00000000-0000-0000-0000-000000000000").await;
    assert_eq!(
        status,
        reqwest::StatusCode::NOT_FOUND,
        "an unknown intent id must answer 404, not 200 or 500: {status} {body:?}",
    );
    assert_eq!(
        body["kind"], "error",
        "an unknown intent id must answer the error variant, not a synthesized outcome: {body:?}",
    );
    assert_eq!(
        body["message"], "unknown intent",
        "the not-found body must carry the unknown-intent message: {body:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts a synchronous intent's id is absent from the async outcome store over HTTP"]
async fn live_http_intent_result_synchronous_intent_absent_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    // Intent dispatch stores the phase-0 echo memory, which requires
    // memory.write; without it the submit errors before producing an id.
    grant(&client, &base, "memory.write").await;
    let intent: Value = client
        .post(format!("{base}/intent"))
        .json(&json!({ "text": "intent result http probe (no agent matches)" }))
        .send()
        .await
        .expect("post intent")
        .json()
        .await
        .expect("intent body");
    assert_eq!(
        intent["kind"], "intent_result",
        "the no-match intent must dispatch synchronously to a terminal result: {intent:?}",
    );
    let intent_id = intent["intent_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .expect("intent_result must carry the dispatched intent_id")
        .to_string();

    // The synchronous dispatch never enters the async OutcomeStore that
    // /intents/:id/result serves, so a real, just-submitted id still 404s.
    // This pins the documented store boundary: a refactor that started
    // synthesizing or leaking outcomes for synchronous intents would flip
    // this from error to intent_outcome.
    let (status, body) = get_result(&client, &base, &intent_id).await;
    assert_eq!(
        status,
        reqwest::StatusCode::NOT_FOUND,
        "a synchronous intent is absent from the async outcome store, so its id must 404: {status} {body:?}",
    );
    assert_eq!(
        body["kind"], "error",
        "the absent synchronous intent must answer the error variant: {body:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
