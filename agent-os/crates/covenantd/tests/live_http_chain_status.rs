//! Live HTTP coverage for `GET /chain/status` (`chain_status`).
//!
//! The settlement-chain config surface is live-tested over the CLI by
//! `live_cli_chain_status_json`, but no test drives `/chain/status` over the
//! HTTP gateway. That leaves the operator-bearer admission and the
//! `Response::ChainStatus` wire variant — `kind: "chain_status"` with the
//! config nested under `status` (`chain`/`cluster`/`rpc_url`/`ws_url`/
//! `program_id`/`covnt_mint`/`ready`/`missing`) — unexercised on the HTTP path.
//!
//! `chain_status` is a pure read of the daemon's process environment
//! (`chain_status_from_env`): `cluster` defaults to `devnet`, and `ready` is
//! true only when `COVENANT_SOLANA_RPC_URL`, `COVENANT_PROTOCOL_PROGRAM_ID`,
//! and `COVNT_MINT` are all set. Both states are driven over HTTP:
//! - unconfigured: a daemon spawned with those vars cleared reports `ready`
//!   false, `missing` listing exactly the three readiness vars in order, the
//!   default `devnet` cluster, and every optional field null.
//! - configured: a daemon spawned with the full chain env reports `ready`
//!   true, an empty `missing`, the configured `mainnet` cluster, and the
//!   optional fields echoing the configured values.
//!
//! Hermetic — `chain_status_from_env` only reads env vars, so the configured
//! daemon never dials the RPC; the URLs are inert strings. Each spawn clears
//! the inherited chain env first so the result does not depend on the runner's
//! environment. `#[ignore]`'d, own tempdir per test for `--test-threads`
//! safety. Run with
//! `cargo test -p covenantd --test live_http_chain_status -- --ignored live_`.

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

/// Spawn covenantd on a free HTTP port against `home`. The inherited
/// settlement-chain env is always cleared first so the response reflects only
/// `chain_env`, then each override in `chain_env` is applied. Waits for the
/// unix socket and the HTTP gateway. Returns the child and the gateway base URL.
async fn spawn_http_daemon(home: &Path, chain_env: &[(&str, &str)]) -> (Child, String) {
    let port = pick_free_port();
    let base = format!("http://127.0.0.1:{port}");
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut cmd = Command::new(exe);
    cmd.env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home)
        .env_remove("COVENANT_SOLANA_CLUSTER")
        .env_remove("COVENANT_SOLANA_RPC_URL")
        .env_remove("COVENANT_SOLANA_WS_URL")
        .env_remove("COVENANT_PROTOCOL_PROGRAM_ID")
        .env_remove("COVNT_MINT");
    for (key, value) in chain_env {
        cmd.env(key, value);
    }
    let child = cmd
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

async fn chain_status(client: &reqwest::Client, base: &str) -> Value {
    client
        .get(format!("{base}/chain/status"))
        .send()
        .await
        .expect("get chain/status")
        .json()
        .await
        .expect("chain/status body")
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts an unconfigured GET /chain/status reports not-ready over HTTP"]
async fn live_http_chain_status_unconfigured_reports_not_ready_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path(), &[]).await;
    let client = operator_client(home.path()).await;

    let body = chain_status(&client, &base).await;
    assert_eq!(
        body["kind"], "chain_status",
        "the operator read must answer the chain_status variant, not error or a sibling: {body:?}",
    );
    let status = &body["status"];
    assert_eq!(
        status["chain"], "solana",
        "settlement chain must be solana: {body:?}",
    );
    assert_eq!(
        status["cluster"], "devnet",
        "an unconfigured daemon must default cluster to devnet: {body:?}",
    );
    assert_eq!(
        status["ready"].as_bool(),
        Some(false),
        "ready must be false while the readiness vars are unset: {body:?}",
    );
    for field in ["rpc_url", "ws_url", "program_id", "covnt_mint"] {
        assert!(
            status[field].is_null(),
            "unset optional {field} must serialize as null: {body:?}",
        );
    }
    let missing: Vec<&str> = status["missing"]
        .as_array()
        .expect("missing must be an array")
        .iter()
        .map(|v| v.as_str().expect("missing entries are strings"))
        .collect();
    assert_eq!(
        missing,
        [
            "COVENANT_SOLANA_RPC_URL",
            "COVENANT_PROTOCOL_PROGRAM_ID",
            "COVNT_MINT",
        ],
        "missing must list exactly the three readiness vars in order: {body:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd with the chain env set + asserts GET /chain/status reports ready over HTTP"]
async fn live_http_chain_status_configured_reports_ready_over_http() {
    let rpc_url = "https://rpc.example.invalid";
    let ws_url = "wss://rpc.example.invalid";
    let program_id = "Cov1111111111111111111111111111111111111111";
    let covnt_mint = "Mnt1111111111111111111111111111111111111111";

    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(
        home.path(),
        &[
            ("COVENANT_SOLANA_CLUSTER", "mainnet"),
            ("COVENANT_SOLANA_RPC_URL", rpc_url),
            ("COVENANT_SOLANA_WS_URL", ws_url),
            ("COVENANT_PROTOCOL_PROGRAM_ID", program_id),
            ("COVNT_MINT", covnt_mint),
        ],
    )
    .await;
    let client = operator_client(home.path()).await;

    let body = chain_status(&client, &base).await;
    assert_eq!(
        body["kind"], "chain_status",
        "the configured read must still answer the chain_status variant: {body:?}",
    );
    let status = &body["status"];
    assert_eq!(
        status["cluster"], "mainnet",
        "cluster must echo the configured COVENANT_SOLANA_CLUSTER: {body:?}",
    );
    assert_eq!(
        status["ready"].as_bool(),
        Some(true),
        "ready must be true once every readiness var is set: {body:?}",
    );
    assert!(
        status["missing"]
            .as_array()
            .expect("missing must be an array")
            .is_empty(),
        "a fully configured daemon must report an empty missing list: {body:?}",
    );
    assert_eq!(
        status["rpc_url"].as_str(),
        Some(rpc_url),
        "rpc_url must echo the configured COVENANT_SOLANA_RPC_URL: {body:?}",
    );
    assert_eq!(
        status["ws_url"].as_str(),
        Some(ws_url),
        "ws_url must echo the configured COVENANT_SOLANA_WS_URL: {body:?}",
    );
    assert_eq!(
        status["program_id"].as_str(),
        Some(program_id),
        "program_id must echo the configured COVENANT_PROTOCOL_PROGRAM_ID: {body:?}",
    );
    assert_eq!(
        status["covnt_mint"].as_str(),
        Some(covnt_mint),
        "covnt_mint must echo the configured COVNT_MINT: {body:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
