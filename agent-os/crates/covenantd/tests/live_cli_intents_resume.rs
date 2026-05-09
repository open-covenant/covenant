//! Live integration test: spawns covenantd against a tempdir HOME with a
//! research-agent manifest pinned to `budget_credits_per_hour = 1`,
//! exhausts the budget to mint a `BudgetExhausted` audit row, then runs
//! `covenant intents resume latest` as a subprocess and asserts the CLI
//! locates the row (instead of failing with "no BudgetExhausted audit row").
//!
//! Closes the gap between the IPC-level resume plumbing in
//! `live_budget_enforcement.rs` and the CLI's `intents resume` verb.
//! Hermetic — the research agent falls back to canned text when no
//! providers are configured. `#[ignore]`'d. Build prereqs:
//! `cargo build -p covenant -p research-agent`.
//!
//! Run with:
//! `cargo test -p covenantd --test live_cli_intents_resume -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::Command;
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
#[ignore = "live: spawns covenantd + runs `covenant intents resume latest` subprocess"]
async fn live_cli_intents_resume_latest_round_trip() {
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

    let daemon_exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(daemon_exe)
        .env("COVENANT_HOME", home.path())
        .env("COVENANT_HTTP_PORT", "0")
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

    write_frame(
        &mut stream,
        &Request::GrantCapability {
            action: "tool.web_search".into(),
            scope: None,
            expires_at: None,
        },
    )
    .await
    .unwrap();
    let g: Response = read_frame(&mut stream).await.unwrap();
    assert!(matches!(g, Response::CapabilityGranted { .. }), "{g:?}");

    write_frame(
        &mut stream,
        &Request::SubmitIntent {
            text: "find recent papers on agent memory".into(),
        },
    )
    .await
    .unwrap();
    let r1: Response = read_frame(&mut stream).await.unwrap();
    assert!(
        matches!(r1, Response::IntentResult { ref status, .. } if status == "ok"),
        "first dispatch must pass, got {r1:?}"
    );

    write_frame(
        &mut stream,
        &Request::SubmitIntent {
            text: "find another batch of papers on agent memory".into(),
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

    drop(stream);

    let cli_exe = covenant_cli_bin();
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
        "resume latest should exit non-zero while bucket remains empty; stdout={stdout:?} stderr={stderr:?}"
    );

    assert!(
        stderr.contains("budget exhausted"),
        "stderr missing budget exhaustion error; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stderr.contains("no BudgetExhausted audit row"),
        "stderr suggests resume did not locate the audit row; stdout={stdout:?} stderr={stderr:?}"
    );

    let _ = child.kill().await;
}
