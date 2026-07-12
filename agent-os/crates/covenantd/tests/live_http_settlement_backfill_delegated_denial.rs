//! Live HTTP delegated denial coverage for `POST /settlement/receipts/backfill`.
//! `backfill_settlement_receipts` (lib.rs:14441) checks the
//! `settlement.backfill.<mode>` capability FIRST (14456) and only past it
//! the operator-identity gate (14467) `peer.pubkey !=
//! self.identity.agent_id().pubkey`, returning `Response::Error
//! "settlement backfill requires the operator identity"` with no capability
//! fallback. The HTTP handler (`settlement_backfill_receipts`, http.rs:1237)
//! dispatches `Request::BackfillSettlementReceipts` with the bearer-resolved
//! delegate `AgentId`, so the same two-gate ordering applies over the
//! gateway. The IPC path is pinned by
//! `live_ipc_settlement_backfill_delegated_denial.rs`; this extends the same
//! pin to HTTP.
//!
//! The happy-path HTTP tests drive the route only as the operator, which
//! clears the operator-identity gate, so a non-operator rejection at that
//! gate over HTTP is otherwise unproven: a handler that forwarded the
//! operator identity instead of the bearer-resolved delegate would let any
//! bearer-authed delegate rewrite the operator's receipt store over the
//! network. Self-granting first is what makes this an operator-identity pin
//! and not a capability pin: the delegate clears the capability gate, so the
//! refusal must name the identity layer. Unlike compaction there is no
//! non-empty-policy requirement — a default dry_run reaches the success
//! response once the gate is off, so the never-SettlementReceiptsBackfilled
//! guard is not vacuous.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_settlement_backfill_delegated_denial -- --ignored live_`.

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
#[ignore = "live: spawns covenantd + verifies a self-granted bearer delegate cannot backfill settlement receipts over HTTP"]
async fn live_http_settlement_backfill_rejects_self_granted_non_operator() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([173u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [174u8; 32];
    let delegate_display = "delegate-http-settlement-backfiller@local";
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

    // ── Phase 1: the delegate self-grants settlement.backfill.dry_run over
    //     the gateway. The capability subject is the authenticated bearer
    //     peer, so the grant lands and clears the capability gate for the
    //     backfill request. No scope is sent, so the grant stores the empty
    //     `{}` scope that is unbounded and clears the before_ms = u64::MAX
    //     scope probe.
    let grant_response = client
        .post(format!("{base}/capabilities/grant"))
        .json(&json!({ "action": "settlement.backfill.dry_run" }))
        .send()
        .await
        .expect("send /capabilities/grant request");
    let grant_json: Value = grant_response
        .json()
        .await
        .expect("/capabilities/grant response body");
    assert_eq!(
        grant_json["kind"], "capability_granted",
        "delegate self-grant of settlement.backfill.dry_run must succeed; got {grant_json:?}",
    );

    // ── Phase 2: holding the capability, the delegate POSTs a dry-run
    //     backfill with no scope_pubkey. It is past the capability gate and
    //     the empty granted scope clears the scope probe, so the
    //     operator-identity gate is the only barrier left. The delegate is
    //     refused with the identity message, NOT the capability message
    //     (which still contains "settlement.backfill"), and NOT the
    //     settlement_receipts_backfilled kind that the same request returns
    //     once the gate is dropped. That distinction is what catches a
    //     handler forwarding the operator identity instead of the bearer
    //     delegate.
    let backfill_response = client
        .post(format!("{base}/settlement/receipts/backfill"))
        .json(&json!({ "dry_run": true, "scope_pubkey": null }))
        .send()
        .await
        .expect("send /settlement/receipts/backfill request");
    let backfill_json: Value = backfill_response
        .json()
        .await
        .expect("/settlement/receipts/backfill response body");
    assert_eq!(
        backfill_json["kind"], "error",
        "a self-granted delegate must be refused the backfill, never \
         settlement_receipts_backfilled; got {backfill_json:?}",
    );
    let message = backfill_json["message"]
        .as_str()
        .expect("error envelope carries message");
    assert!(
        message.contains("requires the operator identity"),
        "HTTP /settlement/receipts/backfill must refuse the delegate at the operator-identity \
         gate that fires after the capability gate; got {message:?}",
    );
    assert!(
        !message.contains("settlement.backfill"),
        "the delegate cleared the capability gate, so the refusal must name the identity \
         layer, not the capability; got {message:?}",
    );

    // ── Phase 3: the denial did not brick the bearer session — a follow-up
    //     read that passes back through the auth middleware still succeeds.
    //     `/capabilities/recent` is authenticated (unlike `/health`, which
    //     is unprotected) and filters to the caller's own grants, so the
    //     delegate reads back the grant from Phase 1 with a `capabilities`
    //     envelope. This catches a denial that poisons the connection or
    //     returns a 4xx that drops the session: the refusal must be scoped
    //     to the verb, not the whole authenticated session.
    let recent = client
        .get(format!("{base}/capabilities/recent"))
        .send()
        .await
        .expect("send /capabilities/recent request");
    assert!(
        recent.status().is_success(),
        "delegate must remain authenticated after the backfill denial; got {}",
        recent.status(),
    );
    let recent_json: Value = recent
        .json()
        .await
        .expect("/capabilities/recent response body");
    assert_eq!(
        recent_json["kind"], "capabilities",
        "the delegate's bearer session must still read its own grants after the denial; \
         got {recent_json:?}",
    );

    let _ = child.kill().await;
}
