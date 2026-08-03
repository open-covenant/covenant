//! Live HTTP coverage for `POST /capabilities/purge` (round-trip +
//! delegated denial).
//!
//! The purge verb is routed in http.rs (`capabilities_purge` →
//! `Request::PurgeCapabilities`) but had no live HTTP test at all: the
//! socket pin is `live_ipc_purge_capabilities.rs`, the CLI pin is
//! `live_cli_capabilities_purge_json.rs`, and the only capability-family
//! HTTP tests are `live_http_capabilities_recent.rs` and
//! `live_http_capabilities_revoke_readback.rs`. Both the purge round-trip
//! and the non-operator refusal were therefore unexercised through the
//! Bearer middleware, the `PurgeCapabilitiesBody` deserialization, and the
//! HTTP error mapping. This pins that path end-to-end against a real
//! daemon, following the two-scenario shape of `live_http_memory_purge.rs`.
//!
//! Purge eligibility semantics are ported from the IPC pin, not invented:
//! an unscoped `capabilities.purge` self-grant admits any cutoff, a
//! revoked throwaway grant is the one tombstone `revoked_at < before_ms`
//! selects, and the counts bracket the seeded state (0 on the empty
//! baseline, 1 for the tombstone, 0 on the idempotent re-purge). The
//! listing readback leans on the pinned `/capabilities/recent` semantics:
//! revoked signatures are filtered out of the listing, so revocation
//! removes the row and a correct purge deletes only the tombstone while
//! every live row survives.
//!
//! Delegated coverage stays denial-only by matrix policy: capability purge
//! mutates delegated-authority retention state and a delegated allowance
//! requires a human release review
//! (docs/internal/signed-capabilities-live-coverage.md).
//!
//! Hermetic — no external services. `#[ignore]`'d. Each test uses its own
//! tempdir and port to stay safe under `--test-threads`. Run with
//! `cargo test -p covenantd --test live_http_capabilities_purge -- --ignored live_`.

use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::process::{Child, Command};
use tokio::time::sleep;

/// The throwaway action revoked to seed the one purgeable tombstone —
/// the same fixture `live_ipc_purge_capabilities.rs` seeds over the
/// socket.
const THROWAWAY_ACTION: &str = "tool.call.echo";

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_millis() as u64
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

fn bearer_client(token_b58: &str) -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token_b58}").parse().unwrap(),
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .expect("reqwest client")
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

/// Grant `action` as the caller and return the grant's base58 signature —
/// the handle `POST /capabilities/revoke` keys on.
async fn grant(client: &reqwest::Client, base: &str, action: &str) -> String {
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
    granted["signature_b58"]
        .as_str()
        .expect("capability_granted carries signature_b58")
        .to_string()
}

/// POST `/capabilities/purge` with `before_ms` and return the parsed body.
async fn post_purge(client: &reqwest::Client, base: &str, before_ms: u64) -> Value {
    client
        .post(format!("{base}/capabilities/purge"))
        .json(&json!({ "before_ms": before_ms }))
        .send()
        .await
        .expect("send /capabilities/purge request")
        .json()
        .await
        .expect("/capabilities/purge response body")
}

/// Read the caller-visible `/capabilities/recent` listing and return the
/// listed actions.
async fn recent_actions(client: &reqwest::Client, base: &str) -> Vec<String> {
    let listed: Value = client
        .get(format!("{base}/capabilities/recent?limit=50"))
        .send()
        .await
        .expect("get /capabilities/recent")
        .json()
        .await
        .expect("/capabilities/recent response body");
    assert_eq!(
        listed["kind"], "capabilities",
        "recent capabilities must answer with the capabilities envelope: {listed:?}",
    );
    listed["capabilities"]
        .as_array()
        .expect("capabilities array")
        .iter()
        .map(|row| {
            row["capability"]["action"]
                .as_str()
                .expect("listed capability carries an action")
                .to_string()
        })
        .collect()
}

