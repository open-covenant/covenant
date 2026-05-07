//! Live integration test: spawns the `research` binary as a real
//! subprocess, pipes a JSON `Intent` on stdin, reads the JSON `AgentResult`
//! from stdout. Hermetic — sets `COVENANT_HOME` to a tempdir so the agent
//! falls back to its canned-text path (no Ollama / Brave / SerpAPI required).
//!
//! `#[ignore]`'d. Run with `cargo test -p research-agent -- --ignored live_`.

use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

#[tokio::test]
#[ignore = "live: spawns the research binary as a real subprocess"]
async fn live_research_agent_returns_result_via_stdio() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let exe = env!("CARGO_BIN_EXE_research");

    let mut child = Command::new(exe)
        .env("COVENANT_HOME", tmp.path())
        // Wipe HOME so the agent doesn't read the operator's real
        // ~/.covenant/secrets.toml and try to call live providers — we want
        // the canned fallback path for hermeticity.
        .env("HOME", tmp.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn research agent");

    // research-agent only deserialises `text` from the Intent envelope; the
    // other fields are tolerated as unknown.
    let intent = serde_json::json!({
        "text": "find recent papers on agent memory"
    });
    {
        let mut stdin = child.stdin.take().expect("stdin");
        stdin
            .write_all(serde_json::to_string(&intent).unwrap().as_bytes())
            .await
            .expect("write intent");
        stdin.flush().await.ok();
        // Drop to send EOF — research-agent reads stdin to EOF.
    }

    let mut stdout = child.stdout.take().expect("stdout");
    let mut buf = String::new();
    stdout.read_to_string(&mut buf).await.expect("read stdout");
    let status = child.wait().await.expect("wait");
    assert!(status.success(), "research agent exited non-zero: {status}");

    let result: serde_json::Value = serde_json::from_str(buf.trim())
        .unwrap_or_else(|e| panic!("agent stdout is not JSON: {e}\n--- stdout ---\n{buf}"));
    let text = result
        .get("text")
        .and_then(|t| t.as_str())
        .expect("result.text");
    assert!(!text.is_empty(), "result.text was empty");
}
