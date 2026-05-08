//! Live integration test: spawns covenantd against a tempdir HOME with
//! no agents registered, then runs the real `covenant intent "<text>"`
//! CLI as a subprocess and verifies the echo-fallback round-trip.
//!
//! Closes the gap between the in-process intent test
//! (`crates/covenantd/src/lib.rs::tests::submit_intent_falls_back_to_echo_when_no_match`)
//! and the CLI binary's `intent` verb. The mock test exercises
//! `Server::respond` directly; this test exercises the full process
//! boundary — the CLI's argv parsing, IPC handshake (auth via
//! `peers/operator.token`), `Request::SubmitIntent` round-trip, and
//! the binary's stdout rendering of `Response::IntentResult`.
//!
//! The fallback path is hermetic: `phase 0 echo (no agent matched):
//! <text>` is emitted by the daemon when the intent router finds no
//! agent whose keyword score beats the threshold. With the tempdir
//! HOME containing no `agents/<id>/agent.toml`, that branch fires
//! deterministically — no Ollama, no model warmup, no external
//! services needed. Same hermetic posture as `live_cli_grant_expand.rs`.
//!
//! `#[ignore]`'d. Build prereq: `cargo build -p covenant` (the test
//! panics with a clear message when the CLI binary isn't on disk).
//! Run with
//! `cargo test -p covenantd --test live_cli_intent_dispatch -- --ignored live_`.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
}

fn covenant_cli_bin() -> PathBuf {
    // CARGO_MANIFEST_DIR for this crate is `crates/covenantd`; the CLI
    // binary lives at workspace `target/<profile>/covenant`. Mirrors
    // the cross-crate binary lookup in `live_cli_grant_expand.rs` and
    // `live_agent_dispatch.rs`.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/covenant")
        .canonicalize()
        .expect("covenant CLI binary not built; run `cargo build -p covenant` first")
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

async fn wait_for_operator_token(home: &std::path::Path) {
    let path = home.join("peers").join("operator.token");
    for _ in 0..50 {
        if let Ok(s) = std::fs::read_to_string(&path) {
            if !s.trim().is_empty() {
                return;
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("operator token never appeared at {}", path.display());
}

#[tokio::test]
#[ignore = "live: spawns covenantd + runs `covenant intent` subprocess"]
async fn live_cli_intent_dispatch_round_trip() {
    let home = tempfile::tempdir().expect("tempdir");

    // ── Spawn the real covenantd binary against the tempdir HOME with
    //    no agents registered. The intent router has no candidates, so
    //    `Server::respond` takes the echo-fallback branch.
    let port = pick_free_port();
    let daemon_exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(daemon_exe)
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
    wait_for_operator_token(home.path()).await;

    // ── Run `covenant intent "<text>"` as a subprocess. The CLI joins
    //    argv[1..] with spaces, so a multi-word intent travels as one
    //    `Request::SubmitIntent { text }` frame. The daemon's echo
    //    branch returns `phase 0 echo (no agent matched): <text>` and
    //    the CLI prints `IntentResult.text` to stdout.
    let intent_text = "hello from the cli";
    let cli_exe = covenant_cli_bin();
    let cli_out = Command::new(&cli_exe)
        .arg("intent")
        .arg(intent_text)
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI");
    let stdout = String::from_utf8_lossy(&cli_out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&cli_out.stderr).to_string();
    assert!(
        cli_out.status.success(),
        "CLI exit non-zero: status={:?} stdout={stdout:?} stderr={stderr:?}",
        cli_out.status
    );

    // The full echo-fallback string carries both the marker and the
    // operator's text — asserting on the full shape catches a
    // regression where the CLI starts dropping/reformatting either
    // half. The marker alone would let "phase 0 echo (no agent
    // matched): <some other text>" pass; the text alone would let a
    // raw passthrough that bypassed the daemon pass.
    let expected = format!("phase 0 echo (no agent matched): {intent_text}");
    assert!(
        stdout.contains(&expected),
        "stdout missing echo-fallback line {expected:?}; got stdout={stdout:?} stderr={stderr:?}"
    );

    let _ = child.kill().await;
}
