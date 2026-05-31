//! Live integration test: spawns the real covenantd binary and drives
//! `Request::ProtocolInfo` over the raw IPC socket on a fresh, unauthenticated
//! connection, asserting the daemon answers `Response::ProtocolInfo` with
//! `info == covenant_ipc::protocol_info()` — the version-negotiation frame
//! every CLI and HTTP client reads before it authenticates.
//!
//! The pre-auth branch lives in the connection loop in
//! `agent-os/crates/covenantd/src/lib.rs`: `Request::ProtocolInfo` is answered
//! and `continue`d before `Authenticate`. `end_to_end.rs` exercises that branch
//! against an in-process `Server` (not the spawned binary); `live_cli_version.rs`
//! reaches the spawned binary only through the `covenant version` CLI + JSON. No
//! live test proved the shipped daemon returns the correct raw ProtocolInfo
//! frame to an un-authenticated peer.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_protocol_info -- --ignored live_`.

use covenant_ipc::{protocol_info, read_frame, write_frame, Request, Response};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().unwrap().port()
}

async fn wait_for_sock(path: &Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn read_operator_token(home: &Path) -> String {
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

async fn spawn_daemon(home: &Path) -> Child {
    let port = pick_free_port();
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let child = Command::new(exe)
        .env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");
    if !wait_for_sock(&home.join("sock")).await {
        panic!("daemon never created its socket");
    }
    child
}

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

#[tokio::test]
#[ignore = "live: spawns covenantd + queries Request::ProtocolInfo over the raw socket before Authenticate"]
async fn live_ipc_protocol_info_pre_auth_returns_protocol_info() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;

    let mut stream = UnixStream::connect(home.path().join("sock"))
        .await
        .expect("connect socket");

    // Probe before authenticating: the daemon must answer the version
    // handshake on a fresh, unauthenticated connection.
    match req(&mut stream, Request::ProtocolInfo).await {
        Response::ProtocolInfo { info } => assert_eq!(
            info,
            protocol_info(),
            "pre-auth ProtocolInfo must echo the canonical protocol_info() the codec negotiates against"
        ),
        other => panic!("expected Response::ProtocolInfo, got {other:?}"),
    }

    // The probe is repeatable on the same connection — the loop `continue`s
    // rather than spending the connection's single auth slot.
    match req(&mut stream, Request::ProtocolInfo).await {
        Response::ProtocolInfo { info } => assert_eq!(
            info,
            protocol_info(),
            "a second pre-auth ProtocolInfo must return the identical frame"
        ),
        other => panic!("expected a second Response::ProtocolInfo, got {other:?}"),
    }

    // After the probes the connection still authenticates — the handshake did
    // not corrupt the connection's auth state machine.
    let token = read_operator_token(home.path()).await;
    match req(&mut stream, Request::Authenticate { token_b58: token }).await {
        Response::Authenticated { .. } => {}
        other => {
            panic!("connection must still authenticate after ProtocolInfo probes: {other:?}")
        }
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
