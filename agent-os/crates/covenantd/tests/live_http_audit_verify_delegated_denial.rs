//! Live HTTP delegated denial coverage for `GET /audit/verify`.
//! `audit_verify` (http.rs:772) dispatches `Request::VerifyAuditIntegrity`
//! with the Bearer-resolved delegate `AgentId`, so the operator-identity
//! gate in `verify_audit_integrity` (lib.rs:3891) applies over HTTP: when
//! `peer.pubkey != self.identity.agent_id().pubkey` it returns
//! `Response::Error "audit integrity verification requires the operator
//! identity"` with NO capability fallback and no report. The IPC path is
//! pinned by `live_ipc_audit_verify_delegated_denial.rs`; this extends the
//! same pin to the HTTP gateway.
//!
//! `live_http_audit_verify.rs` drives the route only as the operator (it
//! records in its own header that "the non-operator rejection path needs a
//! second authenticated identity"), so a non-operator rejection at that gate
//! over HTTP is otherwise unproven: a handler that forwarded the operator
//! identity instead of the bearer-resolved delegate would hand any
//! bearer-authed delegate the whole-chain integrity verdict and root hash
//! over the network. Unlike the post-capability mutation verbs there is NO
//! capability gate in front, so the delegate does not self-grant — it just
//! authenticates and calls. `recent_audit` is feed-scoped to the caller's
//! own issuer; this whole-chain read is not, which is exactly why the
//! verdict and root must not leak.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_audit_verify_delegated_denial -- --ignored live_`.

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
#[ignore = "live: spawns covenantd + verifies a non-operator bearer delegate cannot run audit-integrity verification over HTTP"]
async fn live_http_audit_verify_rejects_non_operator() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([197u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [198u8; 32];
    let delegate_display = "delegate-http-audit-verifier@local";
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
    let delegate = bearer_client(&delegate_token_b58);
    let operator = bearer_client(&operator_token);

    // ── Phase 1: the delegate asks for the global integrity verdict and is
    //     refused by name. The whole-chain verdict and root_hash_hex never
    //     leak to a non-operator — a handler that forwarded the operator
    //     identity instead of the bearer delegate would return the
    //     audit_integrity report, which this asserts against.
    let denied = delegate
        .get(format!("{base}/audit/verify"))
        .send()
        .await
        .expect("send delegate /audit/verify request");
    let denied_json: Value = denied.json().await.expect("delegate /audit/verify body");
    assert_eq!(
        denied_json["kind"], "error",
        "a non-operator delegate must be refused audit verification, never the audit_integrity \
         report; got {denied_json:?}",
    );
    let message = denied_json["message"]
        .as_str()
        .expect("error envelope carries message");
    assert!(
        message.contains("audit integrity verification requires the operator identity"),
        "HTTP /audit/verify must refuse the delegate at the operator-identity gate; got {message:?}",
    );
    assert!(
        denied_json.get("report").is_none(),
        "the refused response must not carry a report payload; got {denied_json:?}",
    );

    // ── Phase 2: the operator clears the same gate and gets a valid report,
    //     so the refusal is identity-scoped, not a broken verb.
    let allowed = operator
        .get(format!("{base}/audit/verify"))
        .send()
        .await
        .expect("send operator /audit/verify request");
    let allowed_json: Value = allowed.json().await.expect("operator /audit/verify body");
    assert_eq!(
        allowed_json["kind"], "audit_integrity",
        "the operator must be admitted with an audit_integrity report; got {allowed_json:?}",
    );
    assert_eq!(
        allowed_json["report"]["valid"], true,
        "a fresh daemon's chain must verify clean for the operator; got {allowed_json:?}",
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
        "delegate must remain authenticated after the audit-verify denial; got {}",
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
