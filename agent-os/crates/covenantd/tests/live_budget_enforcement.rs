//! Live integration test: spawns covenantd against a tempdir HOME with a
//! research-agent manifest pinned to `budget_credits_per_hour = 1`,
//! dispatches the same matching intent twice, and asserts the second
//! dispatch is rejected with `Response::Error` while the audit log gains
//! a `BudgetExhausted` row carrying the rejected text.
//!
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_budget_enforcement -- --ignored live_`.

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

fn research_agent_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/research")
        .canonicalize()
        .expect("research binary not built; run `cargo build -p research-agent` first")
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
#[ignore = "live: spawns covenantd + exhausts a budget=1 agent"]
async fn live_covenantd_rejects_when_budget_exhausted() {
    let home = tempfile::tempdir().expect("tempdir");
    let port = pick_free_port();
    let bin = research_agent_bin();

    let agents_dir = home.path().join("agents").join("research");
    std::fs::create_dir_all(&agents_dir).expect("agents dir");
    let staged = agents_dir.join("research");
    std::fs::copy(&bin, &staged).expect("stage research binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&staged).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&staged, perms).unwrap();
    }
    let manifest = r#"
[agent]
id = "research"
name = "Research Agent"
version = "0.0.1"
runtime = "rust-bin"
entry = "research"

[capabilities]
required = ["tool.web_search"]

[settlement]
budget_credits_per_hour = 1
"#;
    std::fs::write(agents_dir.join("agent.toml"), manifest).expect("write manifest");

    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(exe)
        .env("COVENANT_HOME", home.path())
        .env("COVENANT_HTTP_PORT", port.to_string())
        // This test exercises *pre-spawn* budget admission: dispatch 1 is
        // admitted (bucket 1 -> 0) and runs to completion, dispatch 2 is
        // rejected at admission, resume re-rejects on the empty bucket. The
        // budget projection tick (250ms default) is a separate hard-preempt
        // mechanism — documented "tokens_remaining == 0 -> kill the in-flight
        // subprocess", covered by the preempt_subprocess_pg tests — that would
        // otherwise fire mid-dispatch and SIGKILL the first, validly-admitted
        // run. Push its period past the test window so it doesn't race the
        // admission path under test here.
        .env("COVENANT_BUDGET_PROJECTION_TICK_MS", "3600000")
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

    let mut stream = UnixStream::connect(&sock).await.expect("connect");
    authenticate(&mut stream, home.path()).await;

    for action in ["tool.web_search", "memory.write"] {
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
            "grant {action} failed: {g:?}"
        );
    }

    // Dispatch 1: passes — bucket goes from 1 to 0.
    write_frame(
        &mut stream,
        &Request::SubmitIntent {
            text: "find recent papers on agent memory".into(),
            prefer_stream: None,
        },
    )
    .await
    .unwrap();
    let r1: Response = read_frame(&mut stream).await.unwrap();
    assert!(
        matches!(r1, Response::IntentResult { ref status, .. } if status == "ok"),
        "first dispatch must pass, got {r1:?}"
    );

    // Dispatch 2: same intent text — should hit budget exhaustion since
    // the bucket is empty and the refill rate is 1/hr (test runtime is
    // milliseconds, so refill is ~0).
    let exhausted_text = "find another batch of papers on agent memory";
    write_frame(
        &mut stream,
        &Request::SubmitIntent {
            text: exhausted_text.into(),
            prefer_stream: None,
        },
    )
    .await
    .unwrap();
    let r2: Response = read_frame(&mut stream).await.unwrap();
    let rejected_msg = match r2 {
        Response::Error { message } => message,
        other => panic!("expected Error from exhausted dispatch, got {other:?}"),
    };
    assert!(
        rejected_msg.contains("budget exhausted"),
        "expected budget-exhaustion message, got {rejected_msg:?}"
    );
    assert!(
        rejected_msg.contains("research"),
        "expected agent name in error, got {rejected_msg:?}"
    );

    // Audit feed must include a BudgetExhausted row with the exact
    // rejected text — this is the row `intents resume` uses.
    write_frame(
        &mut stream,
        &Request::RecentAudit {
            limit: 50,
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
    let exhausted = events
        .iter()
        .find(|e| matches!(e.kind, AuditKind::BudgetExhausted { .. }))
        .expect("expected a BudgetExhausted audit event");
    let intent_id = match &exhausted.kind {
        AuditKind::BudgetExhausted {
            agent_display,
            intent_text,
            requested,
            tokens_remaining,
            intent_id,
            ..
        } => {
            assert_eq!(agent_display, "research@agent");
            assert_eq!(intent_text, exhausted_text);
            assert_eq!(*requested, 1);
            assert_eq!(*tokens_remaining, 0);
            *intent_id
        }
        other => panic!("unexpected: {other:?}"),
    };

    // Resume verb plumbing: `ResumeIntent { intent_id }` looks up the
    // audit row, finds the rejected text, and re-dispatches. The bucket
    // is still empty so the resume itself is rejected with another
    // BudgetExhausted, but the resume verb's lookup succeeded.
    write_frame(&mut stream, &Request::ResumeIntent { intent_id })
        .await
        .unwrap();
    let resumed: Response = read_frame(&mut stream).await.unwrap();
    match resumed {
        Response::Error { message } => {
            assert!(
                message.contains("budget exhausted"),
                "resume should hit budget exhaustion (bucket empty), got {message:?}"
            );
        }
        other => panic!("expected Error from resume on empty bucket, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
}
