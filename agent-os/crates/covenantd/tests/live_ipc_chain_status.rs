//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::ChainStatus` over the raw IPC socket, asserting the daemon's
//! `Response::ChainStatus` snapshot for the unconfigured and fully-configured
//! chain-env states.
//!
//! `chain_status()` resolves `chain_status_from_env()` — a pure read of the
//! Solana env vars with no RPC and no signer — so the test is fully offline.
//! The verb is covered today over the CLI (`live_cli_chain_status_json.rs`) and
//! HTTP (`live_http_chain_status.rs`) but never over the raw Unix socket the CLI
//! is built on. This pins that wire contract: the `missing` list and its order,
//! the `devnet` cluster default, and the `ready = missing.is_empty()` rule.
//!
//! Hermetic — config is read from env, nothing dials out. Each spawn clears the
//! inherited chain env so the result is host-independent. `#[ignore]`'d. Run
//! with `cargo test -p covenantd --test live_ipc_chain_status -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().unwrap().port()
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

/// Spawn covenantd against `home`, clearing inherited chain env so the status
/// is host-independent, then applying `env` overrides.
async fn spawn_daemon(home: &Path, env: &[(&str, &str)]) -> Child {
    let port = pick_free_port();
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
    for (key, value) in env {
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
    child
}

/// Authenticate as the operator over the socket and send one
/// `Request::ChainStatus`, returning the decoded response.
async fn chain_status(home: &Path) -> Response {
    let mut stream = UnixStream::connect(home.join("sock"))
        .await
        .expect("connect socket");
    let token = read_operator_token(home).await;
    write_frame(&mut stream, &Request::Authenticate { token_b58: token })
        .await
        .expect("write authenticate");
    match read_frame(&mut stream).await.expect("read authenticate") {
        Response::Authenticated { .. } => {}
        other => panic!("authenticate failed: {other:?}"),
    }
    write_frame(&mut stream, &Request::ChainStatus)
        .await
        .expect("write chain status");
    read_frame(&mut stream).await.expect("read chain status")
}

#[tokio::test]
#[ignore = "live: spawns covenantd with no chain env + asserts Request::ChainStatus reports the unconfigured/not-ready status"]
async fn live_ipc_chain_status_unconfigured_reports_missing_vars() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path(), &[]).await;

    match chain_status(home.path()).await {
        Response::ChainStatus { status } => {
            assert_eq!(status.chain, "solana", "chain must be solana");
            assert_eq!(status.cluster, "devnet", "cluster must default to devnet");
            assert!(
                !status.ready,
                "an unconfigured daemon must not be chain-ready"
            );
            assert_eq!(status.rpc_url, None);
            assert_eq!(status.ws_url, None);
            assert_eq!(status.program_id, None);
            assert_eq!(status.covnt_mint, None);
            assert_eq!(
                status.missing,
                vec![
                    "COVENANT_SOLANA_RPC_URL",
                    "COVENANT_PROTOCOL_PROGRAM_ID",
                    "COVNT_MINT"
                ],
                "missing must list the three required vars in declaration order"
            );
        }
        other => panic!("expected Response::ChainStatus, got {other:?}"),
    }

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd with a full chain env + asserts Request::ChainStatus reports configured/ready"]
async fn live_ipc_chain_status_configured_reports_ready() {
    let home = tempfile::tempdir().expect("tempdir");
    // Inert placeholders — chain_status_from_env only reads the strings; nothing
    // is dialed or validated, so the test stays hermetic.
    let mut child = spawn_daemon(
        home.path(),
        &[
            ("COVENANT_SOLANA_CLUSTER", "mainnet"),
            ("COVENANT_SOLANA_RPC_URL", "https://rpc.example.invalid"),
            ("COVENANT_SOLANA_WS_URL", "wss://ws.example.invalid"),
            (
                "COVENANT_PROTOCOL_PROGRAM_ID",
                "Cov1111111111111111111111111111111111111111",
            ),
            ("COVNT_MINT", "Mnt1111111111111111111111111111111111111111"),
        ],
    )
    .await;

    match chain_status(home.path()).await {
        Response::ChainStatus { status } => {
            assert_eq!(status.chain, "solana");
            assert_eq!(
                status.cluster, "mainnet",
                "cluster must echo COVENANT_SOLANA_CLUSTER"
            );
            assert!(
                status.ready,
                "all three required vars set must make the daemon chain-ready"
            );
            assert!(
                status.missing.is_empty(),
                "no required vars may be missing: {:?}",
                status.missing
            );
            assert_eq!(
                status.rpc_url.as_deref(),
                Some("https://rpc.example.invalid")
            );
            assert_eq!(status.ws_url.as_deref(), Some("wss://ws.example.invalid"));
            assert_eq!(
                status.program_id.as_deref(),
                Some("Cov1111111111111111111111111111111111111111")
            );
            assert_eq!(
                status.covnt_mint.as_deref(),
                Some("Mnt1111111111111111111111111111111111111111")
            );
        }
        other => panic!("expected Response::ChainStatus, got {other:?}"),
    }

    let _ = child.kill().await;
    let _ = child.wait().await;
}
