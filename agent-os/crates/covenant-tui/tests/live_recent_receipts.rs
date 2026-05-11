//! Live integration test for `covenant_tui::ipc::recent_receipts`.
//!
//! Flow:
//!   1. Grant `memory.write` so the dispatch path can write a
//!      working-tier record. The same path persists a memory
//!      settlement receipt for the credit consumption.
//!   2. Grant `chain.receipts` so `RecentReceipts` is not rejected.
//!   3. Submit an intent so a working-tier memory write lands.
//!   4. Call `recent_receipts(limit=10)` via the TUI IPC client and
//!      assert the returned list is non-empty and at least one row
//!      describes a memory resource.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenant-tui --test live_recent_receipts -- --ignored live_`.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use covenant_tui::ipc::{
    grant_capability, recent_receipts, submit_intent, GrantOutcome, ReceiptsFetchOutcome,
};
use covenant_tui::SubmissionOutcome;
use covenant_types::ResourceKind;
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
#[ignore = "live: spawns covenantd + drives recent_receipts after submitting an intent"]
async fn live_recent_receipts_lists_memory_receipt_after_dispatch() {
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

    for action in ["memory.write", "chain.receipts"] {
        match grant_capability(home.path(), action, None, None)
            .await
            .unwrap_or_else(|e| panic!("grant_capability({action}) wire error: {e:#}"))
        {
            GrantOutcome::Granted { .. } => {}
            GrantOutcome::Failed { message } => panic!("grant {action} failed: {message}"),
        }
    }

    match submit_intent(home.path(), "live recent_receipts probe")
        .await
        .expect("submit_intent: wire-level error")
    {
        SubmissionOutcome::Accepted { .. } => {}
        SubmissionOutcome::Failed { message } => panic!("submit_intent failed: {message}"),
    }

    let outcome = recent_receipts(home.path(), 10)
        .await
        .expect("recent_receipts: wire-level error");
    let receipts = match outcome {
        ReceiptsFetchOutcome::Fetched { receipts } => receipts,
        ReceiptsFetchOutcome::Failed { message } => panic!("recent_receipts failed: {message}"),
    };
    assert!(
        !receipts.is_empty(),
        "submit_intent with memory.write must produce at least one settlement receipt"
    );
    assert!(
        receipts
            .iter()
            .any(|r| matches!(r.resource, ResourceKind::Memory)),
        "at least one receipt must describe a memory resource; got {receipts:?}"
    );

    let _ = child.kill().await;
}
