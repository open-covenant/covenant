//! Live HTTP coverage for `POST /memory/purge` (`memory_purge`).
//!
//! The memory purge mutator is live-tested for delegated IPC denial by
//! `live_memory_read_purge_delegated_denial.rs`, but no test drives the
//! allowance or the selective deletion over the HTTP gateway, leaving the
//! `PurgeBody` deserialization, the bearer-auth operator identity, the
//! `memory.purge` scope gate, and the tier/`before_ms` cutoff unexercised
//! on the HTTP path. This pins that path end-to-end against a real daemon.
//!
//! Two scenarios:
//! - purge round-trip: an operator holding `memory.purge` POSTs a working
//!   tier purge with a `before_ms` falling between two seeded records and
//!   receives `kind: "memory_purged"` with `purged=1`; after the daemon
//!   exits the older record is gone and the newer one survives, proving the
//!   cutoff deletes strictly below it rather than the whole tier.
//! - grant denial: an operator with no grant POSTs the same purge and is
//!   rejected with a body naming `memory.purge`, and both records survive.
//!
//! Hermetic — no external services. `#[ignore]`'d. Each test uses its own
//! tempdir to stay safe under `--test-threads`. Run with
//! `cargo test -p covenantd --test live_http_memory_purge -- --ignored live_`.

use covenant_identity::LocalIdentity;
use covenant_memory::{MemoryStore, SqliteStore};
use covenant_types::{MemoryRecord, MemoryTier};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::time::sleep;
use uuid::Uuid;

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

/// Pre-create the operator identity and two operator-owned working-tier
/// memory records with distinct `created_at` under `home`, returning
/// `(older_id, newer_id)`.
///
/// All writes land before the daemon exists because the daemon takes
/// ownership of the SQLite store on spawn. The identity is created first so
/// the records' owner matches the operator the daemon loads — keeping the
/// rows operator-owned so the grant under test belongs to their owner.
async fn seed_two_working_records(home: &Path) -> (Uuid, Uuid) {
    let identity_path = home.join("identity").join("local.key");
    let identity =
        LocalIdentity::load_or_create(&identity_path, "user@local").expect("create identity");
    let operator = identity.agent_id();

    let older = Uuid::new_v4();
    let newer = Uuid::new_v4();
    let store = SqliteStore::open(&home.join("memory.db")).expect("open memory store");
    for (id, created_at, text) in [
        (older, 10, "older working memory"),
        (newer, 1000, "newer working memory"),
    ] {
        store
            .put(MemoryRecord {
                id,
                tier: MemoryTier::Working,
                owner: operator.clone(),
                text: text.into(),
                embedding: Vec::new(),
                metadata: json!({}),
                created_at,
                parent: None,
            })
            .await
            .expect("put memory record");
    }
    drop(store);

    (older, newer)
}

async fn record_exists(home: &Path, id: Uuid) -> bool {
    let store = SqliteStore::open(&home.join("memory.db")).expect("reopen memory store");
    store.get(id).await.expect("memory get").is_some()
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts POST /memory/purge deletes only records below the cutoff over HTTP"]
async fn live_http_memory_purge_applies_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (older, newer) = seed_two_working_records(home.path()).await;
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    grant(&client, &base, "memory.purge").await;

    // before_ms sits between the two seeded created_at values, so a correct
    // cutoff removes the older row and spares the newer one.
    let posted: Value = client
        .post(format!("{base}/memory/purge"))
        .json(&json!({ "tier": "working", "before_ms": 500 }))
        .send()
        .await
        .expect("post memory purge")
        .json()
        .await
        .expect("purge body");
    assert_eq!(
        posted["kind"], "memory_purged",
        "granted purge must be accepted over HTTP: {posted:?}",
    );
    assert_eq!(
        posted["purged"].as_u64(),
        Some(1),
        "only the one row below the cutoff must be purged: {posted:?}",
    );

    drop(client);
    let _ = child.kill().await;
    let _ = child.wait().await;

    assert!(
        !record_exists(home.path(), older).await,
        "the record below the cutoff must be gone after purge",
    );
    assert!(
        record_exists(home.path(), newer).await,
        "the record at or above the cutoff must survive the purge",
    );
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts a no-grant POST /memory/purge is rejected and leaves the store intact"]
async fn live_http_memory_purge_requires_grant_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (older, newer) = seed_two_working_records(home.path()).await;
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    // No grant. The rejection must name memory.purge — a generic auth
    // failure would give false assurance about scope discipline on the
    // HTTP path.
    let rejected: Value = client
        .post(format!("{base}/memory/purge"))
        .json(&json!({ "tier": "working", "before_ms": 500 }))
        .send()
        .await
        .expect("post memory purge without grant")
        .json()
        .await
        .expect("rejection body");
    assert_eq!(
        rejected["kind"], "error",
        "an ungranted purge must be rejected: {rejected:?}",
    );
    let message = rejected["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("memory.purge"),
        "rejection must name the missing capability: {rejected:?}",
    );
    assert!(
        message.contains("requires capability"),
        "rejection must surface the requires-capability prefix: {rejected:?}",
    );

    drop(client);
    let _ = child.kill().await;
    let _ = child.wait().await;

    assert!(
        record_exists(home.path(), older).await && record_exists(home.path(), newer).await,
        "a denied purge must leave every seeded record in the store",
    );
}
