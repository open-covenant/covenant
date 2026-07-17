//! Live integration test: a budget-paused intent actually comes back —
//! `covenant intents resume latest` exits zero after a sleep-free capacity
//! raise, resumes via its pause checkpoint, and debits exactly once.
//!
//! Closes the daemon-ipc-core live-coverage gap ("Add a resume-success CLI
//! fixture once budget refill semantics can be exercised without long
//! sleeps"). The sleep-free lever is `BudgetLedger::set_capacity`'s raise
//! semantics (covenant-budget/src/lib.rs): on a capacity change it refills
//! at the OLD rate, swaps the capacity, clamps tokens only downward, and
//! re-stamps `last_refill_ms` — a raise credits nothing immediately, but
//! the refill RATE becomes the new capacity/hour. Restarting the daemon
//! with `budget_credits_per_hour = 3_600_000` (1000 tokens/sec) therefore
//! turns an exhausted 1-credit/hour bucket into one whose refill ETA for a
//! single-credit dispatch is ~1ms past boot: `register_agent_budgets`
//! (covenantd/src/lib.rs) re-stamps the raised capacity right after
//! `Server::new`, and the bucket has accrued dispatch credit long before
//! the CLI can connect. No sleeps beyond a bounded readiness retry.
//!
//! Complements the two existing resume fixtures, which only ever prove
//! failure shapes: `live_cli_intents_resume.rs` (rejection while the bucket
//! stays empty) and `live_cli_intents_resume_checkpoint_fallback.rs` (the
//! lost-checkpoint legacy fallback, asserting NO `resume_claimed` line).
//! This test asserts the inverse seam: the resume takes the checkpoint Ok
//! path (`resume_claimed` IS appended to `budget/checkpoints.jsonl`) and
//! the re-dispatch debits exactly one more `"type":"debit"` row in
//! `budget/ledger.jsonl` — one paired debit, no double-charge for the
//! already-settled first run.
//!
//! Uses the dispatch-to-exhaust setup from `live_cli_intents_resume.rs` and
//! the two-phase kill+respawn shape from
//! `live_cli_intents_resume_checkpoint_fallback.rs`. Hermetic — the
//! research agent falls back to canned text with no providers.
//! `#[ignore]`'d so it only runs under `--ignored live_`. Build prereqs:
//! `cargo build -p covenant -p research-agent`.
//!
//! Run with:
//! `cargo test -p covenantd --test live_cli_intents_resume_success -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;

fn covenant_cli_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/covenant")
        .canonicalize()
        .expect("covenant CLI binary not built; run `cargo build -p covenant` first")
}

fn research_agent_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/research")
        .canonicalize()
        .expect("research binary not built; run `cargo build -p research-agent` first")
}

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
    panic!("operator token never appeared at {}", path.display())
}

