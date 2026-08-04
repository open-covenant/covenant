//! Live integration test: eight concurrent tool calls contend for the final
//! (only) unit of a `max_uses = 1` capability budget through the real
//! multi-threaded daemon — exactly one wins, seven are refused, and the unit is
//! spent exactly once.
//!
//! `covenant-permissions` serializes `consume_uses` per store (a `Mutex` guards
//! the read-count-check-append), so two concurrent callers can never both
//! consume the last unit. Both existing live budget tests
//! (`live_capability_usage_budget.rs`, `live_capability_usage_budget_multi_use.rs`)
//! issue strictly sequential calls, so neither drives that store under concurrent
//! load. This test does: it opens one authenticated connection per racer, aligns
//! the tool calls on a barrier, and asserts the contract that must hold under any
//! interleaving.
//!
//! Every assertion is interleaving-independent — it holds under any scheduler
//! ordering, so the test passes deterministically against the correct daemon and
//! cannot flake by timing:
//!   - exactly one of the barrier-aligned calls returns `ToolResult` with
//!     `is_error = false`; the other seven return `Response::Error`
//!   - exactly seven `CapabilityBudgetExhausted` audit rows land — one per
//!     refused racer, none for the winner — each naming `used = 1`,
//!     `max_uses = 1`, and the one contended grant's signature (row-count
//!     equality proves each loser was refused by the budget gate, not by
//!     authorization; a double-spend would surface as `used = 2` here)
//!   - the durable accounting reads back `used 1 / max_uses 1 / remaining 0`
//!     with effective `Exhausted`: the final unit was consumed exactly once
//!
//! Scope. This drives the concurrent path and pins the aggregate contract; it
//! does not *force* the sub-millisecond `consume_uses` read-then-append window to
//! collide. Each call's audit row is fsync-serialized before the budget gate
//! runs (`record_peer_event` precedes `consume_uses` in the dispatch path), which
//! staggers the racers enough that on a fast host the window rarely overlaps — so
//! deleting the store's serialization `Mutex` is not reliably observable through
//! this black-box surface. A deterministic guard for that exact TOCTOU belongs at
//! the `covenant-permissions` store layer under a loom / injected-yield harness;
//! it is recorded as the open residual on the capability-usage-budget
//! live-coverage boundary.
//!
//! Racers hold one authenticated connection each — a shared connection would
//! serialize the calls at the request-framing layer and make the contention
//! vacuous.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_capability_usage_budget_race -- --ignored live_`.

use covenant_audit::{AuditEvent, AuditKind};
use covenant_ipc::{
    read_frame, write_frame, CapabilityEffectiveStatus, CapabilityUsageEntry, Request, Response,
};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::Barrier;
use tokio::time::sleep;

/// Number of concurrent calls contending for the single budget unit.
const RACERS: usize = 8;

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
#[ignore = "live: spawns covenantd + races 8 concurrent calls for a max_uses=1 budget's final unit"]
async fn live_covenantd_capability_usage_budget_final_unit_race() {
    let home = tempfile::tempdir().expect("tempdir");
    let sock = home.path().join("sock");

    let mut child = spawn_daemon(home.path(), pick_free_port());
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon never created its socket at {}", sock.display());
    }

    // Operator session for the grant and the post-race readbacks.
    let mut op = UnixStream::connect(&sock).await.expect("connect operator");
    authenticate(&mut op, home.path()).await;

    // ── Phase 1: grant one budgeted unit. The fixture is identical to the
    //    sequential max_uses=1 test — only the calling pattern differs.
    match req(
        &mut op,
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

    // ── Phase 2: eight racers, one authenticated connection each, all
    //    barrier-aligned so the contended section contains nothing but the
    //    CallTool itself.
    let mut conns = Vec::with_capacity(RACERS);
    for n in 0..RACERS {
        let mut stream = UnixStream::connect(&sock)
            .await
            .unwrap_or_else(|e| panic!("connect racer {n}: {e}"));
        authenticate(&mut stream, home.path()).await;
        conns.push(stream);
    }

    let barrier = Arc::new(Barrier::new(RACERS));
    let mut racers = Vec::with_capacity(RACERS);
    for (n, mut stream) in conns.into_iter().enumerate() {
        let barrier = Arc::clone(&barrier);
        racers.push(tokio::spawn(async move {
            barrier.wait().await;
            let response = call_echo(&mut stream, &format!("racer {n}")).await;
            (n, response)
        }));
    }

    let mut wins = 0usize;
    let mut refusals = 0usize;
    for racer in racers {
        let (n, response) = racer.await.expect("racer task must not panic");
        match response {
            Response::ToolResult { is_error, .. } => {
                assert!(
                    !is_error,
                    "racer {n}: a winning call must not be a tool error"
                );
                wins += 1;
            }
            Response::Error { .. } => refusals += 1,
            other => panic!("racer {n}: expected ToolResult or Error, got {other:?}"),
        }
    }
    assert_eq!(
        wins, 1,
        "exactly one racer may consume the final budget unit"
    );
    assert_eq!(refusals, RACERS - 1, "every other racer must be refused");

    // ── Phase 3: exactly one CapabilityBudgetExhausted row per refused racer,
    //    none for the winner, all pinning used=1/max_uses=1 on the same grant.
    //    Row-count equality proves each loser was refused by the budget gate
    //    specifically — an authorization denial writes no exhaustion row.
    let events = recent_audit(&mut op).await;
    let exhausted: Vec<(String, u64, u64, String)> = events
        .iter()
        .filter_map(|event| match &event.kind {
            AuditKind::CapabilityBudgetExhausted {
                action,
                max_uses,
                used,
                signature_b58,
            } => Some((action.clone(), *max_uses, *used, signature_b58.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        exhausted.len(),
        RACERS - 1,
        "one exhaustion row per refused racer, none for the winner"
    );
    let reference_signature = &exhausted[0].3;
    for (action, max_uses, used, signature_b58) in &exhausted {
        assert_eq!(action, "tool.call.echo");
        assert_eq!(*max_uses, 1, "the refusal must name the granted budget");
        assert_eq!(
            *used, 1,
            "used must never exceed max_uses — a double-spend would read used=2"
        );
        assert!(
            !signature_b58.is_empty(),
            "the spent grant's signature is the join key"
        );
        assert_eq!(
            signature_b58, reference_signature,
            "every refusal must name the one contended grant"
        );
    }

    // ── Phase 4: durable accounting — the final unit was spent exactly once.
    let entry = echo_usage_entry(&mut op).await;
    let budget = entry
        .budget
        .expect("the budgeted grant must report a usage budget");
    assert_eq!(budget.max_uses, 1, "max_uses");
    assert_eq!(
        budget.used, 1,
        "the final unit must be consumed exactly once"
    );
    assert_eq!(budget.remaining, 0, "remaining after the race");
    assert_eq!(
        entry.effective,
        CapabilityEffectiveStatus::Exhausted,
        "a fully spent budget must read Exhausted"
    );

    // ── Phase 5: the contended denials are in-band — the session stays usable.
    match req(&mut op, Request::Ping).await {
        Response::Pong => {}
        other => panic!("the session must stay usable after the race, got {other:?}"),
    }

    drop(op);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
