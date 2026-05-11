//! Live integration test for `covenant_tui::ipc::recent_memory`.
//!
//! Flow against a single covenantd:
//!   1. Grant `memory.write` and `memory.read.working` to the
//!      operator so the dispatch path can run and the read path
//!      can return the resulting record.
//!   2. Submit an intent so a working-tier MemoryRecord lands.
//!   3. Call `recent_memory(tier=Some(Working), limit=10)` and
//!      assert the new record is present in the returned list with
//!      a non-empty `text` field.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenant-tui --test live_recent_memory -- --ignored live_`.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use covenant_tui::ipc::{
    grant_capability, recent_memory, submit_intent, GrantOutcome, MemoryFetchOutcome,
};
use covenant_tui::SubmissionOutcome;
use covenant_types::MemoryTier;
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
#[ignore = "live: spawns covenantd + drives recent_memory after a write"]
async fn live_recent_memory_round_trips_a_working_tier_record() {
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

    // ── Grant memory.write so SubmitIntent's dispatch_intent gate
    //     passes through to the working-memory write path.
    match grant_capability(home.path(), "memory.write", None, None)
        .await
        .expect("grant_capability(memory.write): wire-level error")
    {
        GrantOutcome::Granted { .. } => {}
        GrantOutcome::Failed { message } => {
            panic!("grant memory.write failed: {message}")
        }
    }

    // ── Grant memory.read.working so the subsequent recent_memory
    //     query is not rejected by the read-side capability gate.
    match grant_capability(home.path(), "memory.read.working", None, None)
        .await
        .expect("grant_capability(memory.read.working): wire-level error")
    {
        GrantOutcome::Granted { .. } => {}
        GrantOutcome::Failed { message } => {
            panic!("grant memory.read.working failed: {message}")
        }
    }

    // ── Submit an intent. With memory.write granted the daemon
    //     completes dispatch and writes a working-tier record.
    let intent_text = "live recent_memory round-trip probe";
    match submit_intent(home.path(), intent_text)
        .await
        .expect("submit_intent: wire-level error")
    {
        SubmissionOutcome::Accepted { .. } => {}
        SubmissionOutcome::Failed { message } => {
            panic!("submit_intent failed after grant: {message}")
        }
    }

    // ── Fetch recent working-tier memory. The submitted intent's
    //     resulting record must appear in the list.
    let outcome = recent_memory(home.path(), Some(MemoryTier::Working), 10)
        .await
        .expect("recent_memory: wire-level error");
    let records = match outcome {
        MemoryFetchOutcome::Fetched { records } => records,
        MemoryFetchOutcome::Failed { message } => {
            panic!("recent_memory failed: {message}")
        }
    };
    assert!(
        !records.is_empty(),
        "memory tier must contain at least one record after a successful dispatch"
    );
    let working_only: Vec<_> = records
        .iter()
        .filter(|r| matches!(r.tier, MemoryTier::Working))
        .collect();
    assert!(
        !working_only.is_empty(),
        "tier filter must restrict to working-tier records"
    );
    let any_non_empty_text = working_only.iter().any(|r| !r.text.is_empty());
    assert!(
        any_non_empty_text,
        "at least one working-tier record must carry text"
    );

    let _ = child.kill().await;
}
