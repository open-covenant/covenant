//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::SapStatus` over the raw IPC socket, asserting the daemon's
//! `Response::SapStatus` snapshot for the disabled-default, enabled, and
//! signer-configured bridge states.
//!
//! The daemon wires the SAP bridge unconditionally at boot from
//! `sap_bridge_config_from_env` (a pure, offline config read — `SapBridge::new`
//! never dials), so a spawned daemon always answers from the configured-bridge
//! branch: the bridge is *disabled* by default but its config still carries
//! non-empty defaults (cluster `devnet`, a default program id and RPC/explorer
//! URL), `enabled` flips with the SAP opt-in env, and `has_signer` reflects
//! `COVENANT_SAP_KEYPAIR` independent of `enabled`. The companion
//! `live_cli_sap_status_json.rs` pins the same surface through the CLI; this
//! pins the socket frame the CLI is built on and adds the enabled path.
//!
//! Hermetic — config is read from env, the bridge never connects, and the
//! keypair var is probed for presence only (never loaded). Each spawn clears
//! the inherited SAP/cluster env so the result is host-independent.
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_sap_status -- --ignored live_`.

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

/// Spawn covenantd against `home`, clearing inherited SAP/cluster env so the
/// bridge config is host-independent, then applying `env` overrides.
async fn spawn_daemon(home: &Path, env: &[(&str, &str)]) -> Child {
    let port = pick_free_port();
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut cmd = Command::new(exe);
    cmd.env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home)
        .env_remove("COVENANT_SOLANA_CLUSTER")
        .env_remove("COVENANT_SAP_ENABLED")
        .env_remove("COVENANT_SAP_DEVNET_ENABLED")
        .env_remove("COVENANT_SAP_KEYPAIR");
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
/// `Request::SapStatus`, returning the decoded response.
async fn sap_status(home: &Path) -> Response {
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
    write_frame(&mut stream, &Request::SapStatus)
        .await
        .expect("write sap status");
    read_frame(&mut stream).await.expect("read sap status")
}

#[tokio::test]
#[ignore = "live: spawns covenantd + queries Request::SapStatus over the socket for the disabled-default bridge"]
async fn live_ipc_sap_status_reports_disabled_default_bridge_config() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path(), &[]).await;

    match sap_status(home.path()).await {
        Response::SapStatus {
            enabled,
            cluster,
            program_id,
            rpc_url,
            explorer_url,
            has_signer,
        } => {
            assert!(
                !enabled,
                "with no SAP opt-in the bridge must report disabled"
            );
            assert_eq!(
                cluster, "devnet",
                "the bridge config must default the cluster to devnet"
            );
            assert_eq!(
                program_id,
                covenant_sap_bridge::DEFAULT_SYNAPSE_PROGRAM_ID,
                "the disabled bridge must still return the resolved program_id, not an empty string"
            );
            assert!(
                !rpc_url.is_empty(),
                "the disabled-default config must still carry an rpc_url"
            );
            assert!(
                !explorer_url.is_empty(),
                "the disabled-default config must still carry an explorer_url"
            );
            assert!(
                !has_signer,
                "without COVENANT_SAP_KEYPAIR the daemon must report no signer"
            );
        }
        other => panic!("expected Response::SapStatus, got {other:?}"),
    }

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd with COVENANT_SAP_ENABLED + asserts Request::SapStatus returns the enabled bridge config"]
async fn live_ipc_sap_status_enabled_returns_bridge_config() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path(), &[("COVENANT_SAP_ENABLED", "true")]).await;

    match sap_status(home.path()).await {
        Response::SapStatus {
            enabled,
            cluster,
            program_id,
            rpc_url,
            explorer_url,
            has_signer,
        } => {
            assert!(
                enabled,
                "COVENANT_SAP_ENABLED=true must flip the bridge enabled"
            );
            assert_eq!(cluster, "devnet", "the default cluster stays devnet");
            assert_eq!(
                program_id,
                covenant_sap_bridge::DEFAULT_SYNAPSE_PROGRAM_ID,
                "an enabled bridge must return the resolved program_id, not an empty default"
            );
            assert!(
                !rpc_url.is_empty(),
                "an enabled bridge must return its rpc_url"
            );
            assert!(
                !explorer_url.is_empty(),
                "an enabled bridge must return its explorer_url"
            );
            assert!(
                !has_signer,
                "enabling the bridge must not fabricate a signer"
            );
        }
        other => panic!("expected Response::SapStatus, got {other:?}"),
    }

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd with COVENANT_SAP_KEYPAIR + asserts Request::SapStatus flips has_signer independent of enabled"]
async fn live_ipc_sap_status_reflects_configured_signer() {
    let home = tempfile::tempdir().expect("tempdir");
    // The handler probes COVENANT_SAP_KEYPAIR for presence only; the value is
    // never loaded, so a placeholder path keeps the test hermetic.
    let mut child = spawn_daemon(
        home.path(),
        &[("COVENANT_SAP_KEYPAIR", "/nonexistent/sap-signer.json")],
    )
    .await;

    match sap_status(home.path()).await {
        Response::SapStatus {
            enabled,
            has_signer,
            ..
        } => {
            assert!(
                has_signer,
                "a non-empty COVENANT_SAP_KEYPAIR must flip has_signer true"
            );
            assert!(
                !enabled,
                "configuring a signer must not flip enabled — the two are independent"
            );
        }
        other => panic!("expected Response::SapStatus, got {other:?}"),
    }

    let _ = child.kill().await;
    let _ = child.wait().await;
}
