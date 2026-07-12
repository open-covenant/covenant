//! Live integration test: the daemon refuses to start when
//! `$COVENANT_HOME/peers/operator.token` carries insecure file
//! permissions, failing loud before the socket appears.
//!
//! `require_operator_token_mode_0600` (covenantd lib.rs:15426) rejects any
//! operator-token mode with a group or world bit set (`mode & 0o077 != 0`)
//! and rejects a symlink at that path outright. The pure function is
//! unit-tested (`require_operator_token_mode_0600_pins_accept_reject_paths`
//! at lib.rs:54733), but the security-relevant decision is the daemon's:
//! `bootstrap_operator_token` (main.rs:663) calls it with `?` during
//! startup, so an insecure-permission token must abort boot — not be
//! silently regenerated or reused, which would hand the bootstrap
//! credential to every user who could already read the file. That
//! integration boundary (propagation through `?`, loud exit before the
//! Unix socket is created) is what this drives through a real process.
//!
//! The test pre-creates `operator.token` with VALID token content but a
//! world-readable `0o644` mode, spawns the real daemon, and pins that the
//! process exits non-zero without ever creating its socket — so a
//! regression that dropped or weakened the bootstrap check (and let the
//! daemon reuse the leaky token) surfaces as a socket appearing and the
//! process running instead of exiting.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_operator_token_insecure_mode_refuses_start -- --ignored live_`.

use covenant_peer_auth::PeerToken;
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
}

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies it refuses to start when operator.token has insecure (0o644) permissions"]
async fn live_covenantd_refuses_to_start_with_insecure_operator_token_mode() {
    let home = tempfile::tempdir().expect("tempdir");
    let sock = home.path().join("sock");

    // Pre-create operator.token with VALID content but a world-readable
    // mode. Content validity isolates the bite to the permission check: if
    // the bootstrap check were dropped, the daemon would parse this token,
    // re-register it, and start normally — so a socket appearing proves the
    // guard stopped firing.
    let token = PeerToken::generate();
    let token_dir = home.path().join("peers");
    std::fs::create_dir_all(&token_dir).expect("create peers dir");
    let token_path = token_dir.join("operator.token");
    std::fs::write(&token_path, token.to_b58()).expect("write operator.token");
    std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o644))
        .expect("chmod 0o644 to force insecure-mode reject");
    let observed_mode = std::fs::metadata(&token_path)
        .expect("stat operator.token")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        observed_mode, 0o644,
        "test preconditions: operator.token must be 0o644 before spawn, got {observed_mode:#o}"
    );

    let exe = env!("CARGO_BIN_EXE_covenantd");
    let child = Command::new(exe)
        .env("COVENANT_HOME", home.path())
        .env("COVENANT_HTTP_PORT", pick_free_port().to_string())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");

    // The daemon must exit (loud refusal), not run. wait_with_output
    // resolves only when the process terminates; a daemon that booted
    // despite the insecure token would run forever and time out.
    let outcome = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .expect("daemon did not exit within 20s — it started despite an insecure (0o644) operator token, so the bootstrap permission check did not fire");
    let output = outcome.expect("wait_with_output failed");

    assert!(
        !output.status.success(),
        "the daemon must exit non-zero on an insecure operator token, got status {:?}",
        output.status
    );

    // The socket must never have been created — boot aborted before the
    // server bound its listener.
    let mut socket_appeared = false;
    for _ in 0..5 {
        if sock.exists() {
            socket_appeared = true;
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    assert!(
        !socket_appeared,
        "the daemon created its socket despite an insecure operator token — the bootstrap 0600 check did not abort boot"
    );

    // The loud-failure context must reach stderr so an operator can
    // diagnose the refused start (the anyhow chain from bootstrap's `?`).
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("insecure permissions") || stderr.contains("0o600"),
        "stderr must name the insecure-permission refusal; got stderr: {stderr:?}"
    );
}
