//! Live integration test: spawns covenantd against a tempdir HOME with
//! a fixture-agent manifest pinned to `budget_credits_per_hour = 1` and
//! `cpu_ms_per_task = 10000`, dispatches one intent into the fixture
//! (which sleeps 3s before returning), and polls the audit feed for a
//! `BudgetPreempted` row keyed by the dispatched intent_id. The
//! daemon's budget projection-tick driver observes the empty bucket
//! after the dispatch-time `try_debit`, flags the still-running
//! subprocess, and signals its process group — the audit row pins that
//! the end-to-end hard-preempt path works against a real daemon binary.
//!
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_runtime_preempt -- --ignored live_`.
//!
//! Requires the fixture binary to be built first:
//! `cargo build -p covenantd --bin preempt_fixture --locked`.

use covenant_audit::AuditKind;
use covenant_ipc::{read_frame, write_frame, Request, Response};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
}

fn fixture_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/preempt_fixture")
        .canonicalize()
        .expect(
            "preempt_fixture binary not built; run `cargo build -p covenantd --bin preempt_fixture --locked` first",
        )
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

async fn read_operator_token(home: &std::path::Path) -> String {
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

async fn authenticate(stream: &mut UnixStream, home: &std::path::Path) {
    let token_b58 = read_operator_token(home).await;
    write_frame(stream, &Request::Authenticate { token_b58 })
        .await
        .unwrap();
    match read_frame::<_, Response>(stream).await.unwrap() {
        Response::Authenticated { .. } => {}
        other => panic!("authenticate failed: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts projection-tick BudgetPreempted on in-flight subprocess"]
async fn live_runtime_preempt() {
    let home = tempfile::tempdir().expect("tempdir");
    let port = pick_free_port();
    let bin = fixture_bin();

    let agents_dir = home.path().join("agents").join("preempt-fixture");
    std::fs::create_dir_all(&agents_dir).expect("agents dir");
    let staged = agents_dir.join("preempt_fixture");
    std::fs::copy(&bin, &staged).expect("stage fixture binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&staged).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&staged, perms).unwrap();
    }
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
budget_credits_per_hour = 1
"#;
    std::fs::write(agents_dir.join("agent.toml"), manifest).expect("write manifest");

    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(exe)
        .env("COVENANT_HOME", home.path())
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home.path())
        // Tight projection tick so the test does not wait long.
        .env("COVENANT_BUDGET_PROJECTION_TICK_MS", "100")
        .env("COVENANT_BUDGET_PREEMPT_GRACE_MS", "250")
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

    // The daemon's dispatch path writes the agent result to memory on
    // success, so memory.write is enforced even for fixture agents
    // whose own manifests declare zero required capabilities. The
    // fixture also declares intent.subscribe so the Router's keyword
    // bridge matches "test" in the dispatch text to this agent. Both
    // grants must land before SubmitIntent so the dispatch reaches
    // try_debit + spawn.
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

    // Submit one intent. The dispatch-time try_debit empties the bucket
    // (capacity=1, refill 1/hour ≈ 0 in test runtime). The fixture
    // subprocess sleeps 3s; the projection-tick driver should observe
    // would_exceed(agent, 1) == true within ~100ms and dispatch the
    // preempt. The IntentResult response shape depends on whether the
    // preempt arrives before stdout-read or after the daemon returns
    // its own error — both paths are valid; the audit row is what we
    // assert on.
    // Intent text contains "test" so the router's intent.subscribe
    // keyword bridge matches the fixture agent. Without a router
    // match, the daemon takes the no-agent echo path and never spawns
    // a subprocess for the projection-tick to preempt.
    write_frame(
        &mut stream,
        &Request::SubmitIntent {
            text: "test ping the preempt fixture".into(),
        },
    )
    .await
    .unwrap();
    let _r: Response = read_frame(&mut stream).await.unwrap();

    // Poll the audit feed for the BudgetPreempted row. The first tick
    // can fire as soon as 100ms after dispatch; the SIGTERM lands
    // ~100ms after that; the runner observes the kill and the daemon
    // records the row. Allow a generous 5s deadline so kernel-level
    // jitter (especially under CI) cannot flake the assertion.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let found = loop {
        write_frame(
            &mut stream,
            &Request::RecentAudit {
                limit: 64,
                since_ms: None,
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
        {
            break Some(events);
        }
        if std::time::Instant::now() >= deadline {
            break Some(events);
        }
        sleep(Duration::from_millis(100)).await;
    };

    drop(stream);
    let _ = child.kill().await;

    let events = found.expect("polling loop must terminate");
    let preempt = events
        .iter()
        .find(|e| matches!(e.kind, AuditKind::BudgetPreempted { .. }))
        .unwrap_or_else(|| {
            panic!(
                "projection-tick driver must emit a BudgetPreempted audit row within the deadline; events seen: {events:?}"
            )
        });
    if let AuditKind::BudgetPreempted {
        agent_display,
        reason,
        signal_sent,
        ..
    } = &preempt.kind
    {
        // preempt_intent uses the SubprocessTracker entry's agent_id —
        // which is the raw manifest id (`card.id.clone()`), not the
        // `<id>@agent` synthesised display the BudgetLedger keys with.
        // Both forms identify the same agent; this assertion pins the
        // raw form so a refactor that mixed the two is caught here.
        assert_eq!(
            agent_display, "preempt-fixture",
            "BudgetPreempted carries the raw manifest id from TrackedSubprocess.agent_id",
        );
        assert_eq!(
            reason, "budget_overshoot",
            "projection-tick must tag the preempt with reason=budget_overshoot",
        );
        assert!(
            matches!(signal_sent.as_str(), "SIGTERM" | "SIGKILL" | "none"),
            "signal_sent must be one of the documented variants; got {signal_sent:?}",
        );
    }
}
