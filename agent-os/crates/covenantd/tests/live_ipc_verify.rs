//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::Verify` over the raw IPC socket, asserting a fresh daemon answers
//! a clean `Response::VerifyReport` — the four named integrity checks all
//! passing, no drift, and zero orphans.
//!
//! The verb is covered today over the CLI (`live_cli_verify_json.rs`) and HTTP
//! (`live_http_verify.rs`) but never over the raw Unix socket the CLI is built
//! on. This pins that wire contract — the integrity report an operator reads to
//! confirm the audit ↔ memory ↔ capability ↔ receipt invariants hold.
//!
//! Hermetic — a clean store has nothing to reconcile, so the report is offline
//! and deterministic. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_verify -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;

/// The four integrity checks every VerifyReport carries, pinned identically in
/// `live_http_verify.rs`. A clean store must pass all four.
const CHECK_NAMES: [&str; 4] = [
    "memory ↔ audit",
    "memory parent references",
    "capability ↔ audit",
    "memory ↔ receipts",
];

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
#[ignore = "live: spawns covenantd + queries Request::Verify over the socket"]
async fn live_ipc_verify_reports_clean_state() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;

    let mut stream = authenticated_stream(home.path()).await;
    match req(&mut stream, Request::Verify { window: 100 }).await {
        Response::VerifyReport {
            window,
            checks,
            drift,
            orphans_total,
        } => {
            assert_eq!(window, 100, "the report must echo the requested window");
            assert!(
                drift.is_empty(),
                "a clean store must report no drift: {drift:?}"
            );
            assert_eq!(orphans_total, 0, "a clean store must report no orphans");
            assert_eq!(
                checks.len(),
                CHECK_NAMES.len(),
                "the report must carry exactly the four named checks: {:?}",
                checks.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
            );
            for name in CHECK_NAMES {
                let check = checks.iter().find(|c| c.name == name).unwrap_or_else(|| {
                    panic!(
                        "verify report is missing the {name:?} check: {:?}",
                        checks.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
                    )
                });
                assert!(
                    check.passed,
                    "the {name:?} check must pass on a clean store: {}",
                    check.message
                );
            }
        }
        other => panic!("expected Response::VerifyReport, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
