//! Live integration test: spawns the real `covenantd` binary against a
//! tempdir `COVENANT_HOME`, drives the full IPC loop (`Ping` → `Pong`,
//! `SubmitIntent` → echo fallback because no agent is registered).
//!
//! Hermetic: tempdir-isolated home, ephemeral TCP port for the HTTP
//! gateway. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_daemon -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    // Bind to 0 to get an OS-assigned port, then drop. There's a race
    // window before covenantd grabs it; for an opt-in live test that's
    // acceptable. Phase 5+ tests should use a more robust scheme.
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

async fn authenticate(stream: &mut UnixStream, home: &std::path::Path) {
    let token_b58 = read_operator_token(home).await;
    write_frame(stream, &Request::Authenticate { token_b58 })
        .await
        .unwrap();
    match read_frame::<_, Response>(stream).await.unwrap() {
        Response::Authenticated { .. } => {}
        other => panic!("authenticate failed: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd as a real subprocess"]
async fn live_covenantd_ping_intent_echo_loop() {
    let home = tempfile::tempdir().expect("tempdir");
    let port = pick_free_port();
    let exe = env!("CARGO_BIN_EXE_covenantd");

    let mut child = Command::new(exe)
        .env("COVENANT_HOME", home.path())
        .env("COVENANT_HTTP_PORT", port.to_string())
        // Wipe HOME so the daemon doesn't inherit operator config.
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");

    let sock = home.path().join("sock");
    let appeared = wait_for_sock(&sock).await;
    if !appeared {
        let _ = child.kill().await;
        panic!("daemon never created its socket at {}", sock.display());
    }

    let mut stream = UnixStream::connect(&sock).await.expect("connect");
    authenticate(&mut stream, home.path()).await;

    write_frame(&mut stream, &Request::Ping).await.unwrap();
    let pong: Response = read_frame(&mut stream).await.unwrap();
    assert_eq!(pong, Response::Pong, "expected Pong, got {pong:?}");

    write_frame(
        &mut stream,
        &Request::SubmitIntent {
            text: "what is the meaning of life".into(),
            prefer_stream: None,
        },
    )
    .await
    .unwrap();
    let r: Response = read_frame(&mut stream).await.unwrap();
    match r {
        Response::IntentResult { text, status, .. } => {
            // No agent is registered in the tempdir, so dispatch falls
            // back to the echo path. That's the hermetic guarantee.
            assert_eq!(status, "ok");
            assert!(
                text.contains("no agent matched"),
                "expected echo fallback, got {text:?}"
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
}
