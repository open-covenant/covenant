//! Live CLI coverage for `covenant peers revoke --json`.
//!
//! Uses a temp Covenant home, a pre-seeded guest peer, and the real CLI
//! subprocess to pin the machine-readable revoke outcomes that
//! automation consumes.

use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;

struct JsonCliOutput {
    success: bool,
    stdout: String,
    stderr: String,
    value: Value,
}

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

async fn revoke_json(
    cli_exe: &std::path::Path,
    home: &std::path::Path,
    token_prefix: &str,
) -> JsonCliOutput {
    let output = Command::new(cli_exe)
        .arg("peers")
        .arg("revoke")
        .arg(token_prefix)
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
        stderr.trim().is_empty(),
        "peers revoke --json must not mix human stderr with JSON stdout: stderr={stderr:?}"
    );
    let value = serde_json::from_str(stdout.trim())
        .expect("peers revoke --json must emit one valid JSON object");

    JsonCliOutput {
        success: output.status.success(),
        stdout,
        stderr,
        value,
    }
}

fn assert_revoke_summary(
    output: &JsonCliOutput,
    expected_type: &str,
    expected_display: &str,
    expected_pubkey: &str,
    expected_prefix: &str,
    full_token: &str,
) {
    assert!(
        output.success,
        "{expected_type} should be a success outcome"
    );
    assert_eq!(output.value["kind"].as_str(), Some("peer_revoke"));
    let outcome = &output.value["outcome"];
    assert_eq!(outcome["type"].as_str(), Some(expected_type));
    assert_eq!(
        outcome["agent_id"]["display"].as_str(),
        Some(expected_display)
    );
    assert_eq!(
        outcome["agent_id"]["pubkey"].as_str(),
        Some(expected_pubkey)
    );
    assert_eq!(outcome["token_prefix"].as_str(), Some(expected_prefix));
    assert!(
        outcome["revoked_at"].as_u64().is_some(),
        "{expected_type} must carry revoked_at: {outcome:?}"
    );
    assert!(
        !output.stdout.contains(full_token) && !output.stderr.contains(full_token),
        "full peer token leaked in peers revoke --json output"
    );
}

#[tokio::test]
#[ignore = "live: spawns covenantd + runs `covenant peers revoke --json` subprocess"]
async fn live_cli_peers_revoke_json_round_trip() {
    let home = tempfile::tempdir().expect("tempdir");
    let guest_token = PeerToken::from_bytes([24u8; 32]);
    let guest_token_b58 = guest_token.to_b58();
    let guest_prefix: String = guest_token_b58.chars().take(12).collect();
    let guest_token_prefix: String = guest_token_b58.chars().take(6).collect();
    let guest_pubkey = [42u8; 32];
    let guest_pubkey_b58 = bs58::encode(guest_pubkey).into_string();
    let guest_display = "guest-revoke-json@local";

    let amb_token_a = PeerToken::from_bytes({
        let mut bytes = [0u8; 32];
        bytes[31] = 1;
        bytes
    });
    let amb_token_b = PeerToken::from_bytes({
        let mut bytes = [0u8; 32];
        bytes[31] = 2;
        bytes
    });
    let amb_prefix = "111111";
    let amb_a_pubkey = [7u8; 32];
    let amb_b_pubkey = [8u8; 32];
    let amb_a_pubkey_b58 = bs58::encode(amb_a_pubkey).into_string();
    let amb_b_pubkey_b58 = bs58::encode(amb_b_pubkey).into_string();

    let registry_path = home.path().join("peers").join("registry.jsonl");
    {
        let registry = JsonlPeerRegistry::open(registry_path)
            .await
            .expect("open seed registry");
        registry
            .register(PeerEntry {
                token: guest_token,
                agent_id: AgentId::new(guest_display, guest_pubkey),
                registered_at: 1_700_000_000_000,
            })
            .await
            .expect("seed guest peer");
        registry
            .register(PeerEntry {
                token: amb_token_a,
                agent_id: AgentId::new("amb-a@local", amb_a_pubkey),
                registered_at: 1_700_000_000_001,
            })
            .await
            .expect("seed ambiguous peer a");
        registry
            .register(PeerEntry {
                token: amb_token_b,
                agent_id: AgentId::new("amb-b@local", amb_b_pubkey),
                registered_at: 1_700_000_000_002,
            })
            .await
            .expect("seed ambiguous peer b");
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

    let revoked = revoke_json(&cli_exe, home.path(), &guest_prefix).await;
    assert_revoke_summary(
        &revoked,
        "revoked",
        guest_display,
        &guest_pubkey_b58,
        &guest_token_prefix,
        &guest_token_b58,
    );

    let already_revoked = revoke_json(&cli_exe, home.path(), &guest_prefix).await;
    assert_revoke_summary(
        &already_revoked,
        "already_revoked",
        guest_display,
        &guest_pubkey_b58,
        &guest_token_prefix,
        &guest_token_b58,
    );

    let not_found = revoke_json(&cli_exe, home.path(), "0").await;
    assert!(
        !not_found.success,
        "not_found must preserve non-zero automation exit status"
    );
    assert_eq!(not_found.value["kind"].as_str(), Some("peer_revoke"));
    assert_eq!(
        not_found.value["outcome"]["type"].as_str(),
        Some("not_found")
    );

    let ambiguous = revoke_json(&cli_exe, home.path(), amb_prefix).await;
    assert!(
        !ambiguous.success,
        "ambiguous must preserve non-zero automation exit status"
    );
    assert_eq!(ambiguous.value["kind"].as_str(), Some("peer_revoke"));
    assert_eq!(
        ambiguous.value["outcome"]["type"].as_str(),
        Some("ambiguous")
    );
    assert_eq!(
        ambiguous.value["outcome"]["truncated"].as_bool(),
        Some(false)
    );
    let matches = ambiguous.value["outcome"]["matches"]
        .as_array()
        .expect("ambiguous matches must be an array");
    assert!(
        matches
            .iter()
            .any(|m| m["agent_id"]["pubkey"] == amb_a_pubkey_b58),
        "ambiguous output missing peer a: {matches:?}"
    );
    assert!(
        matches
            .iter()
            .any(|m| m["agent_id"]["pubkey"] == amb_b_pubkey_b58),
        "ambiguous output missing peer b: {matches:?}"
    );

    let _ = child.kill().await;
}
