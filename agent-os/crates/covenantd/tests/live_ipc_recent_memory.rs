//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::RecentMemory` over the raw IPC socket, asserting the daemon gates
//! on the `memory.read` capability and answers `Response::Memories` once granted.
//!
//! The verb is covered today over the CLI (`live_cli_memory_read_json.rs`) and
//! HTTP (`live_http_memory_sse.rs`) but never over the raw Unix socket they are
//! built on. This pins that wire contract — the `Response::Memories` variant
//! (covenant-ipc/src/lib.rs) and the `memory.read` gate in `recent_memory`
//! (covenantd/src/lib.rs) that an operator must clear before reading memory.
//!
//! `prefer_stream` is left `None`, so the daemon must answer the terminal
//! `Response::Memories`. An ungranted operator is rejected; after a `memory.read`
//! grant the same call returns an empty record list — no public verb writes
//! memory rows on a fresh tempdir, so the granted-but-empty shape is the
//! deterministic contract.
//!
//! Hermetic — the memory store is local and starts empty. `#[ignore]`'d. Run
//! with `cargo test -p covenantd --test live_ipc_recent_memory -- --ignored live_`.

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
#[ignore = "live: spawns covenantd + drives Request::RecentMemory over the socket across the memory.read gate"]
async fn live_ipc_recent_memory_gates_on_read_capability() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    // Ungranted: the operator must be rejected by the memory.read gate.
    match req(
        &mut stream,
        Request::RecentMemory {
            tier: None,
            limit: 50,
            prefer_stream: None,
        },
    )
    .await
    {
        Response::Error { .. } => {}
        other => panic!("expected Response::Error before the memory.read grant, got {other:?}"),
    }

    match req(
        &mut stream,
        Request::GrantCapability {
            action: "memory.read".into(),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { .. } => {}
        other => panic!("expected Response::CapabilityGranted, got {other:?}"),
    }

    // Granted: the same call now succeeds, and a fresh store has no rows.
    match req(
        &mut stream,
        Request::RecentMemory {
            tier: None,
            limit: 50,
            prefer_stream: None,
        },
    )
    .await
    {
        Response::Memories { records } => assert!(
            records.is_empty(),
            "a fresh daemon writes no memory rows, so the granted read must be empty: {records:?}"
        ),
        other => panic!("expected Response::Memories after the grant, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
