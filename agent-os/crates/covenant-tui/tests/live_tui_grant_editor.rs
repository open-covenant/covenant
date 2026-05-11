//! Live integration test for the TUI's grant editor end-to-end at
//! the App level.
//!
//! Drives the App's key handler as if a user typed `g memory.read
//! <Enter>`, then runs the IPC kickoff that the binary's event loop
//! would run (`take_pending_grant_submission` + `grant_capability`),
//! feeds the outcome back through `apply_grant_outcome`, and asserts
//! the final mode is `Mode::GrantResult` with the granted action
//! echoed.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenant-tui --test live_tui_grant_editor -- --ignored live_`.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use covenant_tui::ipc::grant_capability;
use covenant_tui::{App, Mode};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn type_chars(app: &mut App, text: &str) {
    for c in text.chars() {
        app.on_key(press(KeyCode::Char(c)));
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + drives the TUI grant editor end-to-end"]
async fn live_tui_grant_editor_round_trip_grants_capability() {
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

    // ── Drive the App through the editor: g, type, Enter.
    let mut app = App::new();
    app.on_key(press(KeyCode::Char('g')));
    type_chars(&mut app, "memory.read");
    app.on_key(press(KeyCode::Enter));

    // ── Mirror the binary's event loop: pull the pending action,
    //     run the real IPC call, feed the outcome back.
    let action = app
        .take_pending_grant_submission()
        .expect("Enter on non-empty grant editor must arm a submission");
    assert_eq!(action, "memory.read");

    let outcome = grant_capability(home.path(), &action, None, None)
        .await
        .expect("grant_capability: wire-level error");
    app.apply_grant_outcome(outcome);

    match app.mode() {
        Mode::GrantResult {
            action,
            subject_display,
            signature_b58,
        } => {
            assert_eq!(action, "memory.read");
            assert!(!subject_display.is_empty());
            assert!(!signature_b58.is_empty());
        }
        other => panic!("expected GrantResult after a successful daemon grant, got {other:?}"),
    }

    let _ = child.kill().await;
}
