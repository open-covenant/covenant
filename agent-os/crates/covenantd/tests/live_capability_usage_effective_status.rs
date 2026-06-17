//! Live integration test: the operator capability-usage query reports a
//! daemon-derived `effective` status (`live`/`exhausted`/`revoked`) that flips
//! through the real grant/consume/revoke path and survives a restart.
//!
//! The in-process unit tests construct grant state directly; this drives three
//! `tool.call.echo` grants — distinguished only by scope, so each gets its own
//! ed25519 signature — into distinct states through the real daemon:
//!
//!   * `exhausted`: a `max_uses = 1` grant whose unit is spent through a real
//!     tool call, after which the next call is refused — tying the reported
//!     `exhausted` to the enforcement decision, not a re-derivation;
//!   * `live`: an unbudgeted grant that is never spent or revoked;
//!   * `revoked`: a grant revoked through `Request::RevokeCapability`, which must
//!     still appear flagged in the snapshot rather than vanishing.
//!
//! The exhausted grant is created first and spent while it is the only echo
//! grant, so enforcement consumes that signature unambiguously. After a
//! kill/respawn against the same HOME the query reports the same three statuses,
//! showing they are derived from the durable granted/uses/revoked ledgers plus
//! the daemon clock and not an in-memory value a restart would reset.
//!
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_capability_usage_effective_status -- --ignored live_`.

use covenant_ipc::{
    read_frame, write_frame, CapabilityEffectiveStatus, CapabilityUsageEntry, Request, Response,
};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
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

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
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

async fn authenticate(stream: &mut UnixStream, home: &Path) {
    let token_b58 = read_operator_token(home).await;
    match req(stream, Request::Authenticate { token_b58 }).await {
        Response::Authenticated { .. } => {}
        other => panic!("authenticate failed: {other:?}"),
    }
}

fn spawn_daemon(home: &Path, port: u16) -> Child {
    let exe = env!("CARGO_BIN_EXE_covenantd");
    Command::new(exe)
        .env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd")
}

async fn call_echo(stream: &mut UnixStream, text: &str) -> Response {
    req(
        stream,
        Request::CallTool {
            name: "echo".into(),
            arguments: serde_json::json!({ "text": text }),
        },
    )
    .await
}

async fn grant_echo(stream: &mut UnixStream, scope: serde_json::Value) -> String {
    match req(
        stream,
        Request::GrantCapability {
            action: "tool.call.echo".into(),
            scope: Some(scope),
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted {
            signature_b58,
            action,
            ..
        } => {
            assert_eq!(action, "tool.call.echo");
            signature_b58
        }
        other => panic!("grant tool.call.echo failed: {other:?}"),
    }
}

async fn usage_snapshot(stream: &mut UnixStream) -> Vec<CapabilityUsageEntry> {
    match req(stream, Request::CapabilityUsage).await {
        Response::CapabilityUsage { grants } => grants,
        other => panic!("expected CapabilityUsage, got {other:?}"),
    }
}

/// The grant identified by `signature_b58` — the ed25519 join key — or a panic
/// if the snapshot does not carry it.
fn entry_for<'a>(grants: &'a [CapabilityUsageEntry], signature_b58: &str) -> &'a CapabilityUsageEntry {
    grants
        .iter()
        .find(|g| g.signature_b58 == signature_b58)
        .unwrap_or_else(|| panic!("grant {signature_b58} must appear in the usage snapshot"))
}

#[tokio::test]
#[ignore = "live: spawns covenantd twice + verifies the capability-usage effective status flips to exhausted/revoked through the real daemon and survives restart"]
async fn live_covenantd_capability_usage_effective_status_across_restart() {
    let home = tempfile::tempdir().expect("tempdir");
    let sock = home.path().join("sock");

    // Distinct scopes so the three grants for the same action get distinct
    // ed25519 signatures (canonical_message folds the JCS scope bytes).
    let exhausted_scope = serde_json::json!({ "version": 1, "tool": "echo", "max_uses": 1 });
    let live_scope = serde_json::json!({ "version": 1, "tool": "echo" });
    let revoked_scope = serde_json::json!({ "version": 1, "tool": "echo", "max_uses": 2 });

    let exhausted_sig;
    let live_sig;
    let revoked_sig;

    // ── Phase 1: drive three grants into exhausted / live / revoked and confirm
    //     the query reports the matching effective status for each.
    {
        let mut child = spawn_daemon(home.path(), pick_free_port());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon #1 never created its socket at {}", sock.display());
        }
        let mut stream = UnixStream::connect(&sock).await.expect("connect #1");
        authenticate(&mut stream, home.path()).await;

        // Exhausted: granted first so it is the only echo grant while its single
        // unit is spent — enforcement consumes this signature unambiguously.
        exhausted_sig = grant_echo(&mut stream, exhausted_scope).await;
        match call_echo(&mut stream, "spend the unit").await {
            Response::ToolResult { is_error, .. } => {
                assert!(!is_error, "the one unit is within budget and must run")
            }
            other => panic!("expected a tool result within budget, got {other:?}"),
        }
        // The spent grant is refused at enforcement — the same boundary the
        // `exhausted` status names. Asserted while it is still the only echo
        // grant, so the refusal cannot be attributed to any other grant.
        match call_echo(&mut stream, "over budget").await {
            Response::Error { .. } => {}
            other => panic!("a spent budget must refuse the next call, got {other:?}"),
        }

        // Live: unbudgeted, never spent or revoked.
        live_sig = grant_echo(&mut stream, live_scope).await;

        // Revoked: granted, then revoked through the operator path.
        revoked_sig = grant_echo(&mut stream, revoked_scope).await;
        match req(
            &mut stream,
            Request::RevokeCapability {
                signature_b58: revoked_sig.clone(),
            },
        )
        .await
        {
            Response::CapabilityRevoked { .. } => {}
            other => panic!("expected Response::CapabilityRevoked, got {other:?}"),
        }

        assert_effective_statuses(&usage_snapshot(&mut stream).await, &exhausted_sig, &live_sig, &revoked_sig);

        drop(stream);
        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    // SIGKILL leaves the socket file behind; remove it so wait_for_sock after
    // daemon #2 is a real readiness signal, not the stale file from #1.
    let _ = std::fs::remove_file(&sock);

    // ── Phase 2: respawn against the same HOME. The three statuses must be
    //     re-derived from the durable ledgers plus the daemon clock, so they are
    //     identical — an in-memory derivation would reset the exhausted count and
    //     report it `live` here.
    {
        let mut child = spawn_daemon(home.path(), pick_free_port());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon #2 never created its socket at {}", sock.display());
        }
        let mut stream = UnixStream::connect(&sock).await.expect("connect #2");
        authenticate(&mut stream, home.path()).await;

        assert_effective_statuses(&usage_snapshot(&mut stream).await, &exhausted_sig, &live_sig, &revoked_sig);

        drop(stream);
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

fn assert_effective_statuses(
    grants: &[CapabilityUsageEntry],
    exhausted_sig: &str,
    live_sig: &str,
    revoked_sig: &str,
) {
    let exhausted = entry_for(grants, exhausted_sig);
    assert_eq!(
        exhausted.effective,
        CapabilityEffectiveStatus::Exhausted,
        "a spent max_uses grant reports exhausted",
    );
    assert!(!exhausted.revoked, "the exhausted grant is not revoked");
    assert_eq!(
        exhausted.budget.expect("budgeted grant reports its budget").remaining,
        0,
        "the exhausted grant has no remaining budget",
    );

    let live = entry_for(grants, live_sig);
    assert_eq!(
        live.effective,
        CapabilityEffectiveStatus::Live,
        "an unbudgeted, unexpired, unrevoked grant reports live",
    );
    assert!(live.budget.is_none(), "the live grant is unbudgeted");

    let revoked = entry_for(grants, revoked_sig);
    assert_eq!(
        revoked.effective,
        CapabilityEffectiveStatus::Revoked,
        "a revoked grant reports revoked regardless of its budget",
    );
    assert!(
        revoked.revoked,
        "the revoked grant still appears flagged rather than vanishing",
    );
}
