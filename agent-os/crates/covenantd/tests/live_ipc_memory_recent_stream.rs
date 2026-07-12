//! Live IPC coverage for the ADR 0010 v2 streaming path on
//! `Request::RecentMemory` — the IPC-socket transport sibling of
//! `live_http_memory_sse.rs`.
//!
//! `Request::RecentMemory { prefer_stream: Some(true) }` is routed to
//! `Server::stream_recent_memory` (lib.rs:5511) by the dispatch fork at
//! lib.rs:2106. That orchestrator calls `self.recent_memory(...)` which
//! gates on `memory.read` via `check_capabilities_any_of` (lib.rs:5417):
//!
//! - On `Response::Memories { records }` it registers a stream_tracker
//!   entry and drives `stream_dispatch::emit_memory_stream`, writing a
//!   `StreamBegin { response_kind: "memories" }` / `StreamChunk*` /
//!   `StreamEnd` sequence directly to the socket writer (lib.rs:5528).
//! - On any other variant — the `Response::Error` naming `memory.read`
//!   the gate returns when the grant is absent — the orchestrator's
//!   `other => return write_frame(writer, &other)` arm (lib.rs:5525)
//!   writes the failure as a v1-shape TERMINAL `Response` frame and
//!   never opens a stream.
//!
//! Two `#[ignore]`'d tests drive the raw request over the Unix socket as
//! the authenticated operator, controlling only whether `memory.read` is
//! granted:
//!
//! 1. WITH a self-grant, `read_response_or_stream` yields
//!    `ResponseOrStream::Stream(CollectedStream { response_kind:
//!    "memories", .. })` — NOT `Terminal` — proving the daemon honored
//!    `prefer_stream: Some(true)` and emitted the begin/end sequence over
//!    the socket.
//! 2. WITHOUT a grant, `read_response_or_stream` yields
//!    `ResponseOrStream::Terminal(Response::Error)` whose message names
//!    `memory.read`, proving the capability gate binds the streaming
//!    path: a failure never opens a stream that could leak records. A
//!    regression that checked scope only at stream_begin — the exact
//!    hazard ADR 0010 calls out — would flip this from a terminal error
//!    to a `StreamBegin` and fail assertion.
//!
//! Hermetic — no network, no Solana, no signer. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_memory_recent_stream -- --ignored live_`.

use covenant_ipc::{
    read_frame, read_response_or_stream, write_frame, Request, Response, ResponseOrStream,
};
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
    let mut cmd = Command::new(exe);
    cmd.env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home);
    let child = cmd
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

async fn authenticated_stream(home: &Path) -> UnixStream {
    let mut stream = UnixStream::connect(home.join("sock"))
        .await
        .expect("connect socket");
    let token = read_operator_token(home).await;
    write_frame(&mut stream, &Request::Authenticate { token_b58: token })
        .await
        .expect("write_frame auth");
    match read_frame::<_, Response>(&mut stream).await {
        Ok(Response::Authenticated { .. }) => stream,
        other => panic!("authenticate failed: {other:?}"),
    }
}

/// Self-grant `memory.read` as the authenticated operator and assert the
/// grant landed. Returns early on any non-`CapabilityGranted` response so
/// the streaming assertion that follows can't be masked by a silent grant
/// failure.
async fn grant_memory_read(stream: &mut UnixStream) {
    write_frame(
        stream,
        &Request::GrantCapability {
            action: "memory.read".into(),
            scope: None,
            expires_at: None,
        },
    )
    .await
    .expect("write_frame grant");
    match read_frame::<_, Response>(stream).await {
        Ok(Response::CapabilityGranted { .. }) => {}
        other => panic!("memory.read self-grant failed: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + drives Request::RecentMemory{prefer_stream:Some(true)} over the socket, asserting the daemon honors streaming when memory.read is granted"]
async fn live_ipc_memory_recent_stream_returns_stream_when_granted() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;

    let mut stream = authenticated_stream(home.path()).await;
    grant_memory_read(&mut stream).await;

    write_frame(
        &mut stream,
        &Request::RecentMemory {
            tier: None,
            limit: 8,
            prefer_stream: Some(true),
        },
    )
    .await
    .expect("write_frame recent_memory");

    // Load-bearing distinction from the Terminal case: a granted request
    // with prefer_stream: Some(true) must come back as a STREAM, not a
    // terminal Response::Memories frame. A regression that ignored the
    // preference and always returned the v1 terminal shape fails here.
    let reply = read_response_or_stream(&mut stream)
        .await
        .expect("read streaming reply");
    match reply {
        ResponseOrStream::Stream(collected) => {
            assert_eq!(
                collected.response_kind, "memories",
                "StreamBegin response_kind must announce the memories stream: {:?}",
                collected,
            );
            // Empty memory store on a fresh daemon: begin + end, no chunks.
            // Asserting emptiness catches a regression that fabricated or
            // duplicated a phantom record on the streaming path.
            assert!(
                collected.chunks.is_empty(),
                "fresh daemon must stream zero memory records, got chunks: {:?}",
                collected.chunks,
            );
        }
        other => panic!(
            "granted prefer_stream:Some(true) must yield a Stream, got Terminal {:?} — the daemon ignored the streaming preference",
            other
        ),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts Request::RecentMemory{prefer_stream:Some(true)} with NO memory.read grant renders as a TERMINAL Response::Error, never opening a stream"]
async fn live_ipc_memory_recent_stream_renders_capability_failure_as_terminal_error() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;

    let mut stream = authenticated_stream(home.path()).await;
    // Deliberately NO memory.read grant.

    write_frame(
        &mut stream,
        &Request::RecentMemory {
            tier: None,
            limit: 8,
            prefer_stream: Some(true),
        },
    )
    .await
    .expect("write_frame recent_memory");

    let reply = read_response_or_stream(&mut stream)
        .await
        .expect("read streaming reply");
    match reply {
        ResponseOrStream::Terminal(Response::Error { message }) => {
            assert!(
                message.contains("memory.read"),
                "capability failure must name the missing memory.read grant so the operator can act; got: {message:?}",
            );
        }
        other => panic!(
            "un-granted prefer_stream:Some(true) must yield a Terminal Response::Error, never a stream — got {:?} (a Stream here means the capability gate was bypassed on the streaming path and records could leak)",
            other
        ),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
