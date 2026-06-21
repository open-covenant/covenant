//! Live integration test: budget `intents resume` survives a lost
//! pause-checkpoint store across a daemon restart via the legacy audit-row
//! fallback.
//!
//! `resume_intent` (covenantd/src/lib.rs) re-dispatches a budget-exhausted
//! intent from its durable `BudgetExhausted` audit row. When the production
//! daemon wires a pause-checkpoint store (main.rs:318) it first calls
//! `claim_resume`; that reads the in-memory map rebuilt at `open()`, so it
//! returns `BudgetCheckpointError::NotFound` when the store lost the entry
//! across a restart (truncated/missing `checkpoints.jsonl`). The `NotFound`
//! arm is deliberately non-fatal — it warns and falls through to
//! `dispatch_intent` (the legacy audit-row resume) so a lost checkpoint store
//! degrades gracefully instead of permanently breaking resume for that intent.
//!
//! That arm had no live coverage: `live_cli_intents_resume` and
//! `live_budget_enforcement` both resume with the checkpoint present
//! (`claim_resume` Ok). This test exhausts a budgeted agent to mint a
//! checkpoint + `BudgetExhausted` row, SIGKILLs the daemon, truncates
//! `checkpoints.jsonl`, respawns (so `open()` rebuilds an empty map), and runs
//! `covenant intents resume latest`. The resume must re-dispatch via the
//! legacy fallback and hit the replayed-restored (still-exhausted) bucket —
//! surfacing a `budget exhausted` error, NOT the fatal
//! `resume: checkpoint claim failed` a removed fallback would produce — and
//! `checkpoints.jsonl` must carry no `ResumeClaimed` event (proving the
//! `NotFound` path ran, distinct from the `Ok` path that appends one).
//!
//! Uses the dispatch-to-exhaust setup from `live_cli_intents_resume.rs` and
//! the two-phase kill+respawn shape from `live_restart_peers_revoke.rs`.
//! Hermetic — the research agent falls back to canned text with no providers.
//! `#[ignore]`'d so it only runs under `--ignored live_`. Build prereqs:
//! `cargo build -p covenant -p research-agent`.
//!
//! Run with:
//! `cargo test -p covenantd --test live_cli_intents_resume_checkpoint_fallback -- --ignored live_`.

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

#[tokio::test]
#[ignore = "live: spawns covenantd twice + verifies budget resume survives a lost checkpoint store"]
async fn live_cli_intents_resume_survives_lost_checkpoint_via_legacy_fallback() {
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

    let sock = home.path().join("sock");
    let checkpoints_path = home.path().join("budget").join("checkpoints.jsonl");

    // --- Daemon #1: dispatch to exhaust, minting a checkpoint + BudgetExhausted row.
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

    // The checkpoint save (covenantd lib.rs save_budget_pause_checkpoint) runs
    // before the exhaustion Response returns, so the PauseSaved line is on disk.
    let checkpoints_before = std::fs::read_to_string(&checkpoints_path)
        .unwrap_or_else(|_| panic!("checkpoints.jsonl must exist after exhaustion: {}", checkpoints_path.display()));
    assert!(
        checkpoints_before.contains("pause_saved"),
        "exhaustion must persist a pause_saved checkpoint; got {checkpoints_before:?}"
    );

    // --- Kill, drop the in-memory checkpoint map, respawn.
    let _ = child.kill().await;
    let _ = child.wait().await;

    std::fs::write(&checkpoints_path, "").expect("truncate checkpoints.jsonl");
    assert_eq!(
        std::fs::read_to_string(&checkpoints_path).unwrap(),
        "",
        "checkpoints.jsonl must be empty before respawn"
    );

    let mut child = spawn_daemon(home.path(), pick_free_port());
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon #2 never created its socket");
    }

    let cli_exe = covenant_cli_bin();

    // --- Resume #1 (text): NotFound -> legacy fallback -> re-dispatch -> re-exhaust.
    let cli_out = Command::new(&cli_exe)
        .arg("intents")
        .arg("resume")
        .arg("latest")
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI (intents resume latest)");
    let stdout = String::from_utf8_lossy(&cli_out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&cli_out.stderr).to_string();

    assert!(
        !cli_out.status.success(),
        "resume must exit non-zero (bucket replayed to exhausted); stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.contains("budget exhausted"),
        "resume must re-dispatch via the fallback and re-exhaust (not a checkpoint-claim error); \
         stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stderr.contains("checkpoint claim failed") && !stderr.contains("resume: checkpoint"),
        "the NotFound fallback must NOT surface a fatal checkpoint-claim error; stdout={stdout:?} stderr={stderr:?}"
    );

    // The NotFound path returns before claim_resume appends ResumeClaimed, so no
    // claim line can exist — proving resume took NotFound (not Ok).
    let checkpoints_after = std::fs::read_to_string(&checkpoints_path).unwrap_or_default();
    assert!(
        !checkpoints_after.contains("resume_claimed"),
        "resume must not have claimed a checkpoint (NotFound path); got {checkpoints_after:?}"
    );

    // --- Resume #2 (--json): same fallback outcome, machine-readable.
    let cli_out_json = Command::new(&cli_exe)
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
    let json_stdout = String::from_utf8_lossy(&cli_out_json.stdout).trim().to_string();
    let json_stderr = String::from_utf8_lossy(&cli_out_json.stderr).trim().to_string();

    assert!(
        !cli_out_json.status.success(),
        "resume --json must exit non-zero; stdout={json_stdout:?} stderr={json_stderr:?}"
    );
    assert!(
        !json_stdout.is_empty(),
        "resume --json must emit a JSON object; stderr={json_stderr:?}"
    );
    let value: Value =
        serde_json::from_str(&json_stdout).expect("resume --json stdout must be JSON");
    assert_eq!(value["kind"], "intents_resume");
    assert_eq!(value["ok"], false);
    assert!(value["intent_id"].as_str().is_some_and(|s| !s.is_empty()));
    assert_eq!(value["error"]["code"], "daemon_error");
    assert!(
        value["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("budget")),
        "expected JSON error to mention the re-dispatch budget exhaustion; stdout={json_stdout:?}"
    );
    assert!(
        !json_stdout.contains("checkpoint claim failed"),
        "the NotFound fallback must not surface a fatal checkpoint-claim error; stdout={json_stdout:?}"
    );

    let _ = child.kill().await;
}
