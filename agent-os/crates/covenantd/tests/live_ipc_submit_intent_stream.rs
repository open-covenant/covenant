//! Live IPC coverage for the ADR 0010 v2 streaming path on
//! `Request::SubmitIntent` — the intent sibling of
//! `live_ipc_memory_recent_stream.rs`.
//!
//! `Request::SubmitIntent { prefer_stream: Some(true) }` is routed to
//! `Server::stream_submit_intent` (lib.rs:5776) by the dispatch fork at
//! lib.rs:2143. That orchestrator calls `self.dispatch_intent(...)` which
//! owns the capability checks: `dispatch_intent_run` gates the very first
//! step on `memory.write` via `check_capabilities` (lib.rs:4563) and
//! returns `Response::Error` naming `memory.write` when the grant is
//! absent (lib.rs:4567).
//!
//! - On `Response::IntentResult { .. }` the orchestrator registers a
//!   stream_tracker entry and drives `stream_dispatch::emit_intent_stream`,
//!   writing a `StreamBegin { response_kind: "intent_result" }` / one
//!   `StreamChunk` / `StreamEnd` sequence directly to the socket writer
//!   (lib.rs:5811).
//! - On any other variant — the `Response::Error` the `memory.write` gate
//!   returns when the grant is absent — the orchestrator's
//!   `other => return write_frame(writer, &other)` arm (lib.rs:5808)
//!   writes the failure as a v1-shape TERMINAL `Response` frame and never
//!   opens a stream.
//!
//! Two `#[ignore]`'d tests drive the raw request over the Unix socket as
//! the authenticated operator, controlling only whether `memory.write` is
//! granted. Both run against an empty daemon (no agent card registered),
//! so `dispatch_intent_run` takes the phase-0 echo else-branch (lib.rs:4771)
//! and returns `IntentResult { status: "ok", .. }` once the gate passes —
//! no real runner, network, or signer is involved.
//!
//! 1. WITH a self-grant, `read_response_or_stream` yields
//!    `ResponseOrStream::Stream(CollectedStream { response_kind:
//!    "intent_result", chunks.len() == 1, .. })` — NOT `Terminal` —
//!    proving the daemon honored `prefer_stream: Some(true)` and emitted
//!    the begin/chunk/end sequence over the socket.
//! 2. WITHOUT a grant, `read_response_or_stream` yields
//!    `ResponseOrStream::Terminal(Response::Error)` whose message names
//!    `memory.write`, proving the capability gate binds the streaming
//!    path: a failure never opens a stream that could leak the intent
//!    dispatch. A regression that checked scope only at stream_begin —
//!    the exact hazard ADR 0010 calls out — would flip this from a
//!    terminal error to a `StreamBegin` and fail this assertion.
//!
//! Hermetic — no network, no Solana, no signer. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_submit_intent_stream -- --ignored live_`.

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

/// Self-grant `memory.write` as the authenticated operator and assert the
/// grant landed. Returns early on any non-`CapabilityGranted` response so
/// the streaming assertion that follows can't be masked by a silent grant
/// failure.
async fn grant_memory_write(stream: &mut UnixStream) {
    write_frame(
        stream,
        &Request::GrantCapability {
            action: "memory.write".into(),
            scope: None,
            expires_at: None,
        },
    )
    .await
    .expect("write_frame grant");
    match read_frame::<_, Response>(stream).await {
        Ok(Response::CapabilityGranted { .. }) => {}
        other => panic!("memory.write self-grant failed: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + drives Request::SubmitIntent{prefer_stream:Some(true)} over the socket, asserting the daemon honors streaming when memory.write is granted"]
async fn live_ipc_submit_intent_stream_returns_stream_when_granted() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;

    let mut stream = authenticated_stream(home.path()).await;
    grant_memory_write(&mut stream).await;

    write_frame(
        &mut stream,
        &Request::SubmitIntent {
            text: "phase-0 echo probe: no agent registered".into(),
            prefer_stream: Some(true),
        },
    )
    .await
    .expect("write_frame submit_intent");

    // Load-bearing distinction from the Terminal case: a granted request
    // with prefer_stream: Some(true) must come back as a STREAM, not a
    // terminal Response::IntentResult frame. A regression that ignored the
    // preference and always returned the v1 terminal shape fails here.
    let reply = read_response_or_stream(&mut stream)
        .await
        .expect("read streaming reply");
    match reply {
        ResponseOrStream::Stream(collected) => {
            assert_eq!(
                collected.response_kind, "intent_result",
                "StreamBegin response_kind must announce the intent_result stream: {:?}",
                collected,
            );
            // Empty daemon dispatches through the phase-0 echo path, which
            // packs exactly one AgentResult chunk. Zero chunks would mean the
            // orchestrator opened a stream with no payload; two would mean a
            // double-emit of the runtime_events fold the comment at
            // lib.rs:5757-5760 warns against.
            assert_eq!(
                collected.chunks.len(),
                1,
                "phase-0 echo must stream exactly one intent chunk, got: {:?}",
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
#[ignore = "live: spawns covenantd + asserts Request::SubmitIntent{prefer_stream:Some(true)} with NO memory.write grant renders as a TERMINAL Response::Error, never opening a stream"]
async fn live_ipc_submit_intent_stream_renders_capability_failure_as_terminal_error() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;

    let mut stream = authenticated_stream(home.path()).await;
    // Deliberately NO memory.write grant.

    write_frame(
        &mut stream,
        &Request::SubmitIntent {
            text: "un-granted phase-0 echo probe".into(),
            prefer_stream: Some(true),
        },
    )
    .await
    .expect("write_frame submit_intent");

    let reply = read_response_or_stream(&mut stream)
        .await
        .expect("read streaming reply");
    match reply {
        ResponseOrStream::Terminal(Response::Error { message }) => {
            assert!(
                message.contains("memory.write"),
                "capability failure must name the missing memory.write grant so the operator can act; got: {message:?}",
            );
        }
        other => panic!(
            "un-granted prefer_stream:Some(true) must yield a Terminal Response::Error, never a stream — got {:?} (a Stream here means the capability gate was bypassed on the streaming path and intent dispatch could leak)",
            other
        ),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