// The projection tick would otherwise fire mid-dispatch and SIGKILL the
// first, validly-admitted single-credit run before it can exhaust the budget
// (see live_cli_intents_resume). Push its period past the test window.
fn spawn_daemon(home: &Path, port: u16) -> Child {
    Command::new(env!("CARGO_BIN_EXE_covenantd"))
        .env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("COVENANT_BUDGET_PROJECTION_TICK_MS", "3600000")
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd")
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

fn manifest_with_budget(credits_per_hour: u64) -> String {
    format!(
        r#"
[agent]
id = "research"
name = "Research Agent"
version = "0.0.1"
runtime = "rust-bin"
entry = "research"

[capabilities]
required = ["tool.web_search"]

[settlement]
budget_credits_per_hour = {credits_per_hour}
"#
    )
}

fn count_debit_lines(ledger_path: &Path) -> usize {
    let raw = std::fs::read_to_string(ledger_path)
        .unwrap_or_else(|_| panic!("ledger.jsonl must exist at {}", ledger_path.display()));
    // BudgetEvent is #[serde(tag = "type", rename_all = "snake_case")], so
    // every debit row carries this literal tag (covenant-budget/src/lib.rs).
    raw.lines()
        .filter(|l| l.contains("\"type\":\"debit\""))
        .count()
}

#[tokio::test]
#[ignore = "live: spawns covenantd twice + verifies `covenant intents resume` succeeds after a capacity raise"]
async fn live_cli_intents_resume_succeeds_after_sleep_free_capacity_raise() {
    let home = tempfile::tempdir().expect("tempdir");
    let research_bin = research_agent_bin();

    let agents_dir = home.path().join("agents").join("research");
    std::fs::create_dir_all(&agents_dir).expect("agents dir");
    let staged = agents_dir.join("research");
    std::fs::copy(&research_bin, &staged).expect("stage research binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&staged).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&staged, perms).unwrap();
    }
    let manifest_path = agents_dir.join("agent.toml");
    std::fs::write(&manifest_path, manifest_with_budget(1)).expect("write manifest");

    let sock = home.path().join("sock");
    let checkpoints_path = home.path().join("budget").join("checkpoints.jsonl");
    let ledger_path = home.path().join("budget").join("ledger.jsonl");

    // --- Daemon #1: budget 1/hour. Dispatch to exhaust, minting a
    // pause_saved checkpoint + BudgetExhausted audit row.
    let mut child = spawn_daemon(home.path(), pick_free_port());
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon #1 never created its socket");
    }

    {
        let mut stream = UnixStream::connect(&sock).await.expect("connect #1");
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
            "first dispatch must pass and debit the single credit, got {r1:?}"
        );

        write_frame(
            &mut stream,
            &Request::SubmitIntent {
                text: "find another batch of papers on agent memory".into(),
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
    }

    let checkpoints_before = std::fs::read_to_string(&checkpoints_path).unwrap_or_else(|_| {
        panic!(
            "checkpoints.jsonl must exist after exhaustion: {}",
            checkpoints_path.display()
        )
    });
    assert!(
        checkpoints_before.contains("pause_saved"),
        "exhaustion must persist a pause_saved checkpoint; got {checkpoints_before:?}"
    );

    // The rejected dispatch must not have debited: only the first run pays.
    let debits_before = count_debit_lines(&ledger_path);
    assert_eq!(
        debits_before, 1,
        "exactly the first dispatch debits before resume"
    );

    // --- Kill, raise the budget three-million-fold, respawn. On boot,
    // register_agent_budgets re-stamps the raised capacity; the bucket then
    // refills at 1000 tokens/sec, so the single-credit re-dispatch is
    // admissible ~1ms later — no sleeps required.
    let _ = child.kill().await;
    let _ = child.wait().await;

    std::fs::write(&manifest_path, manifest_with_budget(3_600_000))
        .expect("rewrite manifest with raised budget");

    let mut child = spawn_daemon(home.path(), pick_free_port());
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon #2 never created its socket");
    }

    let cli_exe = covenant_cli_bin();

    // --- Resume: must exit zero. Bounded readiness retry (worst case
    // ~2.5s) armors slow CI hosts; the expected path succeeds first try.
    let mut last_stdout = String::new();
    let mut last_stderr = String::new();
    let mut success_stdout = None;
    for _ in 0..10 {
        let out = Command::new(&cli_exe)
            .arg("intents")
            .arg("resume")
            .arg("latest")
            .arg("--json")
            .env("COVENANT_HOME", home.path())
            .env("HOME", home.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .expect("spawn covenant CLI (intents resume latest --json)");
        last_stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        last_stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if out.status.success() {
            success_stdout = Some(last_stdout.clone());
            break;
        }
        sleep(Duration::from_millis(250)).await;
    }
    let json_stdout = success_stdout.unwrap_or_else(|| {
        let _ = child.start_kill();
        panic!(
            "resume latest --json never exited zero after the capacity raise; \
             last stdout={last_stdout:?} last stderr={last_stderr:?}"
        )
    });

    let value: Value =
        serde_json::from_str(&json_stdout).expect("resume latest --json stdout must be JSON");
    assert_eq!(value["kind"], "intents_resume");
    assert_eq!(
        value["ok"], true,
        "resume must report ok after the raise; stdout={json_stdout:?}"
    );
    assert_eq!(value["mode"], "latest");
    assert_eq!(
        value["status"], "ok",
        "resumed dispatch must complete; stdout={json_stdout:?}"
    );
    assert!(value["intent_id"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(
        value["text"].as_str().is_some_and(|t| !t.is_empty()),
        "resumed run must surface the agent's text; stdout={json_stdout:?}"
    );

    // The success must run through the checkpoint Ok path — claim_resume
    // appends resume_claimed (the exact line the lost-checkpoint fallback
    // test asserts is ABSENT on its NotFound path).
    let checkpoints_after = std::fs::read_to_string(&checkpoints_path).unwrap_or_default();
    assert!(
        checkpoints_after.contains("resume_claimed"),
        "successful resume must claim its checkpoint; got {checkpoints_after:?}"
    );

    // One paired debit for the resumed run — not zero (nothing dispatched)
    // and not two (double-charge for the already-settled first run).
    let debits_after = count_debit_lines(&ledger_path);
    assert_eq!(
        debits_after,
        debits_before + 1,
        "resume must debit exactly once; ledger went {debits_before} -> {debits_after}"
    );

    let _ = child.kill().await;
}
