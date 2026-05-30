//! Live integration test: operator token rotation survives daemon restart,
//! exercised over the HTTP gateway.
//!
//! `live_http_peers_rotate.rs` proves the bearer revoke/accept swap within a
//! single daemon process. But revocation is enforced via an in-memory
//! `revoked` map that is made durable only by a `peers/registry.jsonl`
//! tombstone (`PeerEvent::Revoked`); a regression where the rotated-out token
//! is revoked in memory but its tombstone never reaches disk would pass the
//! in-process test yet resurrect the dead credential after the next restart.
//!
//! This drives `POST /peers/rotate` over HTTP, SIGKILLs the daemon, respawns
//! against the same HOME, and verifies that
//!
//! 1. the on-disk `peers/registry.jsonl` carries the `revoked` event before
//!    respawn (persist-then-mutate means the rotate response we already saw
//!    implies the line is on disk);
//! 2. the rotated-out bootstrap bearer is still rejected with 401 against the
//!    fresh daemon (replay-on-`open` rebuilt the `revoked` map);
//! 3. the rotated-in bearer still authenticates (`bootstrap_operator_token`
//!    read the rotated `operator.token` and saw its registry entry in the
//!    replay, rather than re-minting a third token).
//!
//! Uses the kill+respawn shape from `live_restart_peers_revoke.rs` and the
//! bearer-client helpers from `live_http_peers_rotate.rs`. `#[ignore]`'d. Run
//! with `cargo test -p covenantd --test live_http_peers_rotate_restart -- --ignored live_`.

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

/// Spawn covenantd against `home` on `port`, waiting for both the unix socket
/// and the HTTP gateway. Separated from a base-URL helper so the test can
/// respawn on a fresh port against the same HOME.
async fn spawn_http_daemon(home: &Path, port: u16) -> Child {
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
    wait_for_http(&format!("http://127.0.0.1:{port}")).await;
    child
}

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
#[ignore = "live: spawns covenantd twice + verifies a rotated operator token's revoke/accept swap replays across restart over HTTP"]
async fn live_http_peers_rotate_survives_daemon_restart() {
    let home = tempfile::tempdir().expect("tempdir");
    let sock = home.path().join("sock");
    let registry_path = home.path().join("peers").join("registry.jsonl");

    // ── Phase 1: rotate over HTTP, then SIGKILL so the test exercises the
    //     on-disk replay path rather than any clean-shutdown drain.
    let port1 = pick_free_port();
    let base1 = format!("http://127.0.0.1:{port1}");
    let mut child1 = spawn_http_daemon(home.path(), port1).await;
    let bootstrap = read_operator_token(home.path()).await;

    let rotated: Value = bearer_client(&bootstrap)
        .post(format!("{base1}/peers/rotate"))
        .send()
        .await
        .expect("post rotate")
        .json()
        .await
        .expect("rotate body");
    assert_eq!(
        rotated["kind"], "operator_token_rotated",
        "rotation must succeed before the restart: {rotated:?}",
    );
    let new_token = rotated["token_b58"]
        .as_str()
        .expect("rotated body must carry token_b58")
        .to_string();
    assert_ne!(new_token, bootstrap, "rotation must mint a fresh token");

    let _ = child1.kill().await;
    let _ = child1.wait().await;

    // SIGKILL leaves the socket file behind; remove it so `wait_for_sock`
    // after respawn observes the new listener, not the stale node.
    let _ = std::fs::remove_file(&sock);

    // The revocation must be durable on disk before respawn — the registry is
    // persist-then-mutate, so the rotate response we already observed implies
    // the tombstone line is written.
    let registry_text = std::fs::read_to_string(&registry_path).expect("read registry.jsonl");
    assert!(
        registry_text.contains("\"type\":\"revoked\""),
        "registry.jsonl must carry the rotated-out token's revocation before respawn: {registry_text}",
    );

    // ── Phase 2: respawn against the same HOME on a fresh port. Replay-on-open
    //     must rebuild the revoked map and re-register the rotated-in token.
    let port2 = pick_free_port();
    let base2 = format!("http://127.0.0.1:{port2}");
    let mut child2 = spawn_http_daemon(home.path(), port2).await;

    // Replay assertion #1: the rotated-out bootstrap bearer stays rejected.
    let old_probe = bearer_client(&bootstrap)
        .get(format!("{base2}/peers/list"))
        .send()
        .await
        .expect("get /peers/list with rotated-out bearer after restart");
    assert_eq!(
        old_probe.status(),
        StatusCode::UNAUTHORIZED,
        "the rotated-out bearer must stay revoked across the restart",
    );

    // Replay assertion #2: the rotated-in bearer still authenticates, proving
    // bootstrap_operator_token read the persisted operator.token rather than
    // re-minting a third token.
    let new_probe = bearer_client(&new_token)
        .get(format!("{base2}/peers/list"))
        .send()
        .await
        .expect("get /peers/list with rotated-in bearer after restart");
    let new_status = new_probe.status();
    let new_body: Value = new_probe.json().await.expect("peers/list body");
    assert_eq!(
        new_status,
        StatusCode::OK,
        "the rotated-in bearer must authenticate after the restart: {new_body:?}",
    );
    assert_eq!(
        new_body["kind"], "peer_list",
        "the authenticated request must reach the peers/list handler: {new_body:?}",
    );

    let _ = child2.kill().await;
    let _ = child2.wait().await;
}
