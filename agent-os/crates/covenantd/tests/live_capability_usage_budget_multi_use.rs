//! Live integration test: a `max_uses = 3` metered capability budget authorizes
//! exactly three tool calls through the real daemon and refuses the fourth.
//!
//! This pins that the GRANTED budget value is honored once per call — not a
//! hardcoded single use and not an off-by-one. The only other live budget test
//! (`live_capability_usage_budget.rs`) grants `max_uses = 1`, so it can only see
//! `0 -> deny-at-1`; it cannot tell honoring the granted budget from a hardcoded
//! limit of 1. Here three successive calls must all run and only the fourth is
//! refused with a distinct `CapabilityBudgetExhausted` row, and the durable
//! used/remaining accounting is read back over `Request::CapabilityUsage` — a
//! second read confirming that observing usage records no use.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_capability_usage_budget_multi_use -- --ignored live_`.

use covenant_audit::{AuditEvent, AuditKind};
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

/// The single `tool.call.echo` grant's usage row, read without recording a use.
async fn echo_usage_entry(stream: &mut UnixStream) -> CapabilityUsageEntry {
    match req(stream, Request::CapabilityUsage).await {
        Response::CapabilityUsage { grants } => grants
            .into_iter()
            .find(|e| e.action == "tool.call.echo")
            .expect("a tool.call.echo capability-usage entry must be present"),
        other => panic!("expected CapabilityUsage, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies a max_uses=3 budget authorizes exactly three calls"]
async fn live_covenantd_capability_usage_budget_multi_use() {
    let home = tempfile::tempdir().expect("tempdir");
    let sock = home.path().join("sock");

    let mut child = spawn_daemon(home.path(), pick_free_port());
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon never created its socket at {}", sock.display());
    }
    let mut stream = UnixStream::connect(&sock).await.expect("connect");
    authenticate(&mut stream, home.path()).await;

    // ── Phase 1: grant a budget of three uses.
    match req(
        &mut stream,
        Request::GrantCapability {
            action: "tool.call.echo".into(),
            scope: Some(serde_json::json!({ "version": 1, "tool": "echo", "max_uses": 3 })),
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { .. } => {}
        other => panic!("grant tool.call.echo failed: {other:?}"),
    }

    // ── Phase 2: three calls, each within budget. A hardcoded limit of 1 (or any
    //     value below the granted 3) would deny one of these — the exact signal
    //     the max_uses=1 test cannot produce.
    for n in 1..=3 {
        match call_echo(&mut stream, &format!("call {n} of 3")).await {
            Response::ToolResult { is_error, .. } => assert!(
                !is_error,
                "call {n} of 3 is within the max_uses=3 budget and must run"
            ),
            other => panic!("expected a tool result for call {n} of 3, got {other:?}"),
        }
    }

    // ── Phase 3: the fourth call exhausts the budget and is refused with a
    //     distinct CapabilityBudgetExhausted row (not a missing-capability denial).
    match call_echo(&mut stream, "call 4 (over budget)").await {
        Response::Error { .. } => {}
        other => panic!("the fourth call exceeds max_uses=3 and must be refused, got {other:?}"),
    }
    let events = recent_audit(&mut stream).await;
    let (action, max_uses, used, signature_b58) = latest_budget_exhausted(&events);
    assert_eq!(action, "tool.call.echo");
    assert_eq!(
        max_uses, 3,
        "the refusal must name the granted budget, not a hardcoded 1"
    );
    assert_eq!(used, 3, "used must equal max_uses at refusal");
    assert!(
        !signature_b58.is_empty(),
        "the spent grant's signature is the join key"
    );

    // ── Phase 4: the durable accounting reads back as 3/3/0 Exhausted. A second
    //     read must report the same used count — observing usage records no use.
    let entry = echo_usage_entry(&mut stream).await;
    let budget = entry
        .budget
        .expect("the budgeted grant must report a usage budget");
    assert_eq!(budget.max_uses, 3, "max_uses");
    assert_eq!(budget.used, 3, "used after three calls");
    assert_eq!(budget.remaining, 0, "remaining after three calls");
    assert_eq!(
        entry.effective,
        CapabilityEffectiveStatus::Exhausted,
        "a fully spent budget must read Exhausted"
    );

    let entry_again = echo_usage_entry(&mut stream).await;
    assert_eq!(
        entry_again
            .budget
            .expect("the budgeted grant must still report a usage budget")
            .used,
        3,
        "observing usage must not record a use"
    );

    // ── Phase 5: the budget denial is in-band — the session stays usable.
    match req(&mut stream, Request::Ping).await {
        Response::Pong => {}
        other => panic!("the session must stay usable after the budget denial, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
