//! Live integration test: spawns covenantd against a tempdir HOME, seeds
//! one audit row by running `covenant capabilities grant tool.call.echo`
//! as a subprocess, then runs `covenant audit recent` and asserts the
//! seeded row's discriminator and payload appear in stdout.
//!
//! Closes the gap between the in-process audit-feed unit tests
//! (`crates/covenantd/src/lib.rs::tests::recent_audit_*`) and the CLI
//! binary's new `audit recent` verb. The unit tests exercise
//! `Server::recent_audit` directly with synthesised events; this test
//! exercises the full process boundary — the CLI's argv parsing, IPC
//! handshake (auth via `peers/operator.token`), `Request::RecentAudit`
//! round-trip, the daemon's per-peer audit-feed filter, and the
//! binary's stdout rendering of `Response::AuditEvents` as JSONL.
//!
//! Two CLI invocations against one daemon spawn:
//!
//! (a) `covenant capabilities grant tool.call.echo` — emits exactly one
//!     `CapabilityGranted` audit row against the operator's pubkey.
//!
//! (b) `covenant audit recent` — reads the row back off the wire and
//!     renders it as a JSON line. Asserts the kind tag
//!     `"capability_granted"` AND the payload `"action":"tool.call.echo"`
//!     appear in stdout. Both halves pin both ends of the round-trip:
//!     the tag alone would let payload mangling slip past, the payload
//!     alone would let kind-tag mangling slip past.
//!
//! Hermetic — no Ollama, no external services. `#[ignore]`'d. Build
//! prereq: `cargo build -p covenant` (the test panics with a clear
//! message when the CLI binary isn't on disk). Run with
//! `cargo test -p covenantd --test live_cli_audit_recent -- --ignored live_`.

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
    // the cross-crate binary lookup in `live_cli_grant_expand.rs`,
    // `live_cli_intent_dispatch.rs`, and `live_cli_peers_list.rs`.
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
#[ignore = "live: spawns covenantd + runs `covenant audit recent` subprocess"]
async fn live_cli_audit_recent_round_trip() {
    let home = tempfile::tempdir().expect("tempdir");

    // ── Spawn the real covenantd binary against the tempdir HOME.
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

    // ── Invocation (a): grant a fixture capability. The action string
    //    `tool.call.echo` is unambiguous and won't collide with any
    //    other audit-row substring. The grant emits exactly one
    //    `CapabilityGranted` audit row whose `kind.action` carries the
    //    same string back.
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

    // ── Invocation (b): read the audit log back as JSONL and assert
    //    the seeded row appears.
    let cli_out = Command::new(&cli_exe)
        .arg("audit")
        .arg("recent")
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI (audit recent)");
    let stdout = String::from_utf8_lossy(&cli_out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&cli_out.stderr).to_string();
    assert!(
        cli_out.status.success(),
        "audit recent CLI exit non-zero: status={:?} stdout={stdout:?} stderr={stderr:?}",
        cli_out.status,
    );

    // The kind discriminator. `AuditKind` serializes with internally-tagged
    // `serde(tag = "type", rename_all = "snake_case")`, so the JSONL row
    // carries `"type":"capability_granted"`. Asserting on the snake_case
    // tag pins the discriminator end-to-end across the wire and the
    // CLI's serialize step.
    assert!(
        stdout.contains(r#""type":"capability_granted""#),
        "stdout missing `capability_granted` kind tag; got stdout={stdout:?} stderr={stderr:?}"
    );

    // The payload. The action string is unambiguous and only present
    // on this one row. Asserting alongside the tag means a regression
    // that mangles the kind discriminator OR the action field will
    // fail the test, instead of a one-sided check that lets one
    // direction's regression slip past.
    let expected_payload = format!(r#""action":"{action}""#);
    assert!(
        stdout.contains(&expected_payload),
        "stdout missing action payload {expected_payload:?}; got stdout={stdout:?} stderr={stderr:?}"
    );

    let _ = child.kill().await;
}
