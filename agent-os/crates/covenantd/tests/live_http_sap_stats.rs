//! Live HTTP coverage for `GET /sap/stats` (`sap_stats`).
//!
//! The SAP bridge call-stats route is the one protected gateway route with no
//! live coverage on any boundary. Unlike the other handlers it does not forward
//! a `Request` to `Server::respond`; it reads the process-global counter via
//! `read_sap_stats()` and returns a raw JSON projection
//! `{calls, successes, failures, successRate, firstSeenUnix, lastCallUnix,
//! uptimeSeconds}` — not a `Response` envelope. So nothing proves the route is
//! wired, bearer-gated, or returns that shape rather than a 404 or an error
//! envelope.
//!
//! One scenario over the gateway exclusively: spawn covenantd, confirm an
//! unauthenticated `GET /sap/stats` is rejected `401` (it sits behind the
//! protected router's `require_bearer` layer), then read it with the operator
//! bearer and assert the body is the raw seven-key stats object, all counters
//! zeroed on a just-spawned daemon that has made no SAP calls. Each test spawns
//! its own daemon process, so the process-global counter starts at zero; the
//! zeroed baseline plus the absence of a `kind` field is what pins the raw
//! projection (a regression to an envelope, a 404, or a non-zero/garbage shape
//! fails it).
//!
//! Hermetic — no external services. `#[ignore]`'d. Each test uses its own
//! tempdir to stay safe under `--test-threads`. Run with
//! `cargo test -p covenantd --test live_http_sap_stats -- --ignored live_`.

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

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts GET /sap/stats is bearer-gated and returns the zeroed seven-key bridge-stats projection over HTTP"]
async fn live_http_sap_stats_returns_zeroed_projection_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;

    // Bearer gate: the route sits behind the protected router's require_bearer
    // layer, so an unauthenticated read must be 401 — a regression that drops
    // /sap/stats out of the protected router (exposing process stats without a
    // bearer) surfaces here.
    let unauth = reqwest::Client::new()
        .get(format!("{base}/sap/stats"))
        .send()
        .await
        .expect("send unauthenticated sap stats request");
    assert_eq!(
        unauth.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "GET /sap/stats without a bearer must be 401",
    );

    let client = operator_client(home.path()).await;
    let stats: Value = client
        .get(format!("{base}/sap/stats"))
        .send()
        .await
        .expect("get sap stats")
        .json()
        .await
        .expect("sap stats body json");

    // Raw projection, not a Response envelope: the stats handler returns the
    // bare stats object with no `kind` discriminator.
    assert!(
        stats.get("kind").is_none(),
        "/sap/stats must be the raw stats projection, not a kind-tagged envelope: {stats:?}",
    );

    // All seven keys present and zeroed on a just-spawned daemon that has made
    // no SAP calls. Pinning the names and the zero baseline is what rules out an
    // empty object, the wrong shape, or a non-zero/garbage counter passing.
    for key in [
        "calls",
        "successes",
        "failures",
        "firstSeenUnix",
        "lastCallUnix",
        "uptimeSeconds",
    ] {
        assert_eq!(
            stats[key].as_u64(),
            Some(0),
            "{key} must be present and zero on a fresh daemon: {stats:?}",
        );
    }
    assert_eq!(
        stats["successRate"].as_f64(),
        Some(0.0),
        "successRate must be present and 0.0 with no calls: {stats:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
