//! Live HTTP delegated-denial coverage for `POST /audit/purge`.
//!
//! `live_http_audit_purge.rs` already exercises the capability gate over
//! HTTP for the OPERATOR principal (denied before self-granting
//! `audit.purge`, allowed after). What it never proves is the other
//! principal: a Bearer-authenticated non-operator delegate. The socket
//! pin `live_audit_purge_delegated_denial.rs` drives that principal over
//! raw IPC; this test extends the same pin through the HTTP auth
//! middleware, so a gateway regression that resolved the Bearer token to
//! the daemon operator identity before the gate — or dropped the
//! delegate's missing-grant refusal — becomes visible at the process
//! boundary.
//!
//! Delegated coverage stays denial-only by matrix policy: audit purge
//! mutates retention state and a delegated allowance requires a human
//! release review (docs/internal/signed-capabilities-live-coverage.md).
//!
//! Phases:
//! 1. the operator self-grants a marker action so at least one audit row
//!    exists, then captures the `/audit/recent` baseline;
//! 2. the delegate POSTs `/audit/purge` with a purge-everything cutoff
//!    and is refused with an error envelope naming `audit.purge`;
//! 3. the refusal is scope-level, not auth-level: the delegate's
//!    `/health` probe still succeeds;
//! 4. the operator re-reads `/audit/recent` and every baseline row is
//!    still present — the denied request deleted nothing.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_audit_purge_delegated_denial -- --ignored live_`.

use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tokio::time::sleep;

const MARKER_ACTION: &str = "test.audit.rows.marker";

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

async fn audit_events(client: &reqwest::Client, base: &str) -> Vec<Value> {
    let recent: Value = client
        .get(format!("{base}/audit/recent?limit=200"))
        .send()
        .await
        .expect("send /audit/recent request")
        .json()
        .await
        .expect("/audit/recent response body");
    assert_eq!(
        recent["kind"], "audit_events",
        "/audit/recent must answer with the audit_events envelope; got {recent:?}",
    );
    recent["events"]
        .as_array()
        .expect("events array")
        .clone()
}

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies HTTP delegated denial for /audit/purge"]
async fn live_http_audit_purge_rejects_bearer_delegate_without_grant() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([81u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [82u8; 32];
    let delegate_display = "delegate-http-audit-purge@local";
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
        "operator and delegate tokens must differ"
    );
    let operator = bearer_client(&operator_token);
    let delegate = bearer_client(&delegate_token_b58);

    // ── Phase 1: guarantee at least one audit row, then capture the
    //     baseline as the operator. The self-granted marker action is
    //     inert — it resembles no privileged verb — and the grant is
    //     proven to append a `capability_granted` audit row by
    //     `live_http_audit_recent_since_ms.rs`.
    let grant: Value = operator
        .post(format!("{base}/capabilities/grant"))
        .json(&json!({ "action": MARKER_ACTION }))
        .send()
        .await
        .expect("send marker grant request")
        .json()
        .await
        .expect("marker grant response body");
    assert_eq!(
        grant["kind"], "capability_granted",
        "marker self-grant must succeed to seed an audit row; got {grant:?}",
    );

    let baseline = audit_events(&operator, &base).await;
    assert!(
        !baseline.is_empty(),
        "baseline must contain the marker grant row before the denial phase",
    );

    // ── Phase 2: the Bearer delegate holds no grant, so the purge —
    //     with a cutoff generous enough to delete every row were it
    //     allowed — must be refused by the capability gate naming
    //     `audit.purge`, exactly as the IPC pin asserts at the socket.
    let denied: Value = delegate
        .post(format!("{base}/audit/purge"))
        .json(&json!({ "before_ms": epoch_ms().saturating_add(10_000) }))
        .send()
        .await
        .expect("send delegated /audit/purge request")
        .json()
        .await
        .expect("delegated /audit/purge response body");
    assert_eq!(
        denied["kind"], "error",
        "delegate without audit.purge must be rejected, never audit_purged; got {denied:?}",
    );
    let message = denied["message"]
        .as_str()
        .expect("error envelope carries message");
    assert!(
        message.contains("audit.purge"),
        "HTTP /audit/purge denial must name the governing capability; got {message:?}",
    );

    // ── Phase 3: the refusal is scoped to the privileged verb, not the
    //     delegate's authentication — `/health` still succeeds.
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

    // ── Phase 4: nothing was deleted. Every baseline row must still be
    //     served (rows are immutable, so Value equality identifies
    //     them); the denied request may have appended rows, never
    //     removed any.
    let after = audit_events(&operator, &base).await;
    for event in &baseline {
        assert!(
            after.contains(event),
            "audit row vanished after a denied delegated purge: {event:?}",
        );
    }
    let marker_survives = after.iter().any(|event| {
        event["kind"]["type"].as_str() == Some("capability_granted")
            && event["kind"]["action"].as_str() == Some(MARKER_ACTION)
    });
    assert!(
        marker_survives,
        "the marker grant row must survive the denied purge; got {after:?}",
    );

    let _ = child.kill().await;
}
