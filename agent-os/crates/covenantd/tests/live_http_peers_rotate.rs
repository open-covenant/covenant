//! Live HTTP coverage for `POST /peers/rotate` (`peers_rotate`).
//!
//! Operator token rotation is live-tested over IPC by `live_rotate_token.rs`
//! and over the CLI by `live_cli_peers_rotate_json.rs`, but no test drives it
//! over the HTTP gateway, leaving the `operator_token_rotated` wire shape, the
//! on-disk `operator.token` rewrite, and the bearer-auth revoke/accept swap in
//! `require_bearer` unexercised on the HTTP path.
//!
//! Two scenarios:
//! - mint + accept: the operator POSTs /peers/rotate with the bootstrap bearer
//!   and receives `kind: "operator_token_rotated"` with a fresh `token_b58`
//!   (the daemon's slug, not the CLI's `peer_token_rotated` remap); the on-disk
//!   operator.token then holds the new b58 at mode 0600, and the new bearer
//!   authenticates a follow-up `GET /peers/list`.
//! - revoke: after rotation the bootstrap bearer is rejected with 401 and a
//!   body naming the dead token state, so a rotated-out credential cannot keep
//!   authenticating over HTTP.
//!
//! Hermetic — no external services. `#[ignore]`'d. Each test uses its own
//! tempdir to stay safe under `--test-threads`. Run with
//! `cargo test -p covenantd --test live_http_peers_rotate -- --ignored live_`.

use reqwest::StatusCode;
use serde_json::Value;
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

/// Spawn covenantd on a free HTTP port against `home`, waiting for both
/// its unix socket and the HTTP gateway to come up. Returns the child and
/// the gateway base URL.
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

/// A client that presents `token` as its bearer on every request. Unlike the
/// sibling tests' `operator_client`, this is parameterized so the same test
/// can speak as both the rotated-out and the freshly minted credential.
fn bearer_client(token: &str) -> reqwest::Client {
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

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts POST /peers/rotate mints a fresh operator bearer that authenticates over HTTP"]
async fn live_http_peers_rotate_mints_and_authenticates_new_bearer_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let bootstrap = read_operator_token(home.path()).await;

    let rotated: Value = bearer_client(&bootstrap)
        .post(format!("{base}/peers/rotate"))
        .send()
        .await
        .expect("post rotate")
        .json()
        .await
        .expect("rotate body");
    assert_eq!(
        rotated["kind"], "operator_token_rotated",
        "HTTP must report the daemon's operator_token_rotated slug, not the CLI's peer_token_rotated remap: {rotated:?}",
    );
    let new_token = rotated["token_b58"]
        .as_str()
        .expect("rotated body must carry token_b58");
    assert!(!new_token.is_empty(), "rotated token must not be empty: {rotated:?}");
    assert_ne!(new_token, bootstrap, "rotation must mint a fresh token");

    // The on-disk file is the daemon's contract with the next process start:
    // it must already hold the new b58, at owner-only mode, by the time the
    // response returns.
    let token_path = home.path().join("peers").join("operator.token");
    let on_disk = std::fs::read_to_string(&token_path).expect("read operator.token");
    assert_eq!(on_disk.trim(), new_token, "disk must hold the rotated token");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&token_path)
            .expect("stat operator.token")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "operator.token must remain mode 0600");
    }

    // The freshly minted bearer must authenticate a protected GET.
    let probe = bearer_client(new_token)
        .get(format!("{base}/peers/list"))
        .send()
        .await
        .expect("get /peers/list with new bearer");
    let status = probe.status();
    let body: Value = probe.json().await.expect("peers/list body");
    assert_eq!(
        status,
        StatusCode::OK,
        "the rotated bearer must authenticate over HTTP: {body:?}",
    );
    assert_eq!(
        body["kind"], "peer_list",
        "the authenticated request must reach the peers/list handler: {body:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts a rotated-out operator bearer is rejected over HTTP"]
async fn live_http_peers_rotate_revokes_old_bearer_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let bootstrap = read_operator_token(home.path()).await;

    let client = bearer_client(&bootstrap);
    let rotated: Value = client
        .post(format!("{base}/peers/rotate"))
        .send()
        .await
        .expect("post rotate")
        .json()
        .await
        .expect("rotate body");
    assert_eq!(
        rotated["kind"], "operator_token_rotated",
        "rotation must succeed before the revocation check: {rotated:?}",
    );

    // The same client still carries the bootstrap bearer, which rotation just
    // revoked. A protected GET with it must be rejected by the gateway, not
    // silently keep working.
    let rejected = client
        .get(format!("{base}/peers/list"))
        .send()
        .await
        .expect("get /peers/list with rotated-out bearer");
    let status = rejected.status();
    let body: Value = rejected.json().await.expect("rejection body");
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the rotated-out bearer must be rejected with 401: {body:?}",
    );
    assert_eq!(
        body["kind"], "error",
        "rejection body must be an error envelope: {body:?}",
    );
    let message = body["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("unknown or revoked"),
        "rejection must name the dead token state: {body:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
