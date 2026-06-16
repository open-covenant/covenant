//! Live integration test: the metered-capability usage budget (`max_uses`) is
//! enforced through the real daemon and is **not** refilled by a restart.
//!
//! A grant carrying `max_uses = 1` authorizes exactly one tool call; the second
//! call is refused with a distinct `CapabilityBudgetExhausted` audit row (not a
//! missing-capability denial). The spent count is durable in `uses.jsonl`, so
//! after the daemon is killed and respawned against the same HOME the refusal
//! still holds — a budget that silently reset on restart would re-authorize the
//! call and fail this test.
//!
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_capability_usage_budget -- --ignored live_`.

use covenant_audit::{AuditEvent, AuditKind};
use covenant_ipc::{read_frame, write_frame, Request, Response};
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

async fn recent_audit(stream: &mut UnixStream) -> Vec<AuditEvent> {
    match req(
        stream,
        Request::RecentAudit {
            limit: 100,
            since_ms: None,
            prefer_stream: None,
        },
    )
    .await
    {
        Response::AuditEvents { events } => events,
        other => panic!("expected AuditEvents, got {other:?}"),
    }
}

/// Most recent `CapabilityBudgetExhausted` row, as
/// `(action, max_uses, used, signature_b58)`.
fn latest_budget_exhausted(events: &[AuditEvent]) -> (String, u64, u64, String) {
    events
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            AuditKind::CapabilityBudgetExhausted {
                action,
                max_uses,
                used,
                signature_b58,
            } => Some((action.clone(), *max_uses, *used, signature_b58.clone())),
            _ => None,
        })
        .expect("a CapabilityBudgetExhausted audit row must record the spent grant")
}

#[tokio::test]
#[ignore = "live: spawns covenantd twice + verifies a max_uses budget is not refilled by restart"]
async fn live_covenantd_capability_usage_budget_not_refilled_by_restart() {
    let home = tempfile::tempdir().expect("tempdir");
    let sock = home.path().join("sock");

    // ── Phase 1: grant max_uses = 1, spend the one unit, then confirm the
    //     second call is refused with a CapabilityBudgetExhausted row.
    let spent_signature_b58;
    {
        let mut child = spawn_daemon(home.path(), pick_free_port());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon #1 never created its socket at {}", sock.display());
        }
        let mut stream = UnixStream::connect(&sock).await.expect("connect #1");
        authenticate(&mut stream, home.path()).await;

        match req(
            &mut stream,
            Request::GrantCapability {
                action: "tool.call.echo".into(),
                scope: Some(serde_json::json!({ "version": 1, "tool": "echo", "max_uses": 1 })),
                expires_at: None,
            },
        )
        .await
        {
            Response::CapabilityGranted { .. } => {}
            other => panic!("grant tool.call.echo failed: {other:?}"),
        }

        match call_echo(&mut stream, "within budget").await {
            Response::ToolResult { is_error, .. } => {
                assert!(!is_error, "the first call is within budget and must run")
            }
            other => panic!("expected a tool result within budget, got {other:?}"),
        }

        match call_echo(&mut stream, "over budget").await {
            Response::Error { .. } => {}
            other => panic!("a spent budget must refuse the call, got {other:?}"),
        }

        let events = recent_audit(&mut stream).await;
        let (action, max_uses, used, signature_b58) = latest_budget_exhausted(&events);
        assert_eq!(action, "tool.call.echo");
        assert_eq!(max_uses, 1, "max_uses");
        assert_eq!(used, 1, "used must equal max_uses at refusal");
        assert!(
            !signature_b58.is_empty(),
            "the spent grant's signature is the join key"
        );
        spent_signature_b58 = signature_b58;

        drop(stream);
        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    // SIGKILL leaves the socket file behind; remove it so the wait_for_sock
    // after daemon #2 is a real readiness signal, not the stale file from #1.
    let _ = std::fs::remove_file(&sock);

    // The spent unit must be on disk in uses.jsonl — that file, never purged, is
    // what carries the budget across the restart below. The grant itself must
    // also persist so the post-restart denial is budget exhaustion, not a lost
    // grant.
    let uses_path = home.path().join("capabilities").join("uses.jsonl");
    let uses = std::fs::read_to_string(&uses_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", uses_path.display()));
    assert!(
        !uses.trim().is_empty(),
        "uses.jsonl must record the spent unit before restart: {uses:?}"
    );
    let granted_path = home.path().join("capabilities").join("granted.jsonl");
    assert!(
        granted_path.exists(),
        "expected capabilities log at {}",
        granted_path.display()
    );

    // ── Phase 2: respawn against the same HOME. The budget must stay spent, so
    //     the same call is still refused and the grant is still authorized.
    {
        let mut child = spawn_daemon(home.path(), pick_free_port());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon #2 never created its socket at {}", sock.display());
        }
        let mut stream = UnixStream::connect(&sock).await.expect("connect #2");
        authenticate(&mut stream, home.path()).await;

        match call_echo(&mut stream, "after restart").await {
            Response::Error { .. } => {}
            other => panic!("a restart must not refill the budget, got {other:?}"),
        }

        let events = recent_audit(&mut stream).await;
        let (action, max_uses, used, signature_b58) = latest_budget_exhausted(&events);
        assert_eq!(action, "tool.call.echo");
        assert_eq!(max_uses, 1, "max_uses");
        assert_eq!(used, 1, "used must equal max_uses at refusal after restart");
        assert_eq!(
            signature_b58, spent_signature_b58,
            "the post-restart refusal must name the same spent grant"
        );

        // The grant still matches at the authorization layer, so the denial is
        // budget exhaustion — not a grant that failed to replay. The latest
        // tool:echo capability check passes with no missing action.
        let (passed, missing) = events
            .iter()
            .rev()
            .find_map(|e| match &e.kind {
                AuditKind::CapabilityCheck {
                    agent_id,
                    passed,
                    missing_actions,
                    ..
                } if agent_id == "tool:echo" => Some((*passed, missing_actions.clone())),
                _ => None,
            })
            .expect("a tool:echo capability check row must be present after restart");
        assert!(passed, "the grant still authorizes after restart");
        assert!(
            missing.is_empty(),
            "a spent grant must not appear as a missing action"
        );

        drop(stream);
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}
