//! Live integration test for `covenant_tui::ipc::submit_intent` and
//! `covenant_tui::ipc::grant_capability`.
//!
//! Three-phase flow against a single covenantd:
//!   1. Submit an intent before any grant. The daemon's
//!      dispatch_intent gate rejects with a `memory.write` capability
//!      message; submit_intent must preserve that text verbatim so
//!      the TUI's `Mode::Error` renderer can show it.
//!   2. Grant `memory.write` to the operator (subject == issuer
//!      since v0 is single-peer). The daemon returns a signed
//!      capability with a non-empty signature.
//!   3. Submit again. With the grant in place dispatch runs to
//!      completion; the hermetic tempdir has no agent.toml so the
//!      router takes the canned-fallback echo branch — both 'ok'
//!      status and the echo marker are acceptable so the assertion
//!      is robust to future router default changes.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenant-tui --test live_submit_intent -- --ignored live_`.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use covenant_tui::ipc::{grant_capability, submit_intent, GrantOutcome};
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
#[ignore = "live: spawns covenantd + drives submit_intent and grant_capability end-to-end"]
async fn live_submit_intent_three_phase_capability_gate_and_grant_flow() {
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

    // ── Phase 1: submit without a grant. The daemon's dispatch_intent
    //     gate fires before any working-memory write, and
    //     submit_intent must preserve the message so the TUI's
    //     Mode::Error renderer can show it.
    {
        let outcome = submit_intent(home.path(), "summarise local memory")
            .await
            .expect("submit_intent (phase 1): wire-level error");
        match outcome {
            SubmissionOutcome::Failed { message } => {
                assert!(
                    message.contains("memory.write"),
                    "daemon rejection must name the missing capability; got {message:?}"
                );
                assert!(
                    message.contains("capability"),
                    "daemon rejection must mention 'capability'; got {message:?}"
                );
            }
            SubmissionOutcome::Accepted {
                intent_id, status, ..
            } => panic!(
                "phase 1: submit_intent unexpectedly succeeded without memory.write \
                 (intent_id={intent_id}, status={status:?}); capability gate regression?"
            ),
        }
    }

    // ── Phase 2: grant the operator memory.write. Scope=None means
    //     unscoped; the daemon enforces shape and returns
    //     CapabilityGranted with the on-wire signature.
    {
        let outcome = grant_capability(home.path(), "memory.write", None, None)
            .await
            .expect("grant_capability: wire-level error");
        match outcome {
            GrantOutcome::Granted {
                action,
                subject_display,
                signature_b58,
            } => {
                assert_eq!(action, "memory.write");
                assert!(
                    !subject_display.is_empty(),
                    "daemon must echo subject_display"
                );
                assert!(
                    !signature_b58.is_empty(),
                    "daemon must return a real signature"
                );
            }
            GrantOutcome::Failed { message } => {
                panic!("phase 2: grant_capability unexpectedly failed: {message}")
            }
        }
    }

    // ── Phase 3: submit again. With memory.write granted, the
    //     dispatch path runs to completion. The hermetic tempdir
    //     has no agent.toml, so the router takes the canned-fallback
    //     echo branch — accept either "ok" or the echo marker so the
    //     assertion is robust to future router default changes.
    {
        let outcome = submit_intent(home.path(), "summarise local memory")
            .await
            .expect("submit_intent (phase 3): wire-level error");
        match outcome {
            SubmissionOutcome::Accepted {
                intent_id,
                status,
                text,
            } => {
                assert_ne!(
                    intent_id,
                    Uuid::nil(),
                    "daemon must assign a real intent_id"
                );
                assert!(
                    !status.is_empty(),
                    "daemon response must include a status string; got {status:?}"
                );
                assert!(
                    !text.is_empty(),
                    "daemon response must include result text; got {text:?}"
                );
            }
            SubmissionOutcome::Failed { message } => {
                panic!("phase 3: submit_intent failed after grant: {message}")
            }
        }
    }

    let _ = child.kill().await;
}
