//! Live integration test: proves the daemon enforces the
//! `Authenticate` first-frame handshake on every IPC connection.
//!
//! - A connection that sends any non-`Authenticate` frame as its first
//!   message receives an `AuthenticationFailed` reply, then the
//!   connection is closed.
//! - A connection that sends `Authenticate` with an unknown token
//!   receives `AuthenticationFailed { reason }` and the connection is
//!   closed.
//! - A connection that sends the operator token from
//!   `$COVENANT_HOME/peers/operator.token` is accepted.
//!
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_peer_auth -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, IpcError, Request, Response};
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

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies the auth handshake gate"]
async fn live_covenantd_rejects_unauthenticated_connection() {
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

    // ── Case 1: first frame is *not* Authenticate. The daemon must
    //     reply with AuthenticationFailed and close the connection.

    {
        let mut stream = UnixStream::connect(&sock).await.expect("connect");
        write_frame(&mut stream, &Request::Ping).await.unwrap();
        match read_frame::<_, Response>(&mut stream).await.unwrap() {
            Response::AuthenticationFailed { reason } => {
                assert!(
                    reason.contains("first frame must be Authenticate"),
                    "unexpected reason: {reason}"
                );
            }
            other => panic!("expected AuthenticationFailed, got {other:?}"),
        }
        // The daemon closed the connection. The next read must produce EOF.
        let next = read_frame::<_, Response>(&mut stream).await;
        match next {
            Err(IpcError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
            other => panic!("expected EOF after auth failure, got {other:?}"),
        }
    }

    // ── Case 2: Authenticate with an unknown token. Same outcome —
    //     the registry doesn't resolve the token, so reject + close.

    {
        let mut stream = UnixStream::connect(&sock).await.expect("connect");
        let stray = covenant_peer_auth::PeerToken::generate().to_b58();
        write_frame(&mut stream, &Request::Authenticate { token_b58: stray })
            .await
            .unwrap();
        match read_frame::<_, Response>(&mut stream).await.unwrap() {
            Response::AuthenticationFailed { reason } => {
                assert!(
                    reason.contains("unknown") || reason.contains("revoked"),
                    "unexpected reason: {reason}"
                );
            }
            other => panic!("expected AuthenticationFailed, got {other:?}"),
        }
        let next = read_frame::<_, Response>(&mut stream).await;
        match next {
            Err(IpcError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
            other => panic!("expected EOF after auth failure, got {other:?}"),
        }
    }

    // ── Case 3: real operator token. The daemon authenticates and the
    //     subsequent Ping returns Pong on the same connection.

    {
        let token_b58 = read_operator_token(home.path()).await;
        let mut stream = UnixStream::connect(&sock).await.expect("connect");
        write_frame(&mut stream, &Request::Authenticate { token_b58 })
            .await
            .unwrap();
        match read_frame::<_, Response>(&mut stream).await.unwrap() {
            Response::Authenticated { display } => {
                assert_eq!(display, "user@local", "unexpected operator display");
            }
            other => panic!("expected Authenticated, got {other:?}"),
        }
        write_frame(&mut stream, &Request::Ping).await.unwrap();
        match read_frame::<_, Response>(&mut stream).await.unwrap() {
            Response::Pong => {}
            other => panic!("expected Pong post-auth, got {other:?}"),
        }
    }

    let _ = child.kill().await;
}
