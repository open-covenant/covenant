//! Live HTTP delegated denial coverage for `POST /peers/rotate`.
//! `peers_rotate` (http.rs:958) dispatches `Request::RotateOperatorToken`
//! with the Bearer-resolved delegate `AgentId`, so the operator-identity
//! gate in `rotate_operator_token` (lib.rs:3265) applies over HTTP: when
//! `peer.pubkey != self.identity.agent_id().pubkey` it returns
//! `Response::Error "operator token rotation requires the operator identity"`
//! with NO capability fallback (and records a daemon-issued
//! `OperatorTokenRotationRejected` row). The IPC path is pinned by
//! `live_ipc_rotate_token_delegated_denial.rs`; this extends the same pin to
//! the HTTP gateway.
//!
//! `live_http_peers_rotate.rs` drives the route only as the operator (happy
//! path) and separately covers the revoked-token 401 of a rotated-OUT
//! bearer — an auth-middleware rejection of a DEAD token, not the
//! operator-identity gate of a VALID non-operator. So a non-operator
//! rejection at that gate over HTTP is otherwise unproven: a handler that
//! forwarded the operator identity instead of the bearer-resolved delegate
//! would let any bearer-authed delegate rotate the operator token over the
//! network — seizing root authority and locking the operator out. Unlike the
//! post-capability mutation verbs there is NO capability gate in front, so
//! the delegate does not self-grant — it just authenticates and calls.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_peers_rotate_delegated_denial -- --ignored live_`.

use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
use serde_json::Value;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
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

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies a non-operator bearer delegate cannot rotate the operator token over HTTP"]
async fn live_http_peers_rotate_rejects_non_operator() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([199u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [200u8; 32];
    let delegate_display = "delegate-http-token-rotator@local";
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

    let operator_token = read_operator_token(home.path()).await;
    assert_ne!(
        operator_token, delegate_token_b58,
        "operator and delegate tokens must differ",
    );
    let token_path = home.path().join("peers").join("operator.token");
    let on_disk_before = std::fs::read_to_string(&token_path).expect("read operator.token");
    let delegate = bearer_client(&delegate_token_b58);
    let operator = bearer_client(&operator_token);

    // ── Phase 1: the delegate tries to rotate the operator token and is
    //     refused by name — not handed a new token. A handler that forwarded
    //     the operator identity instead of the bearer delegate would return
    //     operator_token_rotated and seize root authority, which this
    //     asserts against.
    let denied = delegate
        .post(format!("{base}/peers/rotate"))
        .send()
        .await
        .expect("send delegate /peers/rotate request");
    let denied_json: Value = denied.json().await.expect("delegate /peers/rotate body");
    assert_eq!(
        denied_json["kind"], "error",
        "a non-operator delegate must be refused the rotation, never operator_token_rotated; \
         got {denied_json:?}",
    );
    let message = denied_json["message"]
        .as_str()
        .expect("error envelope carries message");
    assert!(
        message.contains("operator token rotation requires the operator identity"),
        "HTTP /peers/rotate must refuse the delegate at the operator-identity gate; got {message:?}",
    );

    // ── Phase 2: no rotation happened. The on-disk operator.token is
    //     byte-unchanged, and the operator's ORIGINAL bearer still
    //     authenticates a protected read — so the refused rotation could not
    //     have rotated the token or revoked the operator.
    let on_disk_after = std::fs::read_to_string(&token_path).expect("read operator.token");
    assert_eq!(
        on_disk_before, on_disk_after,
        "a refused rotation must not rewrite peers/operator.token",
    );
    let operator_read = operator
        .get(format!("{base}/peers/list"))
        .send()
        .await
        .expect("send operator /peers/list request");
    assert!(
        operator_read.status().is_success(),
        "the operator's original bearer must still authenticate after the denied rotation; got {}",
        operator_read.status(),
    );
    let operator_json: Value = operator_read.json().await.expect("operator /peers/list body");
    assert_ne!(
        operator_json["kind"], "error",
        "the operator's original bearer must still be honored (no rotation happened); \
         got {operator_json:?}",
    );

    // ── Phase 3: the denial did not brick the delegate's bearer session — a
    //     follow-up authenticated read still succeeds. `/capabilities/recent`
    //     filters to the caller's own grants (the delegate holds none), so it
    //     returns an empty `capabilities` envelope rather than an auth error.
    let recent = delegate
        .get(format!("{base}/capabilities/recent"))
        .send()
        .await
        .expect("send delegate /capabilities/recent request");
    assert!(
        recent.status().is_success(),
        "delegate must remain authenticated after the rotation denial; got {}",
        recent.status(),
    );
    let recent_json: Value = recent
        .json()
        .await
        .expect("delegate /capabilities/recent body");
    assert_eq!(
        recent_json["kind"], "capabilities",
        "the delegate's bearer session must still serve authenticated reads after the denial; \
         got {recent_json:?}",
    );

    let _ = child.kill().await;
}
