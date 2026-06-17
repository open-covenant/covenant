//! Live HTTP coverage for the success path of `POST /peers/revoke`
//! (`peers_revoke`).
//!
//! Over HTTP only the delegated-denial arm of `/peers/revoke` is covered
//! (`live_http_peers_lifecycle_delegated_denial.rs` asserts a delegate without
//! the grant is rejected). The happy path — an operator revoking a registered
//! peer by token prefix, the peer being removed, and that peer's bearer then
//! failing authentication — is proven only over the Unix socket
//! (`live_peers_revoke.rs`). So the gateway's success path for the primary
//! peer-deactivation control is unexercised, even though peer revocation is the
//! lever an operator pulls to cut off a compromised agent.
//!
//! `require_bearer` resolves the bearer token through the peer registry before
//! the handler runs, so a valid peer bearer reaches a `200` handler response
//! and a revoked or unknown one is rejected `401`. That makes the bearer's
//! `200 -> 401` flip across the revoke the direct proof that the revoke
//! deactivated the token, not just that the response envelope looked successful.
//!
//! One scenario over the gateway exclusively: pre-seed a guest peer into
//! `peers/registry.jsonl` before boot (the daemon replays it on start), confirm
//! the guest bearer authenticates on a protected route, have the operator revoke
//! it by its full base58 token prefix, assert the `peer_revoked` receipt names
//! the guest, then confirm the guest bearer is now `401` on the same route. The
//! pre-revoke `200` rules out a vacuous post-revoke `401` from a guest that was
//! never authenticated in the first place.
//!
//! Hermetic — no external services. `#[ignore]`'d. Each test uses its own
//! tempdir to stay safe under `--test-threads`. Run with
//! `cargo test -p covenantd --test live_http_peers_revoke -- --ignored live_`.

use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
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

fn client_with_bearer(token: &str) -> reqwest::Client {
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

/// Whether the given bearer is accepted by `require_bearer` — `200` once the
/// token resolves to a registered peer, `401` once it is revoked or unknown.
async fn tools_status(client: &reqwest::Client, base: &str) -> reqwest::StatusCode {
    client
        .get(format!("{base}/tools"))
        .send()
        .await
        .expect("send /tools probe")
        .status()
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts POST /peers/revoke removes a seeded peer and 401s its bearer over HTTP"]
async fn live_http_peers_revoke_removes_peer_and_deauthenticates_bearer_over_http() {
    let home = tempfile::tempdir().expect("tempdir");

    // Pre-seed a guest peer into peers/registry.jsonl before boot; the daemon's
    // own JsonlPeerRegistry replays the file on start, then appends the operator
    // entry alongside.
    let guest_token = PeerToken::generate();
    let guest_token_b58 = guest_token.to_b58();
    let guest_pubkey = [42u8; 32];
    let guest_display = "guest@local";
    let guest_entry = PeerEntry {
        token: guest_token,
        agent_id: AgentId::new(guest_display, guest_pubkey),
        registered_at: 1_700_000_000_000,
    };
    let peers_dir = home.path().join("peers");
    std::fs::create_dir_all(&peers_dir).expect("create peers dir");
    {
        let registry = JsonlPeerRegistry::open(peers_dir.join("registry.jsonl"))
            .await
            .expect("open seed registry");
        registry
            .register(guest_entry)
            .await
            .expect("seed guest peer");
    }

    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let guest = client_with_bearer(&guest_token_b58);
    let operator = client_with_bearer(&read_operator_token(home.path()).await);

    // Phase 1: the seeded guest bearer authenticates pre-revoke. A regression
    // here would make the Phase 3 401 vacuous (a never-authenticated bearer is
    // trivially rejected).
    assert_eq!(
        tools_status(&guest, &base).await,
        reqwest::StatusCode::OK,
        "the seeded guest bearer must authenticate before the revoke",
    );

    // Phase 2: the operator revokes the guest by its full base58 token as the
    // prefix (a unique match). Operator identity authorizes the revoke without a
    // separate grant.
    let revoke: Value = operator
        .post(format!("{base}/peers/revoke"))
        .json(&json!({ "token_prefix": guest_token_b58 }))
        .send()
        .await
        .expect("post peers revoke")
        .json()
        .await
        .expect("revoke body json");
    assert_eq!(
        revoke["kind"], "peer_revoked",
        "operator revoke must succeed: {revoke:?}",
    );
    let outcome = &revoke["outcome"];
    assert_eq!(
        outcome["type"], "revoked",
        "outcome must be the revoked variant, not already-revoked/not-found/ambiguous: {revoke:?}",
    );
    assert_eq!(
        outcome["agent_id"]["display"], guest_display,
        "the revoked peer must be the seeded guest: {revoke:?}",
    );
    let expected_prefix: String = guest_token_b58.chars().take(6).collect();
    assert_eq!(
        outcome["token_prefix"], expected_prefix,
        "summary carries the 6-char prefix, never the full token: {revoke:?}",
    );
    assert!(
        outcome["revoked_at"].as_u64().is_some(),
        "revoked_at must be set on a successful revoke: {revoke:?}",
    );

    // Phase 3: the now-revoked guest bearer is rejected. The 200 -> 401 flip is
    // the proof the revoke deactivated the token, not merely returned a
    // well-formed receipt.
    assert_eq!(
        tools_status(&guest, &base).await,
        reqwest::StatusCode::UNAUTHORIZED,
        "the revoked guest bearer must be 401 after the revoke",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
