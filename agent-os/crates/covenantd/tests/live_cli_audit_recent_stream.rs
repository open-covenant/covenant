//! Live integration test: spawns covenantd against a tempdir HOME, seeds
//! one audit row by running `covenant capabilities grant tool.call.echo`
//! as a subprocess, then runs `covenant audit recent --stream` and asserts
//! the seeded row's discriminator and payload appear in stdout.
//!
//! Mirrors [`live_cli_audit_recent`] but exercises the ADR 0010 v2
//! streaming path: the `--stream` flag sets
//! `Request::RecentAudit.prefer_stream = Some(true)`, the daemon answers
//! with a `StreamEnvelope` sequence via `Server::stream_recent_audit`,
//! and the CLI reassembles the chunks via `read_response_or_stream` +
//! `decode_audit_chunks` into the same JSONL output the v1 path
//! produces. Asserting the same kind tag and payload substrings on both
//! paths is the load-bearing equivalence: a regression that mangles
//! either the daemon-side emit or the CLI-side decode would surface as
//! a mismatched audit row here.
//!
//! Hermetic — no Ollama, no external services. `#[ignore]`'d. Build
//! prereq: `cargo build -p covenant` (the test panics with a clear
//! message when the CLI binary isn't on disk). Run with
//! `cargo test -p covenantd --test live_cli_audit_recent_stream -- --ignored live_`.

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
    // the cross-crate binary lookup in `live_cli_audit_recent.rs`.
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
#[ignore = "live: spawns covenantd + runs `covenant audit recent --stream` subprocess against the v2 streaming path"]
async fn live_cli_audit_recent_stream_round_trip() {
    let home = tempfile::tempdir().expect("tempdir");

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

    let cli_exe = covenant_cli_bin();

    // Invocation (a): seed one CapabilityGranted row. The action string
    // is unambiguous and won't collide with any other audit-row
    // substring.
    let action = "tool.call.echo";
    let cli_out = Command::new(&cli_exe)
        .arg("capabilities")
        .arg("grant")
        .arg(action)
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI (grant)");
    let grant_stdout = String::from_utf8_lossy(&cli_out.stdout).to_string();
    let grant_stderr = String::from_utf8_lossy(&cli_out.stderr).to_string();
    assert!(
        cli_out.status.success(),
        "grant CLI exit non-zero: status={:?} stdout={grant_stdout:?} stderr={grant_stderr:?}",
        cli_out.status,
    );

    // Invocation (b): read back via the v2 streaming path. --stream
    // routes Request::RecentAudit through prefer_stream=Some(true) so
    // the daemon emits a StreamEnvelope sequence; the CLI's
    // read_response_or_stream + decode_audit_chunks reassemble it into
    // the same JSONL output the v1 path produced in live_cli_audit_recent.
    let cli_out = Command::new(&cli_exe)
        .arg("audit")
        .arg("recent")
        .arg("--stream")
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI (audit recent --stream)");
    let stdout = String::from_utf8_lossy(&cli_out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&cli_out.stderr).to_string();
    assert!(
        cli_out.status.success(),
        "audit recent --stream CLI exit non-zero: status={:?} stdout={stdout:?} stderr={stderr:?}",
        cli_out.status,
    );

    // Pin the discriminator (snake_case `capability_granted`) and the
    // payload (`action:tool.call.echo`) just like the v1 sibling test.
    // Equivalence of these two substrings across both transports is the
    // load-bearing claim: a streamed path that drops `action` or
    // mangles the kind tag fails the test, instead of a one-sided
    // check that lets a regression in either direction slip past.
    assert!(
        stdout.contains(r#""type":"capability_granted""#),
        "stdout missing `capability_granted` kind tag from streamed feed; got stdout={stdout:?} stderr={stderr:?}"
    );

    let expected_payload = format!(r#""action":"{action}""#);
    assert!(
        stdout.contains(&expected_payload),
        "stdout missing action payload {expected_payload:?} from streamed feed; got stdout={stdout:?} stderr={stderr:?}"
    );

    let _ = child.kill().await;
}
