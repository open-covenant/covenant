//! Live CLI coverage for `covenant sap status --json`.
//!
//! `sap status` sends `Request::SapStatus` and renders the daemon's
//! `Response::SapStatus` as `{ kind: "sap_status", enabled, cluster,
//! program_id, rpc_url, explorer_url, has_signer }`. No live test drove the
//! CLI→daemon round-trip.
//!
//! The daemon wires the SAP bridge unconditionally at boot from
//! `sap_bridge_config_from_env` (a pure, offline config read — `SapBridge::new`
//! never dials), so a spawned daemon always answers from the configured-bridge
//! branch: with no SAP opt-in the bridge is *disabled* but its config still
//! carries defaults (cluster `devnet`, a default program id, a default RPC
//! endpoint), and `has_signer` reflects whether `COVENANT_SAP_KEYPAIR` is set —
//! independent of `enabled`.
//!
//! Two scenarios:
//! - default/offline: no SAP env → `enabled` false, `cluster` `devnet`,
//!   `program_id`/`rpc_url`/`explorer_url` non-empty defaults, `has_signer`
//!   false.
//! - signer configured: `COVENANT_SAP_KEYPAIR` set → `has_signer` true while
//!   `enabled` stays false, proving the signer probe reads the daemon env and
//!   is independent of the enable opt-in.
//!
//! Hermetic — config is read from env, the bridge never connects, and the
//! keypair var is probed for presence only (never loaded). Each spawn clears
//! the inherited SAP/cluster env so the result is host-independent. Build the
//! CLI first (`cargo build -p covenant`); run with
//! `cargo test -p covenantd --test live_cli_sap_status_json -- --ignored live_`.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().unwrap().port()
}

fn covenant_cli_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/covenant")
        .canonicalize()
        .expect("covenant CLI binary not built; run `cargo build -p covenant` first")
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

async fn wait_for_operator_token(home: &Path) {
    let path = home.join("peers").join("operator.token");
    for _ in 0..50 {
        if let Ok(s) = std::fs::read_to_string(&path) {
            if !s.trim().is_empty() {
                return;
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
    let daemon_exe = env!("CARGO_BIN_EXE_covenantd");
    let mut cmd = Command::new(daemon_exe);
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
    wait_for_operator_token(home).await;
    child
}

async fn sap_status_json(cli: &Path, home: &Path) -> Value {
    let output = Command::new(cli)
        .args(["sap", "status", "--json"])
        .env("COVENANT_HOME", home)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "sap status --json failed: status={:?} stdout={stdout:?} stderr={stderr:?}",
        output.status,
    );
    assert!(
        stderr.trim().is_empty(),
        "sap status --json must not emit stderr on success: {stderr:?}",
    );
    serde_json::from_str(stdout.trim()).expect("sap status --json must be valid JSON")
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts `covenant sap status --json` reports the disabled-default bridge"]
async fn live_cli_sap_status_json_reports_disabled_defaults() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path(), &[]).await;
    let cli = covenant_cli_bin();

    let status = sap_status_json(&cli, home.path()).await;
    assert_eq!(
        status["kind"], "sap_status",
        "the verb must answer the sap_status envelope: {status:?}",
    );
    assert_eq!(
        status["enabled"].as_bool(),
        Some(false),
        "with no SAP opt-in the bridge must report disabled: {status:?}",
    );
    assert_eq!(
        status["cluster"], "devnet",
        "the bridge config must default the cluster to devnet: {status:?}",
    );
    for field in ["program_id", "rpc_url", "explorer_url"] {
        assert!(
            status[field].as_str().is_some_and(|v| !v.is_empty()),
            "the disabled-default config must still carry a {field}: {status:?}",
        );
    }
    assert_eq!(
        status["has_signer"].as_bool(),
        Some(false),
        "without COVENANT_SAP_KEYPAIR the daemon must report no signer: {status:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd with COVENANT_SAP_KEYPAIR + asserts `covenant sap status --json` flips has_signer"]
async fn live_cli_sap_status_json_reflects_configured_signer() {
    let home = tempfile::tempdir().expect("tempdir");
    // The handler probes COVENANT_SAP_KEYPAIR for presence only; the value is
    // never loaded, so a placeholder path keeps the test hermetic.
    let mut child = spawn_daemon(
        home.path(),
        &[("COVENANT_SAP_KEYPAIR", "/nonexistent/sap-signer.json")],
    )
    .await;
    let cli = covenant_cli_bin();

    let status = sap_status_json(&cli, home.path()).await;
    assert_eq!(
        status["kind"], "sap_status",
        "the verb must answer the sap_status envelope: {status:?}",
    );
    assert_eq!(
        status["has_signer"].as_bool(),
        Some(true),
        "a non-empty COVENANT_SAP_KEYPAIR must flip has_signer true: {status:?}",
    );
    assert_eq!(
        status["enabled"].as_bool(),
        Some(false),
        "configuring a signer must not flip enabled — the two are independent: {status:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
