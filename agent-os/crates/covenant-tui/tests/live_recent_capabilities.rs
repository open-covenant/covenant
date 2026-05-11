//! Live integration test for `covenant_tui::ipc::recent_capabilities`.
//!
//! Flow:
//!   1. Grant `memory.read` via grant_capability.
//!   2. Call recent_capabilities, assert at least one returned entry
//!      has the granted action and a non-zero signature.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenant-tui --test live_recent_capabilities -- --ignored live_`.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use covenant_tui::ipc::{
    grant_capability, recent_capabilities, CapabilitiesFetchOutcome, GrantOutcome,
};
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

#[tokio::test]
#[ignore = "live: spawns covenantd + drives recent_capabilities after a grant"]
async fn live_recent_capabilities_contains_granted_action() {
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

    match grant_capability(home.path(), "memory.read", None, None)
        .await
        .expect("grant_capability: wire-level error")
    {
        GrantOutcome::Granted { .. } => {}
        GrantOutcome::Failed { message } => panic!("grant failed: {message}"),
    }

    let outcome = recent_capabilities(home.path(), 10)
        .await
        .expect("recent_capabilities: wire-level error");
    let caps = match outcome {
        CapabilitiesFetchOutcome::Fetched { capabilities } => capabilities,
        CapabilitiesFetchOutcome::Failed { message } => {
            panic!("recent_capabilities failed: {message}")
        }
    };
    let memory_read = caps
        .iter()
        .find(|c| c.capability.action == "memory.read")
        .expect("recent_capabilities must list the granted memory.read action");
    assert!(
        memory_read.signature != [0u8; 64],
        "signed capability must carry a real (non-zero) signature"
    );

    let _ = child.kill().await;
}
