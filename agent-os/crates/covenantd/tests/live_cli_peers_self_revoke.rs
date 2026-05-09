//! Live integration test for the CLI peer self-revoke guard.
//!
//! Hermetic and opt-in: spawns a real daemon, extracts the operator's
//! own token prefix from `covenant peers list`, verifies `peers revoke`
//! refuses it without `--force`, then proves the operator token still
//! authenticates by running `covenant ping`.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
}

fn covenant_cli_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/covenant")
        .canonicalize()
        .expect("covenant CLI binary not built; run `cargo build -p covenant` first")
}

fn self_token_prefix(list_stdout: &str) -> String {
    let value: serde_json::Value =
        serde_json::from_str(list_stdout.trim()).expect("peers list --json must be valid JSON");
    let operator_pubkey_b58 = value["operator_pubkey_b58"]
        .as_str()
        .expect("peers list JSON must include operator_pubkey_b58");
    let peers = value["peers"]
        .as_array()
        .expect("peers list JSON must include peers array");
    let peer = peers
        .iter()
        .find(|peer| peer["agent_id"]["pubkey"].as_str() == Some(operator_pubkey_b58))
        .expect("peers list JSON must include operator peer row");
    peer["token_prefix"]
        .as_str()
        .expect("operator peer row must include token_prefix")
        .to_string()
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

#[tokio::test]
#[ignore = "live: spawns covenantd + runs `covenant peers revoke` self-guard subprocess"]
async fn live_cli_peers_self_revoke_is_rejected() {
    let home = tempfile::tempdir().expect("tempdir");
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
    let list = Command::new(&cli_exe)
        .arg("peers")
        .arg("list")
        .arg("--json")
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI (peers list)");
    let list_stdout = String::from_utf8_lossy(&list.stdout);
    let list_stderr = String::from_utf8_lossy(&list.stderr);
    assert!(
        list.status.success(),
        "peers list failed: status={:?} stdout={list_stdout:?} stderr={list_stderr:?}",
        list.status
    );
    let token_prefix = self_token_prefix(&list_stdout);

    let revoke = Command::new(&cli_exe)
        .arg("peers")
        .arg("revoke")
        .arg(&token_prefix)
        .arg("--json")
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI (peers revoke)");
    let revoke_stdout = String::from_utf8_lossy(&revoke.stdout);
    let revoke_stderr = String::from_utf8_lossy(&revoke.stderr);
    assert!(
        !revoke.status.success(),
        "self revoke unexpectedly succeeded: stdout={revoke_stdout:?} stderr={revoke_stderr:?}"
    );
    assert!(
        revoke_stderr.trim().is_empty(),
        "peers revoke --json must not mix human stderr with JSON stdout: stderr={revoke_stderr:?}"
    );
    let revoke_json: serde_json::Value =
        serde_json::from_str(revoke_stdout.trim()).expect("peers revoke --json must be valid JSON");
    assert_eq!(
        revoke_json.get("kind").and_then(serde_json::Value::as_str),
        Some("peer_revoke")
    );
    assert!(
        revoke_json
            .get("outcome")
            .and_then(|outcome| outcome.get("type"))
            .and_then(serde_json::Value::as_str)
            == Some("self_revoke_forbidden"),
        "self revoke JSON had unexpected shape: {revoke_json}"
    );

    let ping = Command::new(&cli_exe)
        .arg("ping")
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI (ping)");
    let ping_stdout = String::from_utf8_lossy(&ping.stdout);
    let ping_stderr = String::from_utf8_lossy(&ping.stderr);
    assert!(
        ping.status.success(),
        "operator auth broke after self-revoke rejection: status={:?} stdout={ping_stdout:?} stderr={ping_stderr:?}",
        ping.status
    );
    assert!(
        ping_stdout.contains("pong"),
        "ping stdout missing pong: stdout={ping_stdout:?} stderr={ping_stderr:?}"
    );

    let _ = child.kill().await;
}
