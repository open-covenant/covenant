//! Live integration test for Sprint 60: spawns covenantd against a
//! tempdir HOME, authenticates with the bootstrap operator token,
//! issues `RotateOperatorToken` over real IPC, then opens a fresh
//! connection and verifies that (a) the new token authenticates,
//! (b) the old token does NOT authenticate, and (c) the on-disk
//! `peers/operator.token` file holds the new b58 with mode 0600.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_rotate_token -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
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

async fn read_operator_token(home: &std::path::Path) -> String {
    let path = home.join("peers").join("operator.token");
    for _ in 0..50 {
        if let Ok(s) = std::fs::read_to_string(&path) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("operator token never appeared at {}", path.display());
}

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

async fn authenticate(stream: &mut UnixStream, token_b58: &str) -> Response {
    req(
        stream,
        Request::Authenticate {
            token_b58: token_b58.to_string(),
        },
    )
    .await
}

#[tokio::test]
#[ignore = "live: spawns covenantd + drives token rotation through real IPC"]
async fn live_covenantd_rotate_operator_token_replaces_authentication() {
    let home = tempfile::tempdir().expect("tempdir");
    let port = pick_free_port();

    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(exe)
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

    let bootstrap_token = read_operator_token(home.path()).await;

    // Phase 1: authenticate with the bootstrap token, rotate, capture new token.
    let new_token = {
        let mut stream = UnixStream::connect(&sock).await.expect("connect");
        match authenticate(&mut stream, &bootstrap_token).await {
            Response::Authenticated { .. } => {}
            other => panic!("bootstrap auth failed: {other:?}"),
        }
        match req(&mut stream, Request::RotateOperatorToken).await {
            Response::OperatorTokenRotated { token_b58 } => token_b58,
            other => panic!("rotate failed: {other:?}"),
        }
    };
    assert_ne!(
        new_token, bootstrap_token,
        "rotation must produce a fresh token"
    );

    // The on-disk file is the daemon's contract with the next process
    // start; it must already hold the new b58 by the time the response
    // returns (Plan-gate A1: write-disk happens between register-new
    // and revoke-old).
    let token_path = home.path().join("peers").join("operator.token");
    let on_disk = std::fs::read_to_string(&token_path).expect("read operator.token");
    assert_eq!(on_disk.trim(), new_token, "disk must hold the new token");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&token_path)
            .expect("stat operator.token")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "operator.token must remain mode 0600");
    }

    // Phase 2: a fresh connection with the OLD token must be rejected.
    {
        let mut stream = UnixStream::connect(&sock).await.expect("connect");
        match authenticate(&mut stream, &bootstrap_token).await {
            Response::AuthenticationFailed { reason } => {
                assert!(
                    reason.contains("unknown") || reason.contains("revoked"),
                    "rejection must name token state; got {reason:?}"
                );
            }
            other => panic!("expected AuthenticationFailed, got {other:?}"),
        }
    }

    // Phase 3: a fresh connection with the NEW token authenticates.
    {
        let mut stream = UnixStream::connect(&sock).await.expect("connect");
        match authenticate(&mut stream, &new_token).await {
            Response::Authenticated { .. } => {}
            other => panic!("new-token auth failed: {other:?}"),
        }
        // And the connection works for a normal request.
        match req(&mut stream, Request::Ping).await {
            Response::Pong => {}
            other => panic!("post-rotate ping failed: {other:?}"),
        }
    }

    let _ = child.kill().await;
}
