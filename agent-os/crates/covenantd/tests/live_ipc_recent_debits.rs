//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::RecentDebits` over the raw IPC socket, asserting the daemon answers
//! `Response::Debits` with an empty list on a fresh daemon.
//!
//! The verb is covered today over HTTP (`live_http_budget_debits.rs`, GET
//! `/budget/debits`) but never over the raw Unix socket the CLI and HTTP gateway
//! are built on. This pins that wire contract — the operator-facing budget-burn
//! aggregate (`Response::Debits { debits }`, covenant-ipc/src/lib.rs; handler
//! `recent_debits`, covenantd/src/lib.rs:2629).
//!
//! Hermetic — a fresh tempdir router stages no budgeted agents, so the burn
//! aggregate is empty and the response is offline and deterministic. The seeded
//! non-empty path needs a prebuilt research binary and lives in the HTTP test.
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_recent_debits -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
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

async fn authenticated_stream(home: &Path) -> UnixStream {
    let mut stream = UnixStream::connect(home.join("sock"))
        .await
        .expect("connect socket");
    let token = read_operator_token(home).await;
    match req(&mut stream, Request::Authenticate { token_b58: token }).await {
        Response::Authenticated { .. } => {}
        other => panic!("authenticate failed: {other:?}"),
    }
    stream
}

#[tokio::test]
#[ignore = "live: spawns covenantd + queries Request::RecentDebits over the socket"]
async fn live_ipc_recent_debits_returns_empty_on_fresh_daemon() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    match req(&mut stream, Request::RecentDebits { limit: 25 }).await {
        Response::Debits { debits } => {
            assert!(
                debits.is_empty(),
                "a fresh tempdir router stages no budgeted agents, so the burn aggregate must be empty: {debits:?}"
            );
        }
        other => panic!("expected Response::Debits, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
