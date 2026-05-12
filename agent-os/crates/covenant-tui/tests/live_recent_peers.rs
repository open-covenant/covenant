//! Live integration test for `covenant_tui::ipc::recent_peers`.
//!
//! Flow:
//!   1. Spawn covenantd against a fresh COVENANT_HOME so the only
//!      registered peer is the operator's own bootstrap entry.
//!   2. Call recent_peers via the TUI IPC client.
//!   3. Assert the response carries a non-empty Fetched envelope and
//!      that the operator's bootstrap row is present (its pubkey
//!      matches the daemon's identity, revoked_at is None, the token
//!      prefix is exactly the 6-char redaction).
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenant-tui --test live_recent_peers -- --ignored live_`.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use covenant_tui::ipc::{recent_peers, PeersFetchOutcome};
use tokio::process::Command;
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
}

fn covenantd_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/covenantd")
        .canonicalize()
        .expect("covenantd binary not built; run `cargo build -p covenantd` first")
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

fn read_peer_pubkey(home: &std::path::Path) -> [u8; 32] {
    let path = home.join("identity").join("local.key");
    let id = covenant_identity::LocalIdentity::load_or_create(&path, "user@local")
        .expect("load identity");
    id.pubkey_bytes()
}

#[tokio::test]
#[ignore = "live: spawns covenantd + reads the operator bootstrap entry back via recent_peers"]
async fn live_recent_peers_lists_operator_bootstrap_entry() {
    let home = tempfile::tempdir().expect("tempdir");

    let port = pick_free_port();
    let exe = covenantd_bin();
    let mut child = Command::new(&exe)
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

    let expected_pubkey = read_peer_pubkey(home.path());

    let outcome = recent_peers(home.path(), 10)
        .await
        .expect("recent_peers: wire-level error");
    let (peers, truncated) = match outcome {
        PeersFetchOutcome::Fetched { peers, truncated } => (peers, truncated),
        PeersFetchOutcome::Failed { message } => panic!("recent_peers failed: {message}"),
    };

    assert!(
        !peers.is_empty(),
        "fresh daemon must register the operator bootstrap peer"
    );
    assert!(
        !truncated,
        "single-entry registry must not report truncated"
    );

    let operator_row = peers
        .iter()
        .find(|p| p.agent_id.pubkey == expected_pubkey)
        .unwrap_or_else(|| {
            panic!(
                "operator bootstrap row missing from peers list: got {peers:?}, expected pubkey {expected_pubkey:?}"
            )
        });
    assert!(
        operator_row.revoked_at.is_none(),
        "operator bootstrap row must be live, got revoked_at = {:?}",
        operator_row.revoked_at
    );
    assert_eq!(
        operator_row.token_prefix.len(),
        6,
        "PeerSummary.token_prefix invariant: 6-char redaction, got {:?}",
        operator_row.token_prefix
    );

    let _ = child.kill().await;
}
