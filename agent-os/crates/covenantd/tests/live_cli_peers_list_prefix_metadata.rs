//! Live CLI coverage for the `covenant peers list --json` envelope's
//! prefix-match metadata. Seeds three guest peers with disjoint
//! pubkey base58 prefixes, then runs the CLI subprocess with a prefix
//! that intentionally matches exactly one of them. Asserts:
//!
//! - `filter_pubkey_prefix` echoes the requested value (string).
//! - `matched_count` equals 1.
//! - `truncated` is false (so `matched_count` is the exhaustive
//!   match count, not a post-truncation undercount).
//! - `peers[0].agent_id.pubkey` equals the seeded pubkey of the only
//!   matching peer — protects against a "prefix matches but the
//!   daemon returned the wrong row" regression.
//!
//! Also asserts the no-prefix shape: `filter_pubkey_prefix` is null,
//! `matched_count` reflects every visible row (operator + three
//! guests), confirming the field is always present rather than
//! conditional on `--prefix`.
//!
//! Hermetic — no external services. `#[ignore]`'d. Build prereq:
//! `cargo build -p covenant`. Run with
//! `cargo test -p covenantd --test live_cli_peers_list_prefix_metadata -- --ignored live_`.

use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
use serde_json::Value;
use std::path::{Path, PathBuf};
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

async fn run_cli_json(cli: &Path, home: &Path, args: &[&str]) -> Value {
    let out = Command::new(cli)
        .args(args)
        .env("COVENANT_HOME", home)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "CLI {args:?} failed: status={:?} stdout={stdout:?} stderr={stderr:?}",
        out.status,
    );
    serde_json::from_str(stdout.trim()).expect("CLI stdout must be a single JSON object")
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts peers list --prefix --json envelope echoes filter and exhaustive match count"]
async fn live_cli_peers_list_prefix_metadata_round_trip() {
    let home = tempfile::tempdir().expect("tempdir");

    let alpha_pubkey = [0xAAu8; 32];
    let beta_pubkey = [0xBBu8; 32];
    let gamma_pubkey = [0xCCu8; 32];
    let alpha_b58 = bs58::encode(alpha_pubkey).into_string();
    let beta_b58 = bs58::encode(beta_pubkey).into_string();
    let gamma_b58 = bs58::encode(gamma_pubkey).into_string();
    let alpha_prefix: String = alpha_b58.chars().take(6).collect();
    assert!(
        !beta_b58.starts_with(&alpha_prefix) && !gamma_b58.starts_with(&alpha_prefix),
        "test invariant: alpha prefix must not match beta or gamma — alpha={alpha_b58}, beta={beta_b58}, gamma={gamma_b58}, prefix={alpha_prefix}",
    );

    let registry_path = home.path().join("peers").join("registry.jsonl");
    {
        let registry = JsonlPeerRegistry::open(registry_path)
            .await
            .expect("open seed registry");
        for (display, pubkey) in [
            ("guest-alpha@local", alpha_pubkey),
            ("guest-beta@local", beta_pubkey),
            ("guest-gamma@local", gamma_pubkey),
        ] {
            registry
                .register(PeerEntry {
                    token: PeerToken::generate(),
                    agent_id: AgentId::new(display, pubkey),
                    registered_at: 1_700_000_000_000,
                })
                .await
                .expect("seed guest");
        }
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

    let cli = covenant_cli_bin();

    let no_prefix = run_cli_json(
        &cli,
        home.path(),
        &["peers", "list", "--limit", "20", "--json"],
    )
    .await;
    assert_eq!(no_prefix["kind"], "peer_list");
    assert!(
        no_prefix["filter_pubkey_prefix"].is_null(),
        "filter_pubkey_prefix must be null when --prefix is not supplied so the field is always present: {no_prefix:?}",
    );
    let no_prefix_count = no_prefix["matched_count"]
        .as_u64()
        .expect("matched_count must be present as a number");
    assert_eq!(
        no_prefix_count, 4,
        "unfiltered registry must surface operator + 3 guests = 4 rows; got matched_count={no_prefix_count} value={no_prefix:?}",
    );
    assert_eq!(no_prefix["truncated"], false);

    let filtered = run_cli_json(
        &cli,
        home.path(),
        &[
            "peers",
            "list",
            "--limit",
            "20",
            "--prefix",
            &alpha_prefix,
            "--json",
        ],
    )
    .await;
    assert_eq!(filtered["kind"], "peer_list");
    assert_eq!(
        filtered["filter_pubkey_prefix"].as_str(),
        Some(alpha_prefix.as_str()),
        "filter_pubkey_prefix must echo the requested value: {filtered:?}",
    );
    assert_eq!(
        filtered["matched_count"].as_u64(),
        Some(1),
        "alpha prefix must narrow to exactly one row: {filtered:?}",
    );
    assert_eq!(
        filtered["truncated"], false,
        "single-match prefix under default limit must not be truncated, so matched_count is the exhaustive count not a post-truncation undercount: {filtered:?}",
    );
    let peers = filtered["peers"].as_array().expect("peers array");
    assert_eq!(peers.len(), 1);
    assert_eq!(
        peers[0]["agent_id"]["pubkey"].as_str(),
        Some(alpha_b58.as_str()),
        "matched row must carry the alpha pubkey (catches a wrong-prefix regression): peer={:?}",
        peers[0],
    );

    let _ = child.kill().await;
}
