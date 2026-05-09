//! Live integration test: spawns the real `covenantd` binary against a
//! tempdir `COVENANT_HOME`, then exercises the HTTP gateway over a real
//! loopback port.
//!
//! Hermetic: tempdir-isolated home, ephemeral TCP port. `#[ignore]`'d.
//! Run with `cargo test -p covenantd --test live_http_gateway -- --ignored live_`.

use serde_json::{json, Value};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
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
async fn live_http_gateway_health_version_and_bearer_auth() {
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

    let version: Value = reqwest::get(format!("{base}/version"))
        .await
        .expect("version request")
        .json()
        .await
        .expect("version json");
    assert_eq!(version["kind"], "protocol_info");
    assert_eq!(version["info"]["protocol"], "covenant.ipc");
    assert_eq!(version["info"]["version"], 1);
    assert_eq!(version["info"]["min_supported"], 1);
    assert_eq!(version["info"]["max_supported"], 1);

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

#[tokio::test]
#[ignore = "live: spawns covenantd and exercises protected HTTP mutation semantics"]
async fn live_http_gateway_capabilities_purge_scope_round_trip() {
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
    let client = reqwest::Client::new();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("unix time")
        .as_millis() as u64;
    let allowed_before_ms = now_ms + 30_000;
    let denied_before_ms = allowed_before_ms + 30_000;

    let missing_capability: Value = client
        .post(format!("{base}/capabilities/purge"))
        .bearer_auth(&token)
        .json(&json!({ "before_ms": allowed_before_ms }))
        .send()
        .await
        .expect("missing-capability purge request")
        .json()
        .await
        .expect("missing-capability purge json");
    assert_eq!(missing_capability["kind"], "error");
    assert!(missing_capability["message"]
        .as_str()
        .expect("missing-capability message")
        .contains("capabilities.purge"));

    let grant_purge: Value = client
        .post(format!("{base}/capabilities/grant"))
        .bearer_auth(&token)
        .json(&json!({
            "action": "capabilities.purge",
            "scope": { "version": 1, "before_ms": allowed_before_ms }
        }))
        .send()
        .await
        .expect("grant purge request")
        .json()
        .await
        .expect("grant purge json");
    assert_eq!(grant_purge["kind"], "capability_granted");

    let grant_echo: Value = client
        .post(format!("{base}/capabilities/grant"))
        .bearer_auth(&token)
        .json(&json!({ "action": "tool.call.echo" }))
        .send()
        .await
        .expect("grant echo request")
        .json()
        .await
        .expect("grant echo json");
    assert_eq!(grant_echo["kind"], "capability_granted");
    let signature = grant_echo["signature_b58"]
        .as_str()
        .expect("grant signature");

    let revoke: Value = client
        .post(format!("{base}/capabilities/revoke"))
        .bearer_auth(&token)
        .json(&json!({ "signature_b58": signature }))
        .send()
        .await
        .expect("revoke request")
        .json()
        .await
        .expect("revoke json");
    assert_eq!(revoke["kind"], "capability_revoked");
    assert_eq!(revoke["removed"], true);

    let scoped_rejection: Value = client
        .post(format!("{base}/capabilities/purge"))
        .bearer_auth(&token)
        .json(&json!({ "before_ms": denied_before_ms }))
        .send()
        .await
        .expect("scoped purge request")
        .json()
        .await
        .expect("scoped purge json");
    assert_eq!(scoped_rejection["kind"], "error");
    let rejection_message = scoped_rejection["message"]
        .as_str()
        .expect("scope rejection message");
    assert!(rejection_message.contains("capability scope"));
    assert!(rejection_message.contains(&format!("before_ms {denied_before_ms}")));

    let allowed: Value = client
        .post(format!("{base}/capabilities/purge"))
        .bearer_auth(&token)
        .json(&json!({ "before_ms": allowed_before_ms }))
        .send()
        .await
        .expect("allowed purge request")
        .json()
        .await
        .expect("allowed purge json");
    assert_eq!(allowed["kind"], "capabilities_purged");
    assert_eq!(allowed["purged"].as_u64(), Some(1));

    let _ = child.kill().await;
}
