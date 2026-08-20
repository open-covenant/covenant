//! Live HTTP coverage for the buffered `GET /memory/recent` JSON path.
//!
//! `memory_recent` (http.rs:445 -> `Request::RecentMemory` -> `recent_memory`
//! lib.rs:5287) is dual-mode: `Accept: text/event-stream` streams, anything
//! else returns the buffered `Response::Memories` envelope. The SSE branch and
//! the buffered branch's *empty* page are covered by `live_http_memory_sse.rs`,
//! whose header notes it could not seed records ("would require a CLI verb that
//! does not exist yet"). But `live_http_memory_search.rs` shows a working-tier
//! record can be seeded over the gateway via `POST /intent`, so the buffered
//! read of NON-empty data — tier filtering, the limit bound, owner scoping, and
//! record content — is still unproven over HTTP. A daemon that ignored the
//! `tier` or `limit` query param, or mis-scoped ownership, would pass the
//! empty-page coverage.
//!
//! Driving the gateway exclusively, the operator self-grants `memory.read` +
//! `memory.write`, seeds two distinct working-tier echo records via `POST
//! /intent`, then over `GET /memory/recent` with `Accept: application/json`:
//!  - `?tier=working` returns both seeded records; every row is `tier:
//!    "working"` and owned by the caller, and both marker texts are present —
//!    pinning tier inclusion, owner scoping, and content.
//!  - `?tier=episodic` returns neither marker — pinning that the tier filter
//!    excludes other tiers rather than being ignored.
//!  - `?tier=working&limit=1` returns exactly one row — pinning the limit bound
//!    against a daemon that returns the whole tier.
//!
//! Hermetic — pinned to the mock embedder via `secrets.toml` so the seed is
//! deterministic with no Ollama dependency. `#[ignore]`'d, own tempdir per test.
//! Run with `cargo test -p covenantd --test live_http_memory_recent_tier_limit
//! -- --ignored live_`.

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

/// The operator's identity public key in base58 wire form — the owner stamped
/// on records it writes.
fn operator_pubkey_b58(home: &Path) -> String {
    let bytes = covenant_identity::LocalIdentity::load_or_create(
        &home.join("identity").join("local.key"),
        "user@local",
    )
    .expect("load identity")
    .pubkey_bytes();
    covenant_types::AgentId::new("user@local", bytes).pubkey_base58()
}

/// Spawn covenantd pinned to the mock embedder so the seeded echo is stored
/// deterministically on any host.
async fn spawn_http_daemon(home: &Path) -> (Child, String) {
    std::fs::write(home.join("secrets.toml"), b"[embed]\nprovider = \"mock\"\n")
        .expect("write secrets.toml");
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

/// Submit one intent over HTTP. With no `Accept: text/event-stream` the unary
/// path awaits the memory put before responding, so the echo record exists once
/// this returns. No agent is configured, so the router stores
/// `phase 0 echo (no agent matched): <text>` at the working tier, owned by the
/// caller.
async fn submit_intent(client: &reqwest::Client, base: &str, text: &str) {
    let intent: Value = client
        .post(format!("{base}/intent"))
        .json(&json!({ "text": text }))
        .send()
        .await
        .expect("post intent")
        .json()
        .await
        .expect("intent body");
    assert_eq!(
        intent["kind"], "intent_result",
        "the no-match intent must dispatch so the echo memory is stored: {intent:?}",
    );
}

/// Buffered (non-SSE) `GET /memory/recent` read. `Accept: application/json`
/// keeps the daemon off the streaming branch.
async fn recent(client: &reqwest::Client, base: &str, query: &str) -> Value {
    let body: Value = client
        .get(format!("{base}/memory/recent{query}"))
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .expect("get memory/recent")
        .json()
        .await
        .expect("memory/recent body");
    assert_eq!(
        body["kind"], "memories",
        "the buffered recent read must be a memories envelope: {body:?}",
    );
    body
}

fn record_texts(body: &Value) -> Vec<String> {
    body["records"]
        .as_array()
        .expect("records array")
        .iter()
        .map(|r| r["text"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts buffered GET /memory/recent filters by tier, scopes to the owner, carries content, and bounds by ?limit over HTTP"]
async fn live_http_memory_recent_buffered_filters_tier_and_bounds_limit_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    grant(&client, &base, "memory.read").await;
    grant(&client, &base, "memory.write").await;

    // Two distinct working-tier echoes. Two records make the limit bound below
    // meaningful: ?limit=1 must drop one.
    let echo_alpha = "phase 0 echo (no agent matched): recent-http-tier-probe-alpha";
    let echo_beta = "phase 0 echo (no agent matched): recent-http-tier-probe-beta";
    submit_intent(&client, &base, "recent-http-tier-probe-alpha").await;
    submit_intent(&client, &base, "recent-http-tier-probe-beta").await;

    // Affirmative working-tier read: poll until both seeded records surface.
    let expected_owner = operator_pubkey_b58(home.path());
    let mut working = recent(&client, &base, "?tier=working").await;
    for _ in 0..50 {
        let texts = record_texts(&working);
        if texts.iter().any(|t| t == echo_alpha) && texts.iter().any(|t| t == echo_beta) {
            break;
        }
        sleep(Duration::from_millis(50)).await;
        working = recent(&client, &base, "?tier=working").await;
    }
    let rows = working["records"].as_array().expect("records array");
    assert!(
        !rows.is_empty(),
        "the seeded working-tier records must surface: {working:?}",
    );
    // Every returned row is working-tier and owned by the caller: a daemon that
    // ignored the tier filter or dropped the owner scope fails here.
    for row in rows {
        assert_eq!(
            row["tier"], "working",
            "every row under ?tier=working must be working-tier: {row:?}",
        );
        assert_eq!(
            row["owner"]["pubkey"].as_str(),
            Some(expected_owner.as_str()),
            "every returned record must be owned by the caller: {row:?}",
        );
    }
    let texts = record_texts(&working);
    assert!(
        texts.iter().any(|t| t == echo_alpha),
        "the alpha echo must be present in the working-tier read: {working:?}",
    );
    assert!(
        texts.iter().any(|t| t == echo_beta),
        "the beta echo must be present in the working-tier read: {working:?}",
    );

    // Tier exclusion: the working records must NOT appear under a different
    // tier. A daemon that ignores the tier param returns them and fails here.
    let episodic = recent(&client, &base, "?tier=episodic").await;
    let episodic_texts = record_texts(&episodic);
    assert!(
        !episodic_texts
            .iter()
            .any(|t| t == echo_alpha || t == echo_beta),
        "working-tier records must not surface under ?tier=episodic: {episodic:?}",
    );

    // Limit bound: with two working records, ?limit=1 must return exactly one.
    let limited = recent(&client, &base, "?tier=working&limit=1").await;
    assert_eq!(
        limited["records"].as_array().map(|r| r.len()),
        Some(1),
        "?limit=1 must bound the working-tier listing to one row: {limited:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
