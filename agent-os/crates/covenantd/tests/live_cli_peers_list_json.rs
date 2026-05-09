//! Live CLI coverage for `covenant peers list --json`.
//!
//! Spawns a real daemon against a temp home, runs the CLI subprocess,
//! and asserts that automation can parse the peer-list response without
//! scraping human text. Opt-in because it crosses process and socket
//! boundaries. Run from `agent-os/` after `cargo build -p covenant`.

use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
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

async fn wait_for_sock(path: &std::path::Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn wait_for_operator_token(home: &std::path::Path) {
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

async fn run_peers_list_json(
    cli_exe: &std::path::Path,
    home: &std::path::Path,
    args: &[&str],
) -> Value {
    let output = Command::new(cli_exe)
        .arg("peers")
        .arg("list")
        .args(args)
        .arg("--json")
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
        "peers list --json failed: status={:?} stdout={stdout:?} stderr={stderr:?}",
        output.status
    );
    assert!(
        stderr.trim().is_empty(),
        "peers list --json must not mix diagnostics into stderr: {stderr:?}"
    );
    serde_json::from_str(stdout.trim()).expect("peers list --json must emit one valid JSON object")
}

#[tokio::test]
#[ignore = "live: spawns covenantd + runs `covenant peers list --json` subprocess"]
async fn live_cli_peers_list_json_round_trip() {
    let home = tempfile::tempdir().expect("tempdir");
    let guest_pubkey = [42u8; 32];
    let guest_full_b58 = bs58::encode(guest_pubkey).into_string();
    let guest_display = "guest-json@local";

    let registry_path = home.path().join("peers").join("registry.jsonl");
    {
        let registry = JsonlPeerRegistry::open(registry_path)
            .await
            .expect("open seed registry");
        registry
            .register(PeerEntry {
                token: PeerToken::generate(),
                agent_id: AgentId::new(guest_display, guest_pubkey),
                registered_at: 1_700_000_000_000,
            })
            .await
            .expect("seed guest peer");
    }

    let port = pick_free_port();
    let daemon_exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(daemon_exe)
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
    wait_for_operator_token(home.path()).await;

    let cli_exe = covenant_cli_bin();

    let full = run_peers_list_json(&cli_exe, home.path(), &[]).await;
    assert_eq!(full["kind"].as_str(), Some("peer_list"));
    assert_eq!(full["truncated"].as_bool(), Some(false));

    let operator_pubkey = full["operator_pubkey_b58"]
        .as_str()
        .expect("operator_pubkey_b58 must be a string");
    assert!(
        !operator_pubkey.is_empty(),
        "operator_pubkey_b58 must not be empty"
    );

    let peers = full["peers"]
        .as_array()
        .expect("peers list JSON must include peers array");
    assert_eq!(
        peers.len(),
        2,
        "expected operator and guest rows in full JSON response: {peers:?}"
    );

    let operator = peers
        .iter()
        .find(|peer| peer["agent_id"]["pubkey"].as_str() == Some(operator_pubkey))
        .expect("JSON response must include operator row");
    assert_eq!(operator["agent_id"]["display"].as_str(), Some("user@local"));
    assert!(operator["revoked_at"].is_null());
    assert_eq!(operator["token_prefix"].as_str().map(str::len), Some(6));

    let guest = peers
        .iter()
        .find(|peer| peer["agent_id"]["display"].as_str() == Some(guest_display))
        .expect("JSON response must include guest row");
    assert_eq!(
        guest["agent_id"]["pubkey"].as_str(),
        Some(guest_full_b58.as_str())
    );
    assert!(guest["revoked_at"].is_null());
    assert_eq!(guest["token_prefix"].as_str().map(str::len), Some(6));

    let limited = run_peers_list_json(&cli_exe, home.path(), &["--limit", "1"]).await;
    assert_eq!(limited["kind"].as_str(), Some("peer_list"));
    assert_eq!(limited["truncated"].as_bool(), Some(true));

    let limited_peers = limited["peers"]
        .as_array()
        .expect("limited JSON response must include peers array");
    assert_eq!(
        limited_peers.len(),
        1,
        "limit 1 must return exactly one peer row: {limited_peers:?}"
    );
    assert_eq!(
        limited_peers[0]["agent_id"]["pubkey"].as_str(),
        Some(
            limited["operator_pubkey_b58"]
                .as_str()
                .expect("limited response must include operator pubkey")
        )
    );

    let _ = child.kill().await;
}
