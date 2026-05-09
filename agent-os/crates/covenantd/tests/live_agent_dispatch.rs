//! Live integration test: spawns covenantd against a tempdir HOME with a
//! research-agent manifest registered, grants the cap, dispatches a
//! matching intent, and asserts the response came from the agent's canned
//! fallback (not the echo path).
//!
//! Hermetic — no Ollama / Brave / SerpAPI required; the research agent
//! detects mock providers and returns `"research stub processed: <text>"`.
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_agent_dispatch -- --ignored live_`.

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
    // CARGO_MANIFEST_DIR for this crate is `crates/covenantd`. The research
    // binary is at `target/<profile>/research`. `cargo test` builds debug by
    // default; if a future invocation uses `--release`, this path won't
    // exist and the test will fail loudly — preferable to a silent skip.
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
#[ignore = "live: spawns covenantd + dispatches to a real research-agent subprocess"]
async fn live_covenantd_dispatches_to_research_agent() {
    let home = tempfile::tempdir().expect("tempdir");
    let port = pick_free_port();
    let bin = research_agent_bin();

    // Register a research agent in the tempdir HOME. The router reads
    // each subdirectory under `agents/` for an `agent.toml`.
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
"#;
    std::fs::write(agents_dir.join("agent.toml"), manifest).expect("write manifest");

    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(exe)
        .env("COVENANT_HOME", home.path())
        .env("COVENANT_HTTP_PORT", port.to_string())
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

    for action in ["tool.web_search", "memory.write", "memory.read"] {
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

    // Submit an intent that matches the agent's keywords ("papers" maps
    // to tool.web_search → research agent).
    write_frame(
        &mut stream,
        &Request::SubmitIntent {
            text: "find recent papers on agent memory".into(),
        },
    )
    .await
    .unwrap();
    let r: Response = read_frame(&mut stream).await.unwrap();
    match r {
        Response::IntentResult { text, status, .. } => {
            assert_eq!(status, "ok");
            // The agent's output varies by what's reachable in the
            // operator's env: pure-mock returns "research stub processed",
            // a reachable Ollama with a missing model returns "research
            // agent fell back to canned response (...)". The contract this
            // test is verifying is "the daemon dispatched to the agent, not
            // the echo fallback" — both forms of agent output start with or
            // contain "research".
            assert!(
                text.to_lowercase().contains("research"),
                "expected research-agent output, got {text:?}"
            );
            assert!(
                !text.contains("no agent matched"),
                "echo fallback fired; agent dispatch did not happen: {text:?}"
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }

    // Memory + receipt should each have one entry now.
    write_frame(
        &mut stream,
        &Request::RecentMemory {
            tier: None,
            limit: 10,
        },
    )
    .await
    .unwrap();
    let mem: Response = read_frame(&mut stream).await.unwrap();
    match mem {
        Response::Memories { records } => assert_eq!(records.len(), 1),
        other => panic!("unexpected: {other:?}"),
    }
    write_frame(&mut stream, &Request::RecentReceipts { limit: 10 })
        .await
        .unwrap();
    let rec: Response = read_frame(&mut stream).await.unwrap();
    match rec {
        Response::Receipts { receipts } => assert_eq!(receipts.len(), 1),
        other => panic!("unexpected: {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
}
