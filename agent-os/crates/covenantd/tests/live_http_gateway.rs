//! Live integration test: spawns the real `covenantd` binary against a
//! tempdir `COVENANT_HOME`, then exercises the HTTP gateway over a real
//! loopback port.
//!
//! Hermetic: tempdir-isolated home, ephemeral TCP port. `#[ignore]`'d.
//! Run with `cargo test -p covenantd --test live_http_gateway -- --ignored live_`.

use serde_json::Value;
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

async fn read_operator_token(home: &std::path::Path) -> String {
    let path = home.join("peers").join("operator.token");
    for _ in 0..50 {
        if let Ok(s) = std::fs::read_to_string(&path) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
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
            Ok(r) if r.status().is_success() => return,
            _ => sleep(Duration::from_millis(50)).await,
        }
    }
    panic!("http gateway never became healthy at {base}/health");
}

#[tokio::test]
#[ignore = "live: spawns covenantd as a real subprocess"]
async fn live_http_gateway_health_and_bearer_auth() {
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

    let health: Value = reqwest::get(format!("{base}/health"))
        .await
        .expect("health request")
        .json()
        .await
        .expect("health json");
    assert_eq!(health["status"], "ok");

    let denied = reqwest::get(format!("{base}/tools"))
        .await
        .expect("tools request")
        .status();
    assert_eq!(denied, 401);

    let token = read_operator_token(home.path()).await;
    let authed: Value = reqwest::Client::new()
        .get(format!("{base}/tools"))
        .bearer_auth(token)
        .send()
        .await
        .expect("tools authed request")
        .json()
        .await
        .expect("tools authed json");
    assert_eq!(authed["kind"], "tool_list");
    let tools = authed["tools"].as_array().expect("tool array");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(
        names.contains(&"echo"),
        "expected echo in tool list, got {names:?}"
    );
    assert!(
        names.contains(&"clock"),
        "expected clock in tool list, got {names:?}"
    );

    let _ = child.kill().await;
}
