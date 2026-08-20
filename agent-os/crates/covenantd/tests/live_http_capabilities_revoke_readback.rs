//! Live HTTP coverage for `POST /capabilities/revoke` read-back.
//!
//! `revoke_capability` (http.rs:664 -> covenantd `revoke_capability`
//! lib.rs:14996) is exercised over the gateway today only at the envelope
//! level: `live_http_gateway.rs:217-228` asserts `kind:
//! "capability_revoked"` and `removed: true`, but never re-reads the ledger,
//! so a daemon that acknowledges the revoke yet leaves the grant active would
//! pass. The read-back is proven over IPC
//! (`live_ipc_revoke_capability.rs`: a `RecentCapabilities` request after the
//! revoke must not list the grant); this mirrors it over HTTP.
//!
//! Driving the gateway exclusively: grant two distinct capabilities and
//! confirm `GET /capabilities/recent` lists both, revoke the first, then
//! confirm the revoked signature is absent from a fresh listing while the
//! second grant remains — the control that pins the removal as targeted, not
//! a blanket ledger wipe. The pre-revoke read makes the post-revoke absence a
//! real state transition rather than a surface that never listed the grant.
//!
//! Revoke is operator-authenticated, not capability-gated, so only the bearer
//! token is required. Hermetic — no external services. `#[ignore]`'d. Run
//! with `cargo test -p covenantd --test live_http_capabilities_revoke_readback
//! -- --ignored live_`.

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

async fn operator_client(home: &Path) -> reqwest::Client {
    let token = read_operator_token(home).await;
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

async fn grant_signature(client: &reqwest::Client, base: &str, action: &str) -> String {
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
        .unwrap_or_else(|| panic!("grant {action} carries signature_b58: {granted:?}"))
        .to_string()
}

/// The base58 signatures of every row in a fresh `GET /capabilities/recent`.
async fn recent_signatures(client: &reqwest::Client, base: &str) -> Vec<String> {
    let listed: Value = client
        .get(format!("{base}/capabilities/recent?limit=10"))
        .send()
        .await
        .expect("get capabilities recent")
        .json()
        .await
        .expect("recent capabilities body");
    assert_eq!(
        listed["kind"], "capabilities",
        "recent capabilities must be a capabilities envelope: {listed:?}",
    );
    listed["capabilities"]
        .as_array()
        .expect("capabilities array")
        .iter()
        .map(|row| {
            row["signature"]
                .as_str()
                .unwrap_or_else(|| panic!("each row carries a base58 signature: {row:?}"))
                .to_string()
        })
        .collect()
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts a capability revoked over POST /capabilities/revoke disappears from GET /capabilities/recent while an unrelated grant remains"]
async fn live_http_revoked_capability_is_absent_from_recent_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    // Two distinct self-grants. The first will be revoked; the second is the
    // control that must survive, pinning the revoke as targeted.
    let revoked_sig = grant_signature(&client, &base, "memory.read").await;
    let kept_sig = grant_signature(&client, &base, "audit.read").await;
    assert_ne!(
        revoked_sig, kept_sig,
        "the two grants must carry distinct signatures",
    );

    // Pre-revoke, both grants are listed — so the post-revoke absence below is a
    // real state transition, not a surface that never listed the grant.
    let before = recent_signatures(&client, &base).await;
    assert!(
        before.contains(&revoked_sig),
        "the grant to be revoked must be listed before the revoke: {before:?}",
    );
    assert!(
        before.contains(&kept_sig),
        "the control grant must be listed before the revoke: {before:?}",
    );

    let revoke: Value = client
        .post(format!("{base}/capabilities/revoke"))
        .json(&json!({ "signature_b58": revoked_sig }))
        .send()
        .await
        .expect("revoke request")
        .json()
        .await
        .expect("revoke body json");
    assert_eq!(
        revoke["kind"], "capability_revoked",
        "revoke must acknowledge with a capability_revoked envelope: {revoke:?}",
    );
    assert_eq!(
        revoke["removed"], true,
        "revoke of a live grant must report removed: true: {revoke:?}",
    );

    // The revoked signature must be gone from a fresh listing; the control must
    // remain. A daemon that acks the revoke but leaves the grant active fails
    // the first assert; one that wiped the whole ledger fails the second.
    let after = recent_signatures(&client, &base).await;
    assert!(
        !after.contains(&revoked_sig),
        "the revoked capability must not appear in /capabilities/recent after revoke: {after:?}",
    );
    assert!(
        after.contains(&kept_sig),
        "the unrelated grant must survive a targeted revoke: {after:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
