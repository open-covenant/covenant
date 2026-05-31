//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::RecentAudit` over the raw IPC socket, asserting the daemon answers
//! `Response::AuditEvents` and that a real audit row surfaces on the operator's
//! own feed after one is appended.
//!
//! The verb is covered today over the CLI (`live_cli_audit_recent.rs`,
//! `live_cli_audit_recent_json.rs`, `live_cli_audit_recent_since_ms.rs`) and
//! HTTP (`live_http_audit_recent_since_ms.rs`) but never over the raw Unix
//! socket they are built on. This pins that wire contract — the v1
//! `Response::AuditEvents` variant (covenant-ipc/src/lib.rs) and the
//! `AuditEvent`/`AuditKind` shape the operator reads.
//!
//! `prefer_stream` is left `None`, so the daemon must answer with the terminal
//! `Response::AuditEvents` and not the v2 streaming fork. A fresh operator feed
//! is empty; granting one capability appends exactly one
//! `AuditKind::CapabilityGranted` row, filtered to the operator's own pubkey.
//!
//! Hermetic — the audit chain is local and deterministic. `#[ignore]`'d. Run
//! with `cargo test -p covenantd --test live_ipc_recent_audit -- --ignored live_`.

use covenant_audit::AuditKind;
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
#[ignore = "live: spawns covenantd + drives Request::RecentAudit over the socket as an event is appended"]
async fn live_ipc_recent_audit_returns_seeded_event() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    // Empty baseline. A non-AuditEvents reply also catches the v2 streaming
    // fork wrongly firing for a prefer_stream: None request.
    match req(
        &mut stream,
        Request::RecentAudit {
            limit: 100,
            since_ms: None,
            prefer_stream: None,
        },
    )
    .await
    {
        Response::AuditEvents { events } => assert!(
            events.is_empty(),
            "a fresh operator audit feed must be empty before seeding: {events:?}"
        ),
        other => panic!("expected Response::AuditEvents, got {other:?}"),
    }

    match req(
        &mut stream,
        Request::GrantCapability {
            action: "tool.call.echo".into(),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { .. } => {}
        other => panic!("expected Response::CapabilityGranted, got {other:?}"),
    }

    match req(
        &mut stream,
        Request::RecentAudit {
            limit: 100,
            since_ms: None,
            prefer_stream: None,
        },
    )
    .await
    {
        Response::AuditEvents { events } => {
            assert_eq!(
                events.len(),
                1,
                "a single grant must surface exactly one row on the operator's own feed: {events:?}"
            );
            match &events[0].kind {
                AuditKind::CapabilityGranted { action, .. } => assert_eq!(
                    action, "tool.call.echo",
                    "the seeded row must carry the granted action"
                ),
                other => panic!("expected AuditKind::CapabilityGranted, got {other:?}"),
            }
        }
        other => panic!("expected Response::AuditEvents, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
