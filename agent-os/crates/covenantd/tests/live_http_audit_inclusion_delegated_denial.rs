//! Live HTTP delegated denial coverage for `GET /audit/inclusion/:event_id`.
//! `audit_inclusion` (http.rs:808) parses the path id as a `Uuid` and
//! dispatches `Request::ProveAuditInclusion` with the Bearer-resolved
//! delegate `AgentId`, so the operator-identity gate in
//! `prove_audit_inclusion` applies over HTTP: when `peer.pubkey !=
//! self.identity.agent_id().pubkey` it returns `Response::Error "audit
//! inclusion proof requires the operator identity"` with NO capability
//! fallback, before the audit store is ever consulted. The gate matters
//! because the proof discloses the event's exact serialized line
//! (`leaf_line`) plus the chain hashes around it — the operator's own
//! audit trail — and `/audit/recent` is feed-scoped precisely so a
//! delegate cannot read other issuers' rows; a handler that forwarded
//! the operator identity instead of the bearer delegate would hand any
//! enrolled peer that content for any event id over the network.
//!
//! The denial is pinned against a REAL seeded event id, not a random
//! uuid: a route that ran the not-found branch before the identity gate
//! would refuse a random id for the wrong reason, while refusing an id
//! that demonstrably resolves (the operator arm proves it) isolates the
//! identity gate. `/audit/verify`'s twin gate is pinned by
//! `live_http_audit_verify_delegated_denial.rs`; this extends the same
//! pin to the inclusion-proof route, which no test drove from a
//! delegate on any boundary.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_audit_inclusion_delegated_denial -- --ignored live_`.

use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;

const MARKER_ACTION: &str = "test.audit.inclusion.marker";
const DENIAL: &str = "audit inclusion proof requires the operator identity";

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
#[ignore = "live: spawns covenantd + verifies a non-operator bearer delegate cannot fetch audit inclusion proofs over HTTP"]
async fn live_http_audit_inclusion_rejects_non_operator_for_real_event() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([199u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [200u8; 32];
    let delegate_display = "delegate-http-audit-inclusion@local";
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
    let operator = bearer_client(&operator_token);
    let delegate = bearer_client(&delegate_token_b58);

    // ── Phase 1: seed one audit row the test controls. The inert marker
    //     self-grant appends a `capability_granted` event (the pattern
    //     `live_http_audit_purge_delegated_denial.rs` relies on), whose id
    //     becomes the proven-real target for both arms below.
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

    let recent: Value = operator
        .get(format!("{base}/audit/recent?limit=200"))
        .send()
        .await
        .expect("send operator /audit/recent request")
        .json()
        .await
        .expect("/audit/recent response body");
    assert_eq!(
        recent["kind"], "audit_events",
        "/audit/recent must answer with the audit_events envelope; got {recent:?}",
    );
    let event_id = recent["events"]
        .as_array()
        .expect("events array")
        .iter()
        .find(|event| event["kind"]["action"].as_str() == Some(MARKER_ACTION))
        .and_then(|event| event["id"].as_str())
        .expect("the marker grant row must be visible in the operator's feed")
        .to_string();

    // ── Phase 2: the delegate asks for the inclusion proof of that real
    //     event and is refused by the identity gate by exact name — never
    //     the not-found branch, never proof material. `leaf_line` would
    //     disclose the serialized event; the envelope must carry none.
    let denied: Value = delegate
        .get(format!("{base}/audit/inclusion/{event_id}"))
        .send()
        .await
        .expect("send delegate /audit/inclusion request")
        .json()
        .await
        .expect("delegate /audit/inclusion body");
    assert_eq!(
        denied["kind"], "error",
        "a non-operator delegate must be refused an inclusion proof; got {denied:?}",
    );
    assert_eq!(
        denied["message"], DENIAL,
        "the refusal must be the verb-specific operator-identity message; got {denied:?}",
    );
    assert!(
        denied.get("proof").is_none(),
        "the refused response must not carry proof material; got {denied:?}",
    );

    // ── Phase 3: the operator clears the same gate on the SAME id and
    //     gets a resolvable proof, so Phase 2's refusal was identity-
    //     scoped — not a broken verb, not a missing event.
    let allowed: Value = operator
        .get(format!("{base}/audit/inclusion/{event_id}"))
        .send()
        .await
        .expect("send operator /audit/inclusion request")
        .json()
        .await
        .expect("operator /audit/inclusion body");
    assert_eq!(
        allowed["kind"], "audit_inclusion",
        "the operator must be admitted with an audit_inclusion envelope; got {allowed:?}",
    );
    assert_eq!(
        allowed["proof"]["event_id"], event_id,
        "the proof must bind to the requested event id; got {allowed:?}",
    );
    assert!(
        allowed["proof"]["leaf_line"]
            .as_str()
            .is_some_and(|line| !line.is_empty()),
        "the proof must disclose the serialized leaf line to the operator; got {allowed:?}",
    );

    // ── Phase 4: the denial did not brick the delegate's bearer session —
    //     a follow-up authenticated read still succeeds. `/capabilities/
    //     recent` filters to the caller's own grants (the delegate holds
    //     none), so it returns an empty `capabilities` envelope rather
    //     than an auth error.
    let recent = delegate
        .get(format!("{base}/capabilities/recent"))
        .send()
        .await
        .expect("send delegate /capabilities/recent request");
    assert!(
        recent.status().is_success(),
        "delegate must remain authenticated after the inclusion denial; got {}",
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
