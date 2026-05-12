//! Live integration test that no daemon is running.
//!
//! Confirms each TUI fetch helper returns `IpcError::DaemonNotRunning`
//! when there is no covenantd listening at `$COVENANT_HOME/sock`, and
//! that the rendered message names the resolved socket path plus the
//! `covenantd start` hint. Helpers used here have no capability gate
//! or auth precondition past the socket — the failure is unambiguously
//! about the missing socket, not about an unrelated read-side check.
//!
//! Hermetic — no covenantd is spawned at all. `#[ignore]`'d. Run
//! with `cargo test -p covenant-tui --test live_tui_daemon_offline -- --ignored live_`.

use covenant_tui::ipc::{recent_audit, recent_peers, IpcError};

#[tokio::test]
#[ignore = "live: probes recent_audit against an empty $COVENANT_HOME with no daemon"]
async fn live_recent_audit_without_daemon_returns_friendly_error() {
    let home = tempfile::tempdir().expect("tempdir");

    let err = recent_audit(home.path(), 10)
        .await
        .expect_err("recent_audit must fail when the socket does not exist");

    let expected_sock = home.path().join("sock");
    match &err {
        IpcError::DaemonNotRunning { sock_path } => {
            assert_eq!(sock_path, &expected_sock);
        }
        other => panic!("expected DaemonNotRunning, got {other:?}"),
    }

    let message = format!("{err}");
    assert!(
        message.contains(expected_sock.to_str().unwrap()),
        "rendered message must surface the resolved sock path: {message}"
    );
    assert!(
        message.contains("covenantd start"),
        "rendered message must include the fix hint: {message}"
    );
}

#[tokio::test]
#[ignore = "live: probes recent_peers against an empty $COVENANT_HOME with no daemon"]
async fn live_recent_peers_without_daemon_returns_friendly_error() {
    let home = tempfile::tempdir().expect("tempdir");

    let err = recent_peers(home.path(), 10)
        .await
        .expect_err("recent_peers must fail when the socket does not exist");

    let expected_sock = home.path().join("sock");
    match &err {
        IpcError::DaemonNotRunning { sock_path } => {
            assert_eq!(sock_path, &expected_sock);
        }
        other => panic!("expected DaemonNotRunning, got {other:?}"),
    }

    let message = format!("{err}");
    assert!(
        message.contains(expected_sock.to_str().unwrap()),
        "rendered message must surface the resolved sock path: {message}"
    );
    assert!(
        message.contains("covenantd start"),
        "rendered message must include the fix hint: {message}"
    );
}
