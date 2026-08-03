//! Live HTTP coverage for both branches of the peers-list capability
//! fallback. `GET /peers/list` admits "the operator identity or
//! capability \"peers.list\"" (covenantd `list_peers`): the socket
//! denial is pinned by `live_peers_list_purge_delegated_denial.rs` and
//! operator-side filtering by `live_http_peers_list_status_filter.rs`,
//! but neither fallback branch is exercised for an HTTP delegate. The
//! allowance branch is the decisive one — a route that collapsed to a
//! pure operator-identity check would pass every existing denial test
//! while refusing legitimately granted delegates, so serving the
//! roster to a delegate holding exactly `peers.list` is the only proof
//! that the capability fallback is threaded through the Bearer
//! middleware.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_peers_list_fallback_gate -- --ignored live_`.

use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::time::sleep;

const DENIAL: &str = "peers list requires the operator identity or capability \"peers.list\"";

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

/// Enrolls a delegate over `POST /peers/enroll` and returns its
/// `(token_b58, pubkey_b58)`. Asserts the granted list echoes `actions`
/// exactly, so the allowance arm below runs under precisely `peers.list`
/// and never a broader grant that would mask the fallback branch.
async fn enroll(
    operator: &reqwest::Client,
    base: &str,
    display: &str,
    actions: &[&str],
) -> (String, String) {
    let enrolled: Value = operator
        .post(format!("{base}/peers/enroll"))
        .json(&json!({ "display": display, "actions": actions }))
        .send()
        .await
        .expect("send /peers/enroll")
        .json()
        .await
        .expect("enroll body json");
    assert_eq!(
        enrolled["kind"], "peer_enrolled",
        "enrolling {display} must succeed: {enrolled:?}",
    );
    assert_eq!(
        enrolled["granted"],
        json!(actions),
        "enrollment must grant exactly the requested actions: {enrolled:?}",
    );
    let token = enrolled["token_b58"]
        .as_str()
        .expect("enrollment carries token_b58")
        .to_string();
    let pubkey = enrolled["pubkey_b58"]
        .as_str()
        .expect("enrollment carries pubkey_b58")
        .to_string();
    (token, pubkey)
}

async fn list_peers(client: &reqwest::Client, base: &str) -> Value {
    client
        .get(format!("{base}/peers/list"))
        .send()
        .await
        .expect("send /peers/list")
        .json()
        .await
        .expect("/peers/list body json")
}

#[tokio::test]
#[ignore = "live: spawns covenantd + pins both branches of the peers-list capability fallback over HTTP"]
async fn live_http_peers_list_fallback_denies_ungranted_and_serves_granted_delegate() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let operator = bearer_client(&read_operator_token(home.path()).await);

    // ── Branch (a): a delegate with no grant at all. Enrolled with an
    //     empty action list so the denial can only come from the
    //     capability fallback, never from a broken bearer session.
    let (blind_token, blind_pubkey) = enroll(&operator, &base, "roster-blind@local", &[]).await;
    let blind = bearer_client(&blind_token);

    let denied = list_peers(&blind, &base).await;
    assert_eq!(
        denied["kind"], "error",
        "delegate without peers.list must be refused: {denied:?}",
    );
    assert_eq!(
        denied["message"], DENIAL,
        "the refusal must be the verb-specific fallback message: {denied:?}",
    );

    // ── Branch (b): a delegate holding exactly `peers.list`. If the
    //     route had collapsed to a pure operator-identity check this
    //     request would be refused despite the grant.
    let (reader_token, reader_pubkey) =
        enroll(&operator, &base, "roster-reader@local", &["peers.list"]).await;
    let reader = bearer_client(&reader_token);

    let served = list_peers(&reader, &base).await;
    assert_eq!(
        served["kind"], "peer_list",
        "delegate holding exactly peers.list must be served the roster: {served:?}",
    );
    // Membership only — ordering and incidental fields are free to drift.
    let roster: Vec<&str> = served["peers"]
        .as_array()
        .expect("peer_list carries a peers array")
        .iter()
        .map(|row| {
            row["agent_id"]["pubkey"]
                .as_str()
                .expect("each roster row carries a base58 pubkey")
        })
        .collect();
    assert!(
        roster.contains(&reader_pubkey.as_str()),
        "the served roster must contain the granted delegate itself: {roster:?}",
    );
    assert!(
        roster.contains(&blind_pubkey.as_str()),
        "the served roster must be the full registry view, including the ungranted delegate: {roster:?}",
    );

    // ── The grant is per-subject, not a global unlock: the ungranted
    //     delegate is still refused after the reader was served. The
    //     re-denial itself crosses the Bearer middleware, so it also
    //     proves the blind delegate's session survived the first
    //     refusal; `/health` (intentionally unauthenticated) then pins
    //     that the denials did not wedge the gateway.
    let still_denied = list_peers(&blind, &base).await;
    assert_eq!(
        still_denied["kind"], "error",
        "the ungranted delegate must remain refused after another delegate was served: {still_denied:?}",
    );
    assert_eq!(still_denied["message"], DENIAL);
    let health = blind
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("send /health");
    assert!(
        health.status().is_success(),
        "daemon must stay healthy after the capability denials; got {}",
        health.status(),
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
