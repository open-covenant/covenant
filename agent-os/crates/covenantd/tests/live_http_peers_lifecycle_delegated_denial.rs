//! Live HTTP delegated denial coverage for `/peers/revoke` and
//! `/peers/purge`. Both verbs are operator administration: a non-
//! operator delegate without the named grant must hit the capability
//! gate before any registry mutation runs. The IPC variants are
//! already pinned by `live_peers_list_purge_delegated_denial.rs`;
//! this test extends the same pin to the HTTP gateway mutation surface.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_peers_lifecycle_delegated_denial -- --ignored live_`.

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

fn delegate_client(token_b58: &str) -> reqwest::Client {
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

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies HTTP delegated denial for /peers/revoke and /peers/purge"]
async fn live_http_peers_revoke_and_purge_reject_delegate_without_grant() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([81u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [82u8; 32];
    let delegate_display = "delegate-http-peers-mutator@local";
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

    let client = delegate_client(&delegate_token_b58);

    // ── Phase 1: POST /peers/revoke with an arbitrary token prefix.
    //     The capability gate fires before the registry lookup, so
    //     the request is rejected without revealing whether the
    //     prefix matches any live peer entry.
    let revoke_response = client
        .post(format!("{base}/peers/revoke"))
        .json(&json!({ "token_prefix": "abcdef" }))
        .send()
        .await
        .expect("send /peers/revoke request");
    let revoke_json: Value = revoke_response
        .json()
        .await
        .expect("/peers/revoke response body");
    assert_eq!(
        revoke_json["kind"], "error",
        "delegate without peers.revoke must be rejected; got {revoke_json:?}",
    );
    let revoke_message = revoke_json["message"]
        .as_str()
        .expect("error envelope carries message");
    assert!(
        revoke_message.contains("peers.revoke") || revoke_message.contains("operator identity"),
        "HTTP /peers/revoke denial must name the missing capability or operator-identity gate; got {revoke_message:?}",
    );

    // ── Phase 2: POST /peers/purge with a non-zero before_ms.
    //     Same gate semantics — registry retention is operator-only
    //     and the delegate hits the capability layer first.
    let purge_response = client
        .post(format!("{base}/peers/purge"))
        .json(&json!({ "before_ms": 1u64 }))
        .send()
        .await
        .expect("send /peers/purge request");
    let purge_json: Value = purge_response
        .json()
        .await
        .expect("/peers/purge response body");
    assert_eq!(
        purge_json["kind"], "error",
        "delegate without peers.purge must be rejected; got {purge_json:?}",
    );
    let purge_message = purge_json["message"]
        .as_str()
        .expect("error envelope carries message");
    assert!(
        purge_message.contains("peers.purge") || purge_message.contains("operator identity"),
        "HTTP /peers/purge denial must name the missing capability or operator-identity gate; got {purge_message:?}",
    );

    // ── Phase 3: the delegate's session survives both denials —
    //     `/health` still succeeds so the gate is scope, not auth.
    let health = client
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("send /health request");
    assert!(
        health.status().is_success(),
        "delegate must remain authenticated after capability denial; got {}",
        health.status(),
    );

    let _ = child.kill().await;
}
