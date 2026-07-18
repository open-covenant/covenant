//! Live integration test: the `LinearExtrapolation` projection policy
//! preempts an in-flight subprocess *before* its agent's bucket reaches
//! zero. Spawns covenantd with `COVENANT_BUDGET_PROJECTION_POLICY=linear`
//! (parsed by `parse_projection_policy`, covenantd/src/lib.rs) against a
//! fixture agent whose bucket holds 10 credits, then drives five
//! 1-credit dispatch debits — the bucket bottoms out at 5, so the
//! projection tick's policy-independent exhaustion trigger
//! (`would_exceed(agent, 1)`, true only at `tokens_remaining == 0`) is
//! unreachable for the whole run. A `BudgetPreempted` audit row can
//! therefore only come from the linear leg of
//! `covenant_budget::project_overshoot`: `2 × observed_debit > remaining`.
//!
//! The debit-rate signal (`BudgetLedger::debit_rate_since`) is
//! per-agent, scoped to `at_ms >= started_at_ms` of the tracked
//! subprocess, so the steady debits are driven by dispatching four more
//! intents to the same fixture agent while the first subprocess sleeps:
//! after the fifth dispatch the first entry observes 4 debits over a
//! ~800ms window against 5 remaining, and 2×4 > 5 flags it. The
//! min-window/min-samples gates are set low but real (200ms / 3
//! samples), so the first post-dispatch tick — one debit, ~100ms window
//! — stays below both thresholds and does not preempt a cold window.
//!
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_projection_preempt_linear -- --ignored live_`.

use covenant_audit::AuditKind;
use covenant_ipc::{read_frame, write_frame, Request, Response};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::Command;
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
    write_frame(stream, &Request::Authenticate { token_b58 })
        .await
        .unwrap();
    match read_frame::<_, Response>(stream).await.unwrap() {
        Response::Authenticated { .. } => {}
        other => panic!("authenticate failed: {other:?}"),
    }
}

/// Fire one SubmitIntent on its own connection and keep the stream
/// alive without reading the response. Dispatch awaits the subprocess,
/// so reading would block for the full run; the debit this test cares
/// about lands at dispatch time, before `runner.run`.
async fn submit_on_own_connection(sock: &Path, home: &Path, text: &str) -> UnixStream {
    let mut stream = UnixStream::connect(sock).await.expect("connect");
    authenticate(&mut stream, home).await;
    write_frame(
        &mut stream,
        &Request::SubmitIntent {
            text: text.into(),
            prefer_stream: None,
        },
    )
    .await
    .unwrap();
    stream
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts linear-projection BudgetPreempted before bucket exhaustion"]
async fn live_projection_preempt_linear_pauses_before_exhaustion() {
    let home = tempfile::tempdir().expect("tempdir");
    let port = pick_free_port();

    let agents_dir = home.path().join("agents").join("preempt-fixture");
    std::fs::create_dir_all(&agents_dir).expect("agents dir");
    let staged = agents_dir.join("preempt_fixture");
    std::fs::copy(env!("CARGO_BIN_EXE_preempt_fixture"), &staged).expect("stage fixture binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&staged).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&staged, perms).unwrap();
    }
    // Capacity 10 with five 1-credit dispatches means tokens_remaining
    // never drops below 5 (refill at 10/hour adds nothing over a few
    // seconds), so the exhaustion trigger cannot fire and any preempt is
    // projection-attributed by construction.
    let manifest = r#"
[agent]
id = "preempt-fixture"
name = "Preempt Fixture"
version = "0.0.1"
runtime = "rust-bin"
entry = "preempt_fixture"

[capabilities]
required = ["intent.subscribe"]

[resources]
cpu_ms_per_task = 10000

[settlement]
budget_credits_per_hour = 10
"#;
    std::fs::write(agents_dir.join("agent.toml"), manifest).expect("write manifest");

    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(exe)
        .env("COVENANT_HOME", home.path())
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home.path())
        .env("COVENANT_BUDGET_PROJECTION_TICK_MS", "100")
        .env("COVENANT_BUDGET_PREEMPT_GRACE_MS", "250")
        .env("COVENANT_BUDGET_PROJECTION_POLICY", "linear")
        .env("COVENANT_BUDGET_PROJECTION_MIN_WINDOW_MS", "200")
        .env("COVENANT_BUDGET_PROJECTION_MIN_SAMPLES", "3")
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

    let mut stream = UnixStream::connect(&sock).await.expect("connect");
    authenticate(&mut stream, home.path()).await;

    // memory.write for the dispatch path's result write, intent.subscribe
    // for the router's keyword bridge — same pair live_runtime_preempt
    // needs. Grants are store-wide, so the dispatch connections below
    // inherit them.
    for action in ["memory.write", "intent.subscribe"] {
        write_frame(
            &mut stream,
            &Request::GrantCapability {
                action: action.into(),
                scope: None,
                expires_at: None,
            },
        )
        .await
        .unwrap();
        let g: Response = read_frame(&mut stream).await.unwrap();
        assert!(
            matches!(g, Response::CapabilityGranted { .. }),
            "grant {action} failed: {g:?}",
        );
    }

    // Intent #1 is the projection target: its fixture subprocess sleeps
    // 3s, holding the tracker entry open. Intents #2–#5 are the steady
    // debit source — each dispatch try_debits 1 credit against the same
    // agent with at_ms inside #1's observation window. 200ms spacing
    // keeps all five debits well inside the 3s sleep while giving the
    // window time to clear the 200ms minimum.
    let mut dispatch_streams = Vec::new();
    for n in 1..=5u32 {
        dispatch_streams
            .push(submit_on_own_connection(&sock, home.path(), &format!("test ping {n}")).await);
        if n < 5 {
            sleep(Duration::from_millis(200)).await;
        }
    }

    // The fifth debit lands ~800ms in; the next tick (≤100ms later)
    // should flag intent #1 (4 observed debits, 5 remaining, 8 > 5).
    // 6s covers the 3s fixture sleep entirely, so a missing row means
    // the projection leg never fired, not that we gave up early.
    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    let events = loop {
        write_frame(
            &mut stream,
            &Request::RecentAudit {
                limit: 64,
                since_ms: None,
                prefer_stream: None,
            },
        )
        .await
        .unwrap();
        let ar: Response = read_frame(&mut stream).await.unwrap();
        let events = match ar {
            Response::AuditEvents { events } => events,
            other => panic!("unexpected: {other:?}"),
        };
        if events
            .iter()
            .any(|e| matches!(e.kind, AuditKind::BudgetPreempted { .. }))
            || std::time::Instant::now() >= deadline
        {
            break events;
        }
        sleep(Duration::from_millis(100)).await;
    };

    let preempt = events
        .iter()
        .find(|e| matches!(e.kind, AuditKind::BudgetPreempted { .. }))
        .unwrap_or_else(|| {
            panic!(
                "linear projection must preempt the in-flight intent before exhaustion; events seen: {events:?}"
            )
        });
    if let AuditKind::BudgetPreempted {
        agent_display,
        reason,
        signal_sent,
        ..
    } = &preempt.kind
    {
        assert_eq!(
            agent_display, "preempt-fixture",
            "BudgetPreempted carries the raw manifest id from TrackedSubprocess.agent_id",
        );
        assert_eq!(
            reason, "budget_projected_overshoot",
            "the tick attributes each trigger with its own reason slug, so the linear leg must \
             carry the projection-attributed slug; the debit arithmetic below independently \
             corroborates that only the linear leg could have fired",
        );
        assert!(
            matches!(signal_sent.as_str(), "SIGTERM" | "SIGKILL" | "none"),
            "signal_sent must be one of the documented variants; got {signal_sent:?}",
        );
    }

    // The bucket never emptied, so the dispatch-time exhaustion arm must
    // never have fired: a BudgetExhausted row here would mean the
    // preempt above could be the plain empty-bucket trigger and the
    // linear leg proved nothing.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, AuditKind::BudgetExhausted { .. })),
        "no dispatch may hit the exhaustion arm — capacity 10 vs 5 debits: {events:?}",
    );

    // Pin the arithmetic the projection-attribution argument rests on:
    // exactly five 1-credit debits against a 10-credit bucket. Fewer
    // rows would mean a dispatch missed the router (weakening the debit
    // signal); more, or a larger credit, would mean the bucket could
    // have emptied and the exhaustion trigger become reachable. Polled,
    // because an early preempt (same-millisecond debit edge) can break
    // the audit loop before dispatch #5's debit reaches the ledger.
    let debit_deadline = std::time::Instant::now() + Duration::from_secs(2);
    let fixture_debits = loop {
        write_frame(&mut stream, &Request::RecentDebits { limit: 64 })
            .await
            .unwrap();
        let debits = match read_frame::<_, Response>(&mut stream).await.unwrap() {
            Response::Debits { debits } => debits,
            other => panic!("expected Response::Debits, got {other:?}"),
        };
        let fixture: Vec<_> = debits
            .into_iter()
            .filter(|d| d.agent.display == "preempt-fixture@agent")
            .collect();
        if fixture.len() >= 5 || std::time::Instant::now() >= debit_deadline {
            break fixture;
        }
        sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        fixture_debits.len(),
        5,
        "all five dispatches must debit exactly once: {fixture_debits:?}",
    );
    assert!(
        fixture_debits.iter().all(|d| d.credits == 1),
        "dispatch debits are the flat 1-credit intent cost: {fixture_debits:?}",
    );

    drop(dispatch_streams);
    drop(stream);
    let _ = child.kill().await;
}
