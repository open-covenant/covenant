//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::RetryA2AStale` over the raw IPC socket — an enabled scan is gated
//! on `a2a.repair.requeue`, then returns an empty report over a clean store.
//!
//! The verb is covered today over the CLI (`live_cli_a2a_retry_stale_json.rs`)
//! and HTTP (`live_http_a2a_retry_stale.rs`) but never over the raw Unix socket
//! both are built on. This pins the `Response::A2AAutoRetried { report }` wire
//! shape (covenant-ipc/src/lib.rs:891): the daemon echoes the requested policy
//! and, with no in-flight leases, considers and requeues nothing.
//!
//! Hermetic — a clean store has nothing to scan, so the report is offline and
//! deterministic. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_retry_a2a_stale -- --ignored live_`.

use covenant_a2a::A2AAutoRetryPolicy;
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

/// An enabled policy with a zero lease-age floor so the scan is gated and runs
/// rather than short-circuiting on a disabled default.
fn enabled_policy() -> A2AAutoRetryPolicy {
    A2AAutoRetryPolicy {
        enabled: true,
        min_lease_age_ms: 0,
        max_attempts: 2,
        max_requeues: 1,
        scan_limit: 5,
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + queries Request::RetryA2AStale over the socket"]
async fn live_ipc_retry_a2a_stale_reports_empty_scan() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    // Step A: an enabled scan without the capability is rejected by name. The
    // operator is authenticated, so this exercises the capability gate, not the
    // operator-identity guard.
    match req(
        &mut stream,
        Request::RetryA2AStale {
            policy: enabled_policy(),
        },
    )
    .await
    {
        Response::Error { message } => assert!(
            message.contains("a2a.repair.requeue"),
            "ungranted retry must name the missing capability: {message}"
        ),
        other => panic!("expected Response::Error before grant, got {other:?}"),
    }

    // Step B: grant a2a.repair.requeue to the operator.
    match req(
        &mut stream,
        Request::GrantCapability {
            action: "a2a.repair.requeue".into(),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { .. } => {}
        other => panic!("expected Response::CapabilityGranted, got {other:?}"),
    }

    // Step C: the granted scan over a clean store echoes the policy and finds
    // nothing to consider.
    match req(
        &mut stream,
        Request::RetryA2AStale {
            policy: enabled_policy(),
        },
    )
    .await
    {
        Response::A2AAutoRetried { report } => {
            assert!(
                report.policy.enabled,
                "the report must echo the enabled policy"
            );
            assert_eq!(report.policy.min_lease_age_ms, 0);
            assert_eq!(report.policy.max_attempts, 2);
            assert_eq!(report.policy.max_requeues, 1);
            assert_eq!(report.policy.scan_limit, 5);
            assert_eq!(
                report.considered, 0,
                "a clean store has no in-flight leases to consider"
            );
            assert!(
                report.requeued.is_empty(),
                "an empty scan requeues nothing: {:?}",
                report.requeued
            );
            assert!(
                report.skipped.is_empty(),
                "an empty scan skips nothing: {:?}",
                report.skipped
            );
        }
        other => panic!("expected Response::A2AAutoRetried, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
