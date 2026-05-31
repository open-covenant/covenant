//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::PurgeCapabilities` over the raw IPC socket, asserting it drops
//! revoked-capability tombstones older than the cutoff.
//!
//! The verb is covered today over the CLI (`live_cli_capabilities_purge_json.rs`)
//! but never over the raw Unix socket it is built on. This pins that wire
//! contract: `Response::CapabilitiesPurged { purged }` (covenant-ipc/src/lib.rs:860)
//! reached through the `capabilities.purge` gate, and the `revoked_at < before_ms`
//! tombstone selectivity.
//!
//! Seeded over the socket via the public API: a throwaway grant is revoked to
//! create one tombstone, then purged. The empty baseline (no tombstone yet) and
//! the idempotent re-purge bracket the seeded count. Hermetic — no external
//! services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_purge_capabilities -- --ignored live_`.

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
#[ignore = "live: spawns covenantd + drives Request::PurgeCapabilities over the socket after seeding a revoked grant"]
async fn live_ipc_purge_capabilities_drops_revoked_tombstone() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    // A cutoff comfortably after now: any tombstone revoked during this test
    // falls below it.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("unix time")
        .as_millis() as u64;
    let before_ms = now_ms + 30_000;

    // Grant the purge authority unscoped so any cutoff is allowed.
    match req(
        &mut stream,
        Request::GrantCapability {
            action: "capabilities.purge".into(),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { .. } => {}
        other => panic!("grant capabilities.purge failed: {other:?}"),
    }

    // Empty baseline: no revoked tombstones exist yet.
    match req(&mut stream, Request::PurgeCapabilities { before_ms }).await {
        Response::CapabilitiesPurged { purged } => assert_eq!(
            purged, 0,
            "no revoked tombstones yet must purge zero: {purged}"
        ),
        other => panic!("expected Response::CapabilitiesPurged, got {other:?}"),
    }

    // Seed a throwaway grant, then revoke it to create one tombstone.
    let sig_b58 = match req(
        &mut stream,
        Request::GrantCapability {
            action: "tool.call.echo".into(),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { signature_b58, .. } => signature_b58,
        other => panic!("expected Response::CapabilityGranted, got {other:?}"),
    };
    match req(&mut stream, Request::RevokeCapability { signature_b58: sig_b58 }).await {
        Response::CapabilityRevoked { removed, .. } => {
            assert!(removed, "the throwaway grant must be removed by revoke")
        }
        other => panic!("expected Response::CapabilityRevoked, got {other:?}"),
    }

    // The one revoked tombstone, older than the cutoff, is dropped.
    match req(&mut stream, Request::PurgeCapabilities { before_ms }).await {
        Response::CapabilitiesPurged { purged } => assert_eq!(
            purged, 1,
            "the single revoked tombstone older than the cutoff must be dropped: {purged}"
        ),
        other => panic!("expected Response::CapabilitiesPurged, got {other:?}"),
    }

    // Idempotent re-purge over the same window drops nothing.
    match req(&mut stream, Request::PurgeCapabilities { before_ms }).await {
        Response::CapabilitiesPurged { purged } => assert_eq!(
            purged, 0,
            "a second purge over the same window must drop nothing: {purged}"
        ),
        other => panic!("expected Response::CapabilitiesPurged, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
