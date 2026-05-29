//! Live coverage for `covenant chain stake --json`.
//!
//! End-to-end path: spawns a real covenantd against a temp HOME so
//! the CLI passes peer-auth, then runs the `stake` verb against a
//! real Solana validator with the settlement program deployed. The
//! verb signs locally with the operator keypair and submits +
//! confirms via the Solana RPC URL.
//!
//! Opt-in (`#[ignore]`d) because it requires four external pieces
//! beyond default CI:
//!
//!   * a Solana validator reachable at `COVENANT_LIVE_CHAIN_RPC_URL`
//!     (`solana-test-validator` on the default port works);
//!   * the settlement program deployed at
//!     `COVENANT_LIVE_CHAIN_PROGRAM_ID` with `initialize_config`
//!     already run, so the config PDA exists;
//!   * an operator keypair at `COVENANT_LIVE_CHAIN_KEYPAIR_PATH`
//!     that has already been registered via `chain register-agent`
//!     for the same `--agent-key`, so the agent PDA exists and
//!     points back at the operator;
//!   * an owner_covnt token account at
//!     `COVENANT_LIVE_CHAIN_OWNER_COVNT` (covenant token account
//!     owned by the operator, funded with at least --amount
//!     tokens) and a `stake_vault` token account at
//!     `COVENANT_LIVE_CHAIN_STAKE_VAULT` whose authority is the
//!     stake-position PDA derived from (program_id, agent_key,
//!     operator).
//!
//! When the env vars are unset, the test exits early with a clear
//! reason so an operator running `cargo test -- --ignored` against
//! a laptop without a localnet validator sees the setup hint
//! instead of a confusing RPC timeout.
//!
//! Run from `agent-os/` after `cargo build -p covenant`:
//!
//! ```bash
//! solana-test-validator --reset &
//! anchor deploy   # in agent-os/, after `anchor build`
//! # ... seed config + register-agent + create owner_covnt + create stake_vault ...
//! cargo test -p covenantd --test live_chain_stake -- --ignored live_
//! ```

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

fn env_or_skip(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => None,
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + submits `covenant chain stake --json` against a localnet validator with a deployed settlement program + funded owner_covnt and stake_vault token accounts"]
async fn live_cli_chain_stake_submits_and_confirms() {
    let program_id = match env_or_skip("COVENANT_LIVE_CHAIN_PROGRAM_ID") {
        Some(v) => v,
        None => {
            eprintln!(
                "skip: COVENANT_LIVE_CHAIN_PROGRAM_ID not set. \
                 Deploy the settlement program with `anchor deploy` and export the program id."
            );
            return;
        }
    };
    let rpc_url = std::env::var("COVENANT_LIVE_CHAIN_RPC_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8899".to_string());
    let keypair_path = match env_or_skip("COVENANT_LIVE_CHAIN_KEYPAIR_PATH") {
        Some(v) => PathBuf::from(v),
        None => {
            eprintln!(
                "skip: COVENANT_LIVE_CHAIN_KEYPAIR_PATH not set. \
                 Point it at an operator keypair JSON funded on the target cluster."
            );
            return;
        }
    };
    let agent_key = match env_or_skip("COVENANT_LIVE_CHAIN_AGENT_KEY") {
        Some(v) => v,
        None => {
            eprintln!(
                "skip: COVENANT_LIVE_CHAIN_AGENT_KEY not set. \
                 Point it at the base58 agent_key that was registered via `chain register-agent`."
            );
            return;
        }
    };
    let owner_covnt = match env_or_skip("COVENANT_LIVE_CHAIN_OWNER_COVNT") {
        Some(v) => v,
        None => {
            eprintln!(
                "skip: COVENANT_LIVE_CHAIN_OWNER_COVNT not set. \
                 Point it at the operator's COVNT token account address."
            );
            return;
        }
    };
    let stake_vault = match env_or_skip("COVENANT_LIVE_CHAIN_STAKE_VAULT") {
        Some(v) => v,
        None => {
            eprintln!(
                "skip: COVENANT_LIVE_CHAIN_STAKE_VAULT not set. \
                 Point it at the stake vault token account whose authority is the stake-position PDA."
            );
            return;
        }
    };
    let amount =
        std::env::var("COVENANT_LIVE_CHAIN_STAKE_AMOUNT").unwrap_or_else(|_| "1000".to_string());
    let lock_until = std::env::var("COVENANT_LIVE_CHAIN_STAKE_LOCK_UNTIL")
        .unwrap_or_else(|_| "1893456000".to_string()); // 2030-01-01 epoch seconds

    let home = tempfile::tempdir().expect("tempdir");
    let port = pick_free_port();
    let daemon_exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(daemon_exe)
        .env("COVENANT_HOME", home.path())
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("COVENANT_SOLANA_CLUSTER", "localnet")
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

    let output = Command::new(covenant_cli_bin())
        .args([
            "chain",
            "stake",
            "--cluster",
            "localnet",
            "--rpc-url",
            &rpc_url,
            "--program-id",
            &program_id,
            "--agent-key",
            &agent_key,
            "--owner-covnt",
            &owner_covnt,
            "--stake-vault",
            &stake_vault,
            "--amount",
            &amount,
            "--lock-until",
            &lock_until,
            "--keypair",
            keypair_path.to_str().expect("utf-8 keypair path"),
            "--confirm-timeout-ms",
            "60000",
            "--json",
        ])
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("run covenant chain stake");

    let _ = child.kill().await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stake failed: status={:?} stdout={stdout} stderr={stderr}",
        output.status,
    );

    let envelope: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout was not JSON: {e}; raw={stdout}"));
    assert_eq!(envelope["kind"], "covenant.chain.tx.v1");
    assert_eq!(envelope["verb"], "stake");
    assert_eq!(envelope["status"], "confirmed");
    assert_eq!(envelope["cluster"], "localnet");
    assert_eq!(envelope["agent_key"], agent_key);
    let sig = envelope["signature"]
        .as_str()
        .expect("signature is a string");
    assert!(!sig.is_empty(), "signature is non-empty base58");
    assert!(envelope["amount"].is_number(), "amount is numeric");
    assert!(envelope["lock_until"].is_number(), "lock_until is numeric");
}