#[tokio::test]
#[ignore = "live: spawns covenantd + drives the POST /capabilities/purge round-trip over HTTP after seeding a revoked grant"]
async fn live_http_capabilities_purge_drops_revoked_tombstone_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let operator = bearer_client(&read_operator_token(home.path()).await);

    // A cutoff comfortably after now: any tombstone revoked during this
    // test falls below it — the same window the IPC pin uses.
    let before_ms = epoch_ms() + 30_000;

    // Grant the purge authority unscoped so any cutoff is allowed.
    let _purge_grant_sig = grant(&operator, &base, "capabilities.purge").await;

    // Empty baseline: no revoked tombstones exist yet.
    let baseline = post_purge(&operator, &base, before_ms).await;
    assert_eq!(
        baseline["kind"], "capabilities_purged",
        "granted purge must be accepted over HTTP: {baseline:?}",
    );
    assert_eq!(
        baseline["purged"].as_u64(),
        Some(0),
        "no revoked tombstones yet must purge zero: {baseline:?}",
    );

    // Seed a throwaway grant, then revoke it to create one tombstone.
    let throwaway_sig = grant(&operator, &base, THROWAWAY_ACTION).await;
    let listed = recent_actions(&operator, &base).await;
    assert!(
        listed.iter().any(|action| action == THROWAWAY_ACTION)
            && listed.iter().any(|action| action == "capabilities.purge"),
        "both live grants must appear in the listing before revocation: {listed:?}",
    );

    let revoked: Value = operator
        .post(format!("{base}/capabilities/revoke"))
        .json(&json!({ "signature_b58": throwaway_sig }))
        .send()
        .await
        .expect("send capability revoke")
        .json()
        .await
        .expect("revoke body json");
    assert_eq!(
        revoked["kind"], "capability_revoked",
        "revoking the throwaway grant must succeed: {revoked:?}",
    );
    assert_eq!(
        revoked["removed"],
        Value::Bool(true),
        "the throwaway grant must be removed by revoke: {revoked:?}",
    );
    let after_revoke = recent_actions(&operator, &base).await;
    assert!(
        !after_revoke.iter().any(|action| action == THROWAWAY_ACTION),
        "the revoked grant must leave the live listing: {after_revoke:?}",
    );
    assert!(
        after_revoke
            .iter()
            .any(|action| action == "capabilities.purge"),
        "the live purge grant must still be listed after the revoke: {after_revoke:?}",
    );

    // The one revoked tombstone, older than the cutoff, is dropped.
    let purged = post_purge(&operator, &base, before_ms).await;
    assert_eq!(
        purged["kind"], "capabilities_purged",
        "the seeded purge must be accepted over HTTP: {purged:?}",
    );
    assert_eq!(
        purged["purged"].as_u64(),
        Some(1),
        "the single revoked tombstone older than the cutoff must be dropped: {purged:?}",
    );

    // The purge deleted only the tombstone: the live grant still lists and
    // the revoked action is still gone.
    let after_purge = recent_actions(&operator, &base).await;
    assert!(
        after_purge
            .iter()
            .any(|action| action == "capabilities.purge"),
        "purging revoked tombstones must not delete live grants: {after_purge:?}",
    );
    assert!(
        !after_purge.iter().any(|action| action == THROWAWAY_ACTION),
        "the purged tombstone must not resurrect in the listing: {after_purge:?}",
    );

    // Idempotent re-purge over the same window drops nothing.
    let repurged = post_purge(&operator, &base, before_ms).await;
    assert_eq!(
        repurged["purged"].as_u64(),
        Some(0),
        "a second purge over the same window must drop nothing: {repurged:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies HTTP delegated denial for /capabilities/purge leaves the tombstone intact"]
async fn live_http_capabilities_purge_rejects_bearer_delegate_without_grant() {
    let home = tempfile::tempdir().expect("tempdir");

    // Enroll the delegate before the daemon spawns, exactly like the
    // audit-purge HTTP sibling: the registry is the daemon's own peer
    // store, so the Bearer token authenticates as a real non-operator.
    let delegate_token = PeerToken::from_bytes([81u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [82u8; 32];
    let registry_path = home.path().join("peers").join("registry.jsonl");
    {
        let registry = JsonlPeerRegistry::open(registry_path)
            .await
            .expect("open seed registry");
        registry
            .register(PeerEntry {
                token: delegate_token,
                agent_id: AgentId::new("delegate-http-capabilities-purge@local", delegate_pubkey),
                registered_at: 1_700_000_000_000,
            })
            .await
            .expect("seed delegate");
    }

    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let operator_token = read_operator_token(home.path()).await;
    assert_ne!(
        operator_token, delegate_token_b58,
        "operator and delegate tokens must differ"
    );
    let operator = bearer_client(&operator_token);
    let delegate = bearer_client(&delegate_token_b58);

    // Seed the same fixture the round-trip proves purgeable: the operator
    // holds `capabilities.purge`, and one revoked throwaway grant sits
    // below the cutoff.
    let before_ms = epoch_ms() + 30_000;
    let _purge_grant_sig = grant(&operator, &base, "capabilities.purge").await;
    let throwaway_sig = grant(&operator, &base, THROWAWAY_ACTION).await;
    let revoked: Value = operator
        .post(format!("{base}/capabilities/revoke"))
        .json(&json!({ "signature_b58": throwaway_sig }))
        .send()
        .await
        .expect("send capability revoke")
        .json()
        .await
        .expect("revoke body json");
    assert_eq!(
        revoked["removed"],
        Value::Bool(true),
        "the throwaway grant must be removed to seed the tombstone: {revoked:?}",
    );

    // The Bearer delegate holds no grant, so the purge — with a cutoff
    // generous enough to drop the tombstone were it allowed — must be
    // refused by the capability gate naming `capabilities.purge`.
    let denied = post_purge(&delegate, &base, before_ms).await;
    assert_eq!(
        denied["kind"], "error",
        "delegate without capabilities.purge must be rejected, never capabilities_purged: {denied:?}",
    );
    let message = denied["message"]
        .as_str()
        .expect("error envelope carries message");
    assert!(
        message.contains("capabilities.purge"),
        "HTTP /capabilities/purge denial must name the governing capability; got {message:?}",
    );
    assert!(
        message.contains("requires capability"),
        "denial must surface the requires-capability refusal, not a generic auth failure; got {message:?}",
    );

    // The refusal is scoped to the privileged verb, not the delegate's
    // authentication — `/health` still succeeds.
    let health = delegate
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("send delegate /health request");
    assert!(
        health.status().is_success(),
        "delegate must remain authenticated after capability denial; got {}",
        health.status(),
    );

    // Nothing was deleted: the operator's purge over the same window still
    // finds the tombstone, so the denied request removed no rows — the
    // deletion-proof reads back at the same boundary the denial crossed.
    let swept = post_purge(&operator, &base, before_ms).await;
    assert_eq!(
        swept["kind"], "capabilities_purged",
        "the operator purge after the denial must be accepted: {swept:?}",
    );
    assert_eq!(
        swept["purged"].as_u64(),
        Some(1),
        "the tombstone must survive the denied delegated purge and fall to the operator sweep: {swept:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
