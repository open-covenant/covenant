//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::VerifyAuditIntegrity` over the raw IPC socket, asserting the
//! operator gets a valid `Response::AuditIntegrity` report whose hash chain
//! advances as real audit events are appended.
//!
//! The verb is covered today over the CLI (`live_cli_audit_verify.rs`) and HTTP
//! (`live_http_audit_verify.rs`) but never over the raw Unix socket both are
//! built on. This pins that wire contract: the `Response::AuditIntegrity`
//! variant (covenant-ipc/src/lib.rs:854) wrapping an `AuditIntegrityReport`
//! (`events`, `anchors`, `valid`, `root_hash_hex`, `failures`;
//! covenant-audit/src/lib.rs:60), and the operator-identity gate in
//! `verify_audit_integrity` (covenantd/src/lib.rs:3318) that admits the
//! daemon's own operator and would otherwise answer `Response::Error`.
//!
//! Audit state is seeded through the public API — a capability grant appends
//! and anchors audit rows — so the second report must stay valid with
//! `events == anchors`, a strictly higher `events` count than the empty
//! baseline, and an integrity root that has advanced off the baseline hash. A
//! frozen chain or a dropped anchor cannot pass.
//!
//! Hermetic — no external services; the audit chain is local and deterministic.
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_audit_verify -- --ignored live_`.

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
#[ignore = "live: spawns covenantd + drives Request::VerifyAuditIntegrity over the socket as events are appended"]
async fn live_ipc_audit_verify_reports_valid_advancing_chain() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    // Baseline: the daemon's own operator must clear the identity gate and get a
    // valid report. A broken gate answers Response::Error; a wire-shape
    // regression in Response::AuditIntegrity / AuditIntegrityReport fails to
    // decode into this variant.
    let (baseline_events, baseline_root) =
        match req(&mut stream, Request::VerifyAuditIntegrity).await {
            Response::AuditIntegrity { report } => {
                assert!(report.valid, "a fresh daemon's chain must verify clean");
                assert_eq!(
                    report.events, report.anchors,
                    "every event must be anchored, so the counts match: {report:?}"
                );
                assert!(
                    report.failures.is_empty(),
                    "a clean chain reports no failures: {:?}",
                    report.failures
                );
                assert_eq!(
                    report.root_hash_hex.len(),
                    64,
                    "the integrity root is a 64-hex sha256 chain hash: {report:?}"
                );
                (report.events, report.root_hash_hex)
            }
            other => panic!("expected Response::AuditIntegrity, got {other:?}"),
        };

    // Seed audit events through the public API: a capability grant appends and
    // anchors a CapabilityGranted row.
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

    match req(&mut stream, Request::VerifyAuditIntegrity).await {
        Response::AuditIntegrity { report } => {
            assert!(
                report.valid,
                "the chain must stay valid after real events flow through it: {report:?}"
            );
            assert_eq!(
                report.events, report.anchors,
                "every newly appended event must be anchored: {report:?}"
            );
            assert!(
                report.failures.is_empty(),
                "a consistent chain reports no failures: {:?}",
                report.failures
            );
            assert!(
                report.events > baseline_events,
                "granting a capability must add anchored audit events ({baseline_events} -> {}): {report:?}",
                report.events
            );
            assert_ne!(
                report.root_hash_hex, baseline_root,
                "the integrity root must advance as events are appended, not stay frozen: {report:?}"
            );
        }
        other => panic!("expected Response::AuditIntegrity, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
