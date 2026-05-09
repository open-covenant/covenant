//! Live integration test for `/audit/purge` over the real HTTP gateway.
//!
//! Hermetic: tempdir-isolated home, ephemeral TCP port. `#[ignore]`'d.
//! Run with `cargo test -p covenantd --test live_http_audit_purge -- --ignored live_`.

use serde_json::{json, Value};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().unwrap().port()
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

#[tokio::test]
#[ignore = "live: spawns covenantd and exercises a real HTTP audit purge"]
async fn live_http_audit_purge_round_trip() {
    let home = tempfile::tempdir().expect("tempdir");
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

    let token = read_operator_token(home.path()).await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .expect("reqwest client");

    let before_ms = epoch_ms();
    let denied: Value = client
        .post(format!("{base}/audit/purge"))
        .json(&json!({ "before_ms": before_ms }))
        .send()
        .await
        .expect("denied audit purge request")
        .json()
        .await
        .expect("denied audit purge json");
    assert_eq!(denied["kind"], "error");
    assert!(denied["message"]
        .as_str()
        .expect("error message")
        .contains("audit.purge"));

    let grant: Value = client
        .post(format!("{base}/capabilities/grant"))
        .json(&json!({ "action": "audit.purge" }))
        .send()
        .await
        .expect("grant request")
        .json()
        .await
        .expect("grant json");
    assert_eq!(grant["kind"], "capability_granted");

    let purged: Value = client
        .post(format!("{base}/audit/purge"))
        .json(&json!({ "before_ms": epoch_ms().saturating_add(1_000) }))
        .send()
        .await
        .expect("audit purge request")
        .json()
        .await
        .expect("audit purge json");
    assert_eq!(purged["kind"], "audit_purged");
    assert!(purged["purged"].as_u64().unwrap_or(0) > 0);

    let _ = child.kill().await;
}
