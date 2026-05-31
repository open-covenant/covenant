//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::PurgeAudit` over the raw IPC socket — the ungranted denial then the
//! granted success.
//!
//! The verb is covered today over the CLI (`live_cli_audit_purge_json.rs`) and
//! HTTP (`live_http_audit_purge.rs`) but never over the raw Unix socket both are
//! built on. This pins that wire contract: the `audit.purge` capability gate
//! (`purge_audit`, covenantd/src/lib.rs:3196) — which the operator must hold
//! even as the trust root — and `Response::AuditPurged { purged }`
//! (covenant-ipc/src/lib.rs:857).
//!
//! Hermetic: the grant in step 2 itself appends a `capability_granted` audit row
//! strictly older than the future cutoff, so the granted purge deterministically
//! drops at least it. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_audit_purge -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
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
#[ignore = "live: spawns covenantd + drives Request::PurgeAudit over the socket"]
async fn live_ipc_audit_purge_round_trip() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    // A future cutoff so every already-written audit row is strictly older.
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let cutoff = now_ms.saturating_add(60_000);

    // Ungranted baseline: audit purge requires the audit.purge capability, which
    // even the operator must hold.
    match req(&mut stream, Request::PurgeAudit { before_ms: cutoff }).await {
        Response::Error { message } => assert!(
            message.contains("audit.purge"),
            "ungranted purge must be rejected by the capability gate: {message:?}"
        ),
        other => panic!("expected Response::Error before grant, got {other:?}"),
    }

    // Grant it unscoped (any cutoff allowed). The grant itself appends a
    // capability_granted audit row, older than the cutoff.
    match req(
        &mut stream,
        Request::GrantCapability {
            action: "audit.purge".into(),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { action, .. } => {
            assert_eq!(action, "audit.purge", "the grant must be for audit.purge")
        }
        other => panic!("grant failed: {other:?}"),
    }

    // Granted purge drops at least the seeded grant row.
    match req(&mut stream, Request::PurgeAudit { before_ms: cutoff }).await {
        Response::AuditPurged { purged } => assert!(
            purged > 0,
            "purge must drop at least the seeded grant audit row: got {purged}"
        ),
        other => panic!("expected Response::AuditPurged, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
