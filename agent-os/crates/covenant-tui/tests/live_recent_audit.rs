//! Live integration test for `covenant_tui::ipc::recent_audit`.
//!
//! `recent_audit` has no read-side capability gate; the daemon
//! filters rows server-side to `issuer.pubkey == peer.pubkey`. So
//! the test simply needs to (a) make the operator do something that
//! produces an audit row and (b) confirm `recent_audit` returns at
//! least one event.
//!
//! Pattern: granting any capability emits a `CapabilityGranted`
//! audit row issued by the operator. After that, `recent_audit`
//! must return at least one event whose issuer matches the
//! operator.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenant-tui --test live_recent_audit -- --ignored live_`.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use covenant_tui::ipc::{grant_capability, recent_audit, AuditFetchOutcome, GrantOutcome};
use tokio::process::Command;
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
}

fn covenantd_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/covenantd")
        .canonicalize()
        .expect("covenantd binary not built; run `cargo build -p covenantd` first")
}

async fn wait_for_sock(path: &std::path::Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn wait_for_operator_token(home: &std::path::Path) {
    let path = home.join("peers").join("operator.token");
    for _ in 0..50 {
        if let Ok(s) = std::fs::read_to_string(&path) {
            if !s.trim().is_empty() {
                return;
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("operator token never appeared at {}", path.display());
}

#[tokio::test]
#[ignore = "live: spawns covenantd + drives recent_audit after a grant"]
async fn live_recent_audit_returns_operator_audit_rows() {
    let home = tempfile::tempdir().expect("tempdir");

    let port = pick_free_port();
    let exe = covenantd_bin();
    let mut child = Command::new(&exe)
        .env("COVENANT_HOME", home.path())
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");

    let sock = home.path().join("sock");
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon never created its socket at {}", sock.display());
    }
    wait_for_operator_token(home.path()).await;

    // Produce at least one audit row by performing a grant. The
    // daemon records a CapabilityGranted audit event issued by the
    // operator.
    match grant_capability(home.path(), "memory.read", None, None)
        .await
        .expect("grant_capability: wire-level error")
    {
        GrantOutcome::Granted { .. } => {}
        GrantOutcome::Failed { message } => panic!("grant failed: {message}"),
    }

    let outcome = recent_audit(home.path(), 20)
        .await
        .expect("recent_audit: wire-level error");
    let events = match outcome {
        AuditFetchOutcome::Fetched { events } => events,
        AuditFetchOutcome::Failed { message } => {
            panic!("recent_audit failed: {message}")
        }
    };
    assert!(
        !events.is_empty(),
        "recent_audit must return at least one operator-issued event after a grant"
    );

    let _ = child.kill().await;
}
