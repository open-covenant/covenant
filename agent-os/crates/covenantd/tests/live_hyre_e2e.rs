//! Daemon-driven regression proving the old Hyre and x402 environment opt-ins
//! cannot advertise or execute paid tools while the legacy signer path is
//! parked.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::time::sleep;

async fn wait_for_sock(p: &std::path::Path) -> bool {
    for _ in 0..100 {
        if p.exists() {
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
            let t = s.trim();
            if !t.is_empty() {
                return t.to_string();
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

async fn operator(sock: &std::path::Path, token: &str) -> UnixStream {
    let mut s = UnixStream::connect(sock).await.expect("connect");
    match req(
        &mut s,
        Request::Authenticate {
            token_b58: token.to_string(),
        },
    )
    .await
    {
        Response::Authenticated { .. } => s,
        other => panic!("operator auth failed: {other:?}"),
    }
}

#[tokio::test]
async fn live_covenantd_ignores_legacy_hyre_and_x402_opt_ins() {
    let home = tempfile::tempdir().expect("tempdir");
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(exe)
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .env("COVENANT_HYRE_ENABLED", "1")
        .env("COVENANT_X402_ENABLED", "1")
        .env("COVENANT_X402_SIGNER_BINARY", "/nonexistent-x402-signer")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");

    let sock = home.path().join("sock");
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon never created its socket");
    }
    let token = read_operator_token(home.path()).await;

    let mut stream = operator(&sock, &token).await;
    match req(&mut stream, Request::ListTools).await {
        Response::ToolList { tools } => {
            let names: Vec<_> = tools.iter().map(|tool| tool.name.as_str()).collect();
            assert!(
                names.iter().all(|name| !name.starts_with("hyre.")),
                "parked Hyre tools must not be advertised: {names:?}"
            );
        }
        other => panic!("ListTools failed: {other:?}"),
    }

    let _ = child.kill().await;
}
