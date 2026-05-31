//! Live CLI coverage for `covenant bootstrap --json`.
//!
//! `bootstrap` is a CLI-side operation: it reads every `agents/*/agent.toml`
//! manifest, unions their `[capabilities] required` with the always-needed
//! `memory.write`, queries the daemon for existing grants, grants the missing
//! ones, and prints `bootstrap_result_json`
//! (`{ kind: "bootstrap_result", granted: [{action, signature_b58}],
//! already_granted: [..] }`). The JSON shape has a unit test, but no live test
//! drives the full CLI→daemon round-trip, leaving the grant path, the
//! manifest-union, and the idempotent re-run unexercised end to end.
//!
//! Two scenarios:
//! - grant then idempotent: against a daemon with no agents the first run
//!   grants exactly `memory.write` (with a signature) and reports nothing
//!   already-granted; a second run grants nothing and reports `memory.write`
//!   already-granted — proving the existing-grant skip makes re-running safe.
//! - manifest union: with an agent manifest requiring `tool.web_search`
//!   staged, a fresh daemon's run grants both `memory.write` and the agent's
//!   `tool.web_search` — proving bootstrap unions agent-required capabilities.
//!
//! Hermetic — bootstrap parses manifests CLI-side and never runs an agent, so
//! no agent binary is needed. `#[ignore]`'d, own tempdir per test. Build the
//! CLI first (`cargo build -p covenant`); run with
//! `cargo test -p covenantd --test live_cli_bootstrap_json -- --ignored live_`.

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

async fn spawn_daemon(home: &Path) -> Child {
    let port = pick_free_port();
    let daemon_exe = env!("CARGO_BIN_EXE_covenantd");
    let child = Command::new(daemon_exe)
        .env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home)
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

/// Run `covenant bootstrap --json` against `home` and parse the result object.
async fn bootstrap_json(cli: &Path, home: &Path) -> Value {
    let output = Command::new(cli)
        .args(["bootstrap", "--json"])
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
        "bootstrap --json failed: status={:?} stdout={stdout:?} stderr={stderr:?}",
        output.status,
    );
    assert!(
        stderr.trim().is_empty(),
        "bootstrap --json must not emit stderr on success: {stderr:?}",
    );
    serde_json::from_str(stdout.trim()).expect("bootstrap --json must be valid JSON")
}

fn granted_actions(result: &Value) -> Vec<String> {
    result["granted"]
        .as_array()
        .expect("granted must be an array")
        .iter()
        .map(|g| {
            assert!(
                g["signature_b58"].as_str().is_some_and(|s| !s.is_empty()),
                "every granted entry must carry a signature: {g:?}",
            );
            g["action"]
                .as_str()
                .expect("granted action must be a string")
                .to_string()
        })
        .collect()
}

fn already_actions(result: &Value) -> Vec<String> {
    result["already_granted"]
        .as_array()
        .expect("already_granted must be an array")
        .iter()
        .map(|a| {
            a.as_str()
                .expect("already_granted entry must be a string")
                .to_string()
        })
        .collect()
}

#[tokio::test]
#[ignore = "live: spawns covenantd + runs `covenant bootstrap --json` twice asserting grant then idempotent re-run"]
async fn live_cli_bootstrap_json_grants_then_idempotent() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let cli = covenant_cli_bin();

    // No agents loaded, so memory.write is the only required capability.
    let first = bootstrap_json(&cli, home.path()).await;
    assert_eq!(
        first["kind"], "bootstrap_result",
        "bootstrap must answer the bootstrap_result envelope: {first:?}",
    );
    assert_eq!(
        granted_actions(&first),
        ["memory.write"],
        "the first run must grant exactly memory.write when no agents are loaded: {first:?}",
    );
    assert!(
        already_actions(&first).is_empty(),
        "nothing is pre-granted on a fresh daemon: {first:?}",
    );

    // Re-running must be a no-op: the existing-grant skip moves memory.write
    // from granted to already_granted.
    let second = bootstrap_json(&cli, home.path()).await;
    assert_eq!(
        second["kind"], "bootstrap_result",
        "the re-run must still answer bootstrap_result: {second:?}",
    );
    assert!(
        granted_actions(&second).is_empty(),
        "the re-run must grant nothing — bootstrap is idempotent: {second:?}",
    );
    assert!(
        already_actions(&second).contains(&"memory.write".to_string()),
        "the re-run must report memory.write already-granted: {second:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts `covenant bootstrap --json` unions an agent manifest's required capabilities"]
async fn live_cli_bootstrap_json_unions_agent_required_capabilities() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let cli = covenant_cli_bin();

    // Stage a manifest requiring a capability beyond memory.write. bootstrap
    // reads agent.toml CLI-side, so no agent binary and no daemon reload is
    // needed for the union to take effect.
    let agent_dir = home.path().join("agents").join("probe");
    std::fs::create_dir_all(&agent_dir).expect("agents dir");
    std::fs::write(
        agent_dir.join("agent.toml"),
        r#"
[agent]
id = "probe"
name = "Probe Agent"
version = "0.0.1"
runtime = "rust-bin"
entry = "probe"

[capabilities]
required = ["tool.web_search"]
"#,
    )
    .expect("write manifest");

    let result = bootstrap_json(&cli, home.path()).await;
    assert_eq!(
        result["kind"], "bootstrap_result",
        "bootstrap must answer the bootstrap_result envelope: {result:?}",
    );
    let granted = granted_actions(&result);
    assert!(
        granted.contains(&"memory.write".to_string()),
        "bootstrap must always grant memory.write: {result:?}",
    );
    assert!(
        granted.contains(&"tool.web_search".to_string()),
        "bootstrap must union the agent manifest's required capability: {result:?}",
    );
    assert!(
        already_actions(&result).is_empty(),
        "on a fresh daemon both required capabilities are newly granted: {result:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
