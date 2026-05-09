//! Live integration test for the Phase 0 §9 acceptance path: real
//! covenantd binary, real research agent, real Ollama LLM.
//!
//! Requires Ollama running locally with `qwen2.5:7b` pulled. `#[ignore]`'d.
//! Run with
//! `cargo test -p covenantd --test live_full_acceptance -- --ignored live_`.

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
        .expect("research binary not built")
}

async fn wait_for_sock(path: &std::path::Path) -> bool {
    for _ in 0..150 {
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
#[ignore = "live: full §9 acceptance path against real Ollama"]
async fn live_covenantd_full_acceptance_with_ollama() {
    let home = tempfile::tempdir().expect("tempdir");
    let port = pick_free_port();
    let bin = research_agent_bin();

    // Pin LLM to local Ollama with a model the operator has pulled. The
    // research agent reads the same secrets.toml on its own pick_provider.
    let secrets = r#"
[llm]
provider = "ollama"
endpoint = "http://localhost:11434"
model    = "qwen2.5:7b"
"#;
    std::fs::write(home.path().join("secrets.toml"), secrets).expect("write secrets");

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

[resources]
cpu_ms_per_task = 60000
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
        panic!("daemon never created its socket");
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
    let _: Response = read_frame(&mut stream).await.unwrap();

    write_frame(
        &mut stream,
        &Request::SubmitIntent {
            text: "Reply in one short sentence: what is 2+2?".into(),
        },
    )
    .await
    .unwrap();
    let r: Response = read_frame(&mut stream).await.unwrap();

    match r {
        Response::IntentResult { text, status, .. } => {
            assert_eq!(status, "ok");
            // Real LLM output must NOT be the canned-fallback path.
            assert!(
                !text.contains("research stub processed"),
                "got the pure-mock canned response — provider not selected: {text:?}"
            );
            assert!(
                !text.contains("fell back to canned response"),
                "agent's LLM call failed: {text:?}"
            );
            assert!(
                !text.contains("no agent matched"),
                "echo fallback fired: {text:?}"
            );
            // Loose smoke: qwen should mention 4 somewhere, but the model
            // is non-deterministic so we don't assert hard. The "not a
            // canned path" assertions above are the real contract.
        }
        other => panic!("unexpected response: {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
}
