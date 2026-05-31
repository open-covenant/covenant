//! Live HTTP coverage for `GET /memory/search` (`memory_search`).
//!
//! The relevance query is live-tested over the CLI by
//! `live_cli_memory_search_min_relevance`, but no test drives `/memory/search`
//! over the HTTP gateway. The two wire shapes diverge on purpose: the CLI wraps
//! the daemon response as `{ "kind": "memory_read", "min_relevance": _, ... }`,
//! whereas the raw daemon `Response::Memories` serializes as
//! `{ "kind": "memories", "records": [...] }` with no top-level `limit`,
//! `min_relevance`, or query echo. That leaves the `memory.read` capability
//! gate, the bearer-auth peer identity, the owner-scoped row filter, and the
//! `Memories` wire variant unexercised on the HTTP path.
//!
//! Two scenarios:
//! - read round-trip: an operator holding `memory.read` + `memory.write` reads
//!   an empty owner-scoped store, submits a no-match intent (the router stores
//!   `phase 0 echo (no agent matched): <text>` owned by the caller), then reads
//!   it back as `kind: "memories"` carrying a record whose text is that echo
//!   and whose `owner.pubkey` is the calling operator. The body must carry no
//!   CLI-only `min_relevance`/`limit`, pinning the raw daemon wire against the
//!   CLI remap so a dispatch collapse cannot pass by matching the CLI shape.
//! - capability denial: a search without `memory.read` is rejected by name;
//!   granting `memory.read` flips the same query to an empty `memories` list,
//!   proving the denial was an authorization block rather than absent state.
//!
//! Hermetic — the daemon is pinned to the deterministic mock embedder via
//! `secrets.toml`, so the seeded echo is found on any host with no Ollama
//! dependency. `#[ignore]`'d, own tempdir per test for `--test-threads`
//! safety. Run with
//! `cargo test -p covenantd --test live_http_memory_search -- --ignored live_`.

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

/// The operator's identity public key in base58 wire form (the JSON shape of
/// `AgentId.pubkey`). `load_or_create` loads the daemon-created key, so it
/// matches the operator the bearer token resolves to — the owner stamped on
/// records it writes.
fn operator_pubkey_b58(home: &Path) -> String {
    let bytes = covenant_identity::LocalIdentity::load_or_create(
        &home.join("identity").join("local.key"),
        "user@local",
    )
    .expect("load identity")
    .pubkey_bytes();
    covenant_types::AgentId::new("user@local", bytes).pubkey_base58()
}

/// Spawn covenantd on a free HTTP port against `home`, pinned to the mock
/// embedder so the seeded echo memory is found deterministically on any host.
/// Waits for the unix socket and the HTTP gateway.
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

/// Submit one intent over HTTP. With no `Accept: text/event-stream` the daemon
/// answers the unary `intent_result` envelope, and the unary path awaits the
/// memory put before responding — so the echo record exists once this returns.
async fn submit_intent(client: &reqwest::Client, base: &str, text: &str) -> Value {
    client
        .post(format!("{base}/intent"))
        .json(&json!({ "text": text }))
        .send()
        .await
        .expect("post intent")
        .json()
        .await
        .expect("intent body")
}

async fn search(client: &reqwest::Client, base: &str, q: &str) -> Value {
    client
        .get(format!("{base}/memory/search"))
        .query(&[("q", q)])
        .send()
        .await
        .expect("get memory/search")
        .json()
        .await
        .expect("memory/search body")
}

fn record_owner_pubkey(record: &Value) -> &str {
    record["owner"]["pubkey"]
        .as_str()
        .expect("owner pubkey must serialize as a base58 string")
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts GET /memory/search reads the caller-owned phase-0 echo over HTTP"]
async fn live_http_memory_search_reads_owned_echo_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    grant(&client, &base, "memory.read").await;
    grant(&client, &base, "memory.write").await;

    let probe = "memory search http probe fixture";

    // memory.read is granted, so the owner-scoped read of an empty store must
    // be an empty memories list, not an error.
    let baseline = search(&client, &base, probe).await;
    assert_eq!(
        baseline["kind"], "memories",
        "the granted baseline read must be the memories variant: {baseline:?}",
    );
    assert_eq!(
        baseline["records"].as_array().map(|r| r.len()),
        Some(0),
        "no memory is stored yet, so the baseline read must be empty: {baseline:?}",
    );

    let intent = submit_intent(&client, &base, probe).await;
    assert_eq!(
        intent["kind"], "intent_result",
        "the no-match intent must dispatch so the echo memory is stored: {intent:?}",
    );

    let stored_echo = format!("phase 0 echo (no agent matched): {probe}");
    let mut found = search(&client, &base, &stored_echo).await;
    for _ in 0..50 {
        if found["records"].as_array().is_some_and(|r| !r.is_empty()) {
            break;
        }
        sleep(Duration::from_millis(50)).await;
        found = search(&client, &base, &stored_echo).await;
    }

    assert_eq!(
        found["kind"], "memories",
        "the raw daemon wire must be `memories`, not the CLI's `memory_read`: {found:?}",
    );
    assert!(
        found.get("min_relevance").is_none(),
        "the HTTP body must not carry the CLI-only `min_relevance` field: {found:?}",
    );
    assert!(
        found.get("limit").is_none(),
        "the HTTP body must not carry a CLI-only top-level `limit` field: {found:?}",
    );
    let records = found["records"].as_array().expect("records array");
    assert!(
        !records.is_empty(),
        "the seeded echo memory must surface on the read: {found:?}",
    );

    let echo = records
        .iter()
        .find(|r| r["text"].as_str() == Some(stored_echo.as_str()))
        .unwrap_or_else(|| panic!("a returned record must carry the seeded echo text: {found:?}"));

    let expected = operator_pubkey_b58(home.path());
    assert_eq!(
        record_owner_pubkey(echo),
        expected.as_str(),
        "the echo memory must be owned by the calling operator: {echo:?}",
    );
    // Owner-scoped read: every returned record must belong to the caller, so a
    // dropped owner filter (cross-tenant leak) is caught even with one writer.
    for record in records {
        assert_eq!(
            record_owner_pubkey(record),
            expected.as_str(),
            "every returned record must be owned by the caller: {record:?}",
        );
    }

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts a memory.read-less GET /memory/search is rejected by name then reads after grant"]
async fn live_http_memory_search_requires_grant_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    let rejected = search(&client, &base, "any query").await;
    assert_eq!(
        rejected["kind"], "error",
        "an ungranted memory search must be rejected: {rejected:?}",
    );
    let message = rejected["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("memory.read"),
        "rejection must name the missing capability: {rejected:?}",
    );

    // Granting memory.read must flip the same query from error to an empty
    // memories list — proving the denial was authorization, not missing state.
    grant(&client, &base, "memory.read").await;
    let allowed = search(&client, &base, "any query").await;
    assert_eq!(
        allowed["kind"], "memories",
        "granting memory.read must flip the search to the memories variant: {allowed:?}",
    );
    assert_eq!(
        allowed["records"].as_array().map(|r| r.len()),
        Some(0),
        "no memory is stored, so the granted read must be empty: {allowed:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
