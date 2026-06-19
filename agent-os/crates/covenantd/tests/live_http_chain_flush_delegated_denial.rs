//! Live HTTP delegated denial coverage for `POST /chain/flush-receipts`.
//! The capability matrix marks `chain.flush` as delegated-denial-only and
//! notes the dispatch applies an operator-identity gate AHEAD of the
//! capability gate because flushing mutates batch state. The IPC path is
//! pinned by `live_chain_delegated_denial.rs`, which accepts either the
//! `operator identity` or the `chain.flush` refusal; this extends the same
//! pin to the HTTP gateway.
//!
//! The happy-path `live_http_chain_flush_receipts.rs` drives the route only
//! as the operator, which passes the operator-identity gate, so a
//! non-operator rejection at that gate over HTTP is otherwise unproven: a
//! handler that forwarded the operator identity instead of the bearer-
//! resolved delegate would let any bearer-authed delegate flush receipts —
//! an escalation the operator happy-path cannot catch.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_chain_flush_delegated_denial -- --ignored live_`.

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
#[ignore = "live: spawns covenantd + verifies HTTP delegated denial for /chain/flush-receipts"]
async fn live_http_chain_flush_rejects_delegate_without_operator_authority() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([93u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [94u8; 32];
    let delegate_display = "delegate-http-chain-flusher@local";
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

    // ── Phase 1: POST /chain/flush-receipts as a non-operator delegate is
    //     refused before any merkle-batch state mutates. `flush_receipts`
    //     gates on `peer.pubkey != operator.pubkey` FIRST, so a correctly
    //     wired handler that forwards the bearer-resolved delegate identity
    //     surfaces the operator-identity refusal. Asserting that message
    //     specifically (not the downstream `chain.flush` capability message)
    //     makes this an escalation guard: a handler that forwarded the
    //     operator identity instead of the delegate would clear the identity
    //     gate and surface the capability message — or, if the operator held
    //     the grant, flush for real — and this assertion would catch it.
    let flush_response = client
        .post(format!("{base}/chain/flush-receipts"))
        .json(&json!({ "limit": 10 }))
        .send()
        .await
        .expect("send /chain/flush-receipts request");
    let flush_json: Value = flush_response
        .json()
        .await
        .expect("/chain/flush-receipts response body");
    assert_eq!(
        flush_json["kind"], "error",
        "a non-operator delegate must be refused the flush; got {flush_json:?}",
    );
    let message = flush_json["message"]
        .as_str()
        .expect("error envelope carries message");
    assert!(
        message.contains("operator identity"),
        "HTTP /chain/flush-receipts must refuse the delegate at the operator-identity gate \
         that fires before the capability check; got {message:?}",
    );

    // ── Phase 2: the delegate's session is not bricked by the denial —
    //     `/health` keeps reporting healthy, so the refusal was the
    //     operator/capability gate, not authentication.
    let health = client
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("send /health request");
    assert!(
        health.status().is_success(),
        "delegate must remain authenticated after the flush denial; got {}",
        health.status(),
    );

    let _ = child.kill().await;
}
