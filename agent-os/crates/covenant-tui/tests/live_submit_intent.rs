//! Live integration test for `covenant_tui::ipc::submit_intent`.
//!
//! Spawns covenantd against a tempdir HOME, waits for the bootstrap
//! operator token to land, then drives the same submit_intent
//! function the TUI binary uses. The bootstrap operator has no
//! `memory.write` capability (the daemon does not auto-grant it),
//! so dispatch_intent rejects the submission at the capability gate
//! and returns `Response::Error` with a specific message. The test
//! asserts `submit_intent` correctly maps that to
//! `SubmissionOutcome::Failed` and preserves the daemon's message
//! verbatim — that's the contract the TUI's render layer relies on
//! to surface gate failures in `Mode::Error`.
//!
//! A future slice can add a grant step + assert the Accepted path
//! once the TUI supports `g <action>` for self-grants.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenant-tui --test live_submit_intent -- --ignored live_`.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use covenant_tui::ipc::submit_intent;
use covenant_tui::SubmissionOutcome;
use tokio::process::Command;
use tokio::time::sleep;
use uuid::Uuid;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
}

/// Locates the covenantd binary built by the workspace. Cargo does
/// not set `CARGO_BIN_EXE_covenantd` for tests in a different
/// package, so the test follows the same `CARGO_MANIFEST_DIR` +
/// relative-path pattern used by `live_cli_peers_purge_json.rs`. The
/// caller is responsible for building covenantd first (the
/// verification command does so explicitly).
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
#[ignore = "live: spawns covenantd + drives covenant_tui::ipc::submit_intent end-to-end"]
async fn live_submit_intent_surfaces_daemon_capability_gate() {
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

    let outcome = submit_intent(home.path(), "summarise local memory")
        .await
        .expect("submit_intent: wire-level error");

    match outcome {
        SubmissionOutcome::Failed { message } => {
            assert!(
                message.contains("memory.write"),
                "daemon rejection must name the missing capability so the \
                 TUI can render it in Mode::Error; got {message:?}"
            );
            assert!(
                message.contains("capability"),
                "daemon rejection must mention 'capability' so the message \
                 carries the gate context; got {message:?}"
            );
        }
        SubmissionOutcome::Accepted {
            intent_id, status, ..
        } => {
            // The bootstrap operator must NOT have memory.write
            // automatically; the daemon's grant model requires
            // explicit self-grants. If this branch ever fires, the
            // capability model has regressed silently.
            panic!(
                "submit_intent unexpectedly succeeded without a memory.write grant \
                 (intent_id={intent_id}, status={status:?}); capability gate regression?"
            );
        }
    }
    // Silence the unused-import warning until a follow-up slice
    // exercises the Accepted path with a real grant.
    let _ = Uuid::nil();

    let _ = child.kill().await;
}
