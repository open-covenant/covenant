//! Live HTTP coverage for `GET /peers/list?status=live|revoked`. Seeds a
//! delegate via `JsonlPeerRegistry`, spawns the daemon, authenticates as
//! the operator over the HTTP gateway, revokes the delegate through
//! `POST /peers/revoke`, then lists peers three ways:
//!
//! 1. No `status` query — both rows visible by pubkey identity, with
//!    the delegate row carrying a non-null `revoked_at`. Establishes
//!    that the default behaviour shows both live and revoked rows; a
//!    regression that dropped one side would fail here.
//! 2. `status=live` — operator row visible, delegate row absent.
//! 3. `status=revoked` — delegate row visible with `revoked_at`
//!    non-null, operator row absent.
//!
//! Assertions compare pubkey base58 identities (not counts or
//! displays) so a cross-status leak — e.g., a revoked row sneaking
//! into the live response — fails the test instead of passing
//! vacuously.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_peers_list_status_filter -- --ignored live_`.

use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
}

async fn wait_for_sock(path: &std::path::Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn read_operator_token(home: &std::path::Path) -> String {
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

fn pubkeys_in(value: &Value) -> Vec<String> {
    value["peers"]
        .as_array()
        .expect("peers array")
        .iter()
        .filter_map(|peer| peer.pointer("/agent_id/pubkey").and_then(Value::as_str))
        .map(String::from)
        .collect()
}

fn peer_by_pubkey<'a>(value: &'a Value, pubkey_b58: &str) -> Option<&'a Value> {
    value["peers"].as_array().expect("peers array").iter().find(
        |peer| peer.pointer("/agent_id/pubkey").and_then(Value::as_str) == Some(pubkey_b58),
    )
}

#[tokio::test]
#[ignore = "live: spawns covenantd, revokes a seeded delegate via /peers/revoke, asserts /peers/list?status= narrows by pubkey"]
async fn live_http_peers_list_status_filter_narrows_each_side() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_pubkey = [123u8; 32];
    let delegate_pubkey_b58 = bs58::encode(delegate_pubkey).into_string();
    let delegate_token = PeerToken::from_bytes([124u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_display = "delegate-http-peers-status@local";
    let registry_path = home.path().join("peers").join("registry.jsonl");
    {
        let registry = JsonlPeerRegistry::open(registry_path)
            .await
            .expect("open seed registry");
        registry
            .register(PeerEntry {
                token: delegate_token,
                agent_id: AgentId::new(delegate_display, delegate_pubkey),
                registered_at: 1_700_000_000_000,
            })
            .await
            .expect("seed delegate");
    }

    let port = pick_free_port();
    let base = format!("http://127.0.0.1:{port}");
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(exe)
        .env("COVENANT_HOME", home.path())
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");

    let sock = home.path().join("sock");
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon never created its socket at {}", sock.display());
    }
    wait_for_http(&base).await;

    let token = read_operator_token(home.path()).await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .expect("reqwest client");

    let token_prefix: String = delegate_token_b58.chars().take(8).collect();
    let revoke: Value = client
        .post(format!("{base}/peers/revoke"))
        .json(&json!({ "token_prefix": token_prefix, "force": true }))
        .send()
        .await
        .expect("send /peers/revoke")
        .json()
        .await
        .expect("decode /peers/revoke body");
    assert_eq!(
        revoke["kind"], "peer_revoked",
        "operator-authed revoke of the seeded delegate must succeed: {revoke:?}",
    );

    let unfiltered: Value = client
        .get(format!("{base}/peers/list?limit=20"))
        .send()
        .await
        .expect("send /peers/list (unfiltered)")
        .json()
        .await
        .expect("decode /peers/list (unfiltered) body");
    assert_eq!(unfiltered["kind"], "peer_list");
    let unfiltered_pubkeys = pubkeys_in(&unfiltered);
    assert!(
        unfiltered_pubkeys
            .iter()
            .any(|pk| pk == &delegate_pubkey_b58),
        "unfiltered /peers/list must include the revoked delegate row (default surfaces both sides); got pubkeys={unfiltered_pubkeys:?}",
    );
    let operator_pubkey_b58 = unfiltered_pubkeys
        .iter()
        .find(|pk| pk != &&delegate_pubkey_b58)
        .cloned()
        .expect("unfiltered /peers/list must include the operator row alongside the delegate");
    let unfiltered_delegate = peer_by_pubkey(&unfiltered, &delegate_pubkey_b58)
        .expect("delegate row visible without status filter");
    assert!(
        !unfiltered_delegate["revoked_at"].is_null(),
        "delegate row must carry revoked_at after /peers/revoke; got {unfiltered_delegate:?}",
    );

    let live: Value = client
        .get(format!("{base}/peers/list?limit=20&status=live"))
        .send()
        .await
        .expect("send /peers/list?status=live")
        .json()
        .await
        .expect("decode /peers/list?status=live body");
    assert_eq!(live["kind"], "peer_list");
    let live_pubkeys = pubkeys_in(&live);
    assert!(
        live_pubkeys.iter().any(|pk| pk == &operator_pubkey_b58),
        "status=live must keep the operator (live) row; got pubkeys={live_pubkeys:?}",
    );
    assert!(
        !live_pubkeys.iter().any(|pk| pk == &delegate_pubkey_b58),
        "status=live must drop the revoked delegate row; got pubkeys={live_pubkeys:?}",
    );

    let revoked: Value = client
        .get(format!("{base}/peers/list?limit=20&status=revoked"))
        .send()
        .await
        .expect("send /peers/list?status=revoked")
        .json()
        .await
        .expect("decode /peers/list?status=revoked body");
    assert_eq!(revoked["kind"], "peer_list");
    let revoked_pubkeys = pubkeys_in(&revoked);
    assert!(
        revoked_pubkeys
            .iter()
            .any(|pk| pk == &delegate_pubkey_b58),
        "status=revoked must surface the revoked delegate row; got pubkeys={revoked_pubkeys:?}",
    );
    assert!(
        !revoked_pubkeys.iter().any(|pk| pk == &operator_pubkey_b58),
        "status=revoked must drop the live operator row; got pubkeys={revoked_pubkeys:?}",
    );
    let revoked_delegate = peer_by_pubkey(&revoked, &delegate_pubkey_b58)
        .expect("delegate row visible under status=revoked");
    assert!(
        !revoked_delegate["revoked_at"].is_null(),
        "revoked delegate row must carry a non-null revoked_at; got {revoked_delegate:?}",
    );

    let _ = child.kill().await;
}
