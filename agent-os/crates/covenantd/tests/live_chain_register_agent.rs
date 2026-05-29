//! Live coverage for `covenant chain register-agent --json`.
//!
//! End-to-end path: spawns a real covenantd against a temp HOME so
//! the CLI passes peer-auth, then runs the `register-agent` verb
//! against a real Solana validator with the settlement program
//! deployed at the supplied `--program-id`. The verb signs locally
//! with the operator keypair on disk and submits + confirms via the
//! Solana RPC URL.
//!
//! Opt-in (`#[ignore]`d) because it requires three external pieces
//! that are out of scope for default CI:
//!
//!   * a Solana validator reachable at `COVENANT_LIVE_CHAIN_RPC_URL`
//!     (`solana-test-validator` on the default port works);
//!   * the settlement program deployed at
//!     `COVENANT_LIVE_CHAIN_PROGRAM_ID` with `initialize_config`
//!     already run, so the config PDA exists;
//!   * an operator keypair at
//!     `COVENANT_LIVE_CHAIN_KEYPAIR_PATH` funded with at least
//!     enough lamports to cover the agent-PDA rent and the tx fee
//!     (~0.002 SOL).
//!
//! When the env vars are unset, the test exits early with a clear
//! reason so an operator running `cargo test -- --ignored` against a
//! laptop without a local validator sees the setup hint instead of
//! a confusing RPC timeout.
//!
//! Run from `agent-os/` after `cargo build -p covenant`:
//!
//! ```bash
//! solana-test-validator --reset &
//! anchor deploy   # in agent-os/, after `anchor build`
//! cargo test -p covenantd --test live_chain_register_agent -- \
//!     --ignored live_
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

fn random_hex_32() -> String {
    // 32 distinct bytes drawn from the nanosecond clock keep the
    // agent_key / metadata_hash / capability_hash unique across
    // re-runs against a long-lived validator, so the on-chain
    // agent-PDA does not collide with a previous register_agent.
    let mut bytes = [0u8; 32];
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        .wrapping_add(std::process::id() as u64);
    for b in bytes.iter_mut() {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *b = (seed >> 33) as u8;
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn random_agent_pubkey_b58() -> String {
    // Reuse the same byte source as the hashes; bs58-encoded random
    // 32-byte arrays parse as Solana Pubkey::from_str even though
    // they are not on-curve, which is fine because settlement's
    // RegisterAgent does not require the key to be on-curve.
    let hex = random_hex_32();
    let bytes: Vec<u8> = (0..32)
        .map(|i| {
            let s = &hex[2 * i..2 * i + 2];
            u8::from_str_radix(s, 16).expect("hex byte")
        })
        .collect();
    bs58::encode(bytes).into_string()
}

#[tokio::test]
#[ignore = "live: spawns covenantd + submits `covenant chain register-agent --json` against a localnet validator with a deployed settlement program"]
async fn live_cli_chain_register_agent_submits_and_confirms() {
    let program_id = match std::env::var("COVENANT_LIVE_CHAIN_PROGRAM_ID") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            eprintln!(
                "skip: COVENANT_LIVE_CHAIN_PROGRAM_ID not set. \
                 Deploy the settlement program with `anchor deploy` and export the resulting \
                 program id before re-running this test."
            );
            return;
        }
    };
    let rpc_url = std::env::var("COVENANT_LIVE_CHAIN_RPC_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8899".to_string());
    let keypair_path = match std::env::var("COVENANT_LIVE_CHAIN_KEYPAIR_PATH") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => {
            eprintln!(
                "skip: COVENANT_LIVE_CHAIN_KEYPAIR_PATH not set. \
                 Point it at an operator keypair JSON funded on the target cluster."
            );
            return;
        }
    };

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

    let agent_key = random_agent_pubkey_b58();
    let metadata_hash = random_hex_32();
    let capability_hash = random_hex_32();

    let output = Command::new(covenant_cli_bin())
        .args([
            "chain",
            "register-agent",
            "--cluster",
            "localnet",
            "--rpc-url",
            &rpc_url,
            "--program-id",
            &program_id,
            "--agent-key",
            &agent_key,
            "--metadata-hash",
            &metadata_hash,
            "--capability-hash",
            &capability_hash,
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
        .expect("run covenant chain register-agent");

    let _ = child.kill().await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "register-agent failed: status={:?} stdout={stdout} stderr={stderr}",
        output.status,
    );

    let envelope: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout was not JSON: {e}; raw={stdout}"));
    assert_eq!(envelope["kind"], "covenant.chain.tx.v1");
    assert_eq!(envelope["verb"], "register-agent");
    assert_eq!(envelope["status"], "confirmed");
    assert_eq!(envelope["cluster"], "localnet");
    assert_eq!(envelope["agent_key"], agent_key);
    let sig = envelope["signature"]
        .as_str()
        .expect("signature is a string");
    assert!(!sig.is_empty(), "signature is non-empty base58");
}
