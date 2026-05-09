//! Live integration test: spawns covenantd against a tempdir HOME, seeds
//! one granted capability by running `covenant capabilities grant
//! tool.call.echo --expires-at <ms>` as a subprocess, then runs `covenant
//! capabilities recent` and asserts the seeded row's action and the
//! `expires <ms>` bracket render appear in stdout.
//!
//! Sibling of `live_cli_capabilities_recent.rs` — that test pins the
//! `expires_at: None` branch (`[perpetual]`); this one pins the
//! `Some(ms)` branch (`[expires <ms>]`). Together they cover both arms
//! of the CLI's `match c.capability.expires_at` render in
//! `crates/covenant/src/main.rs::"capabilities" => "recent"`.
//!
//! The expiry value `9_999_999_999_999` is well past any realistic
//! current time (~year 2286) and therefore a near-unique substring in
//! the daemon's stdout — a regression that mangled the action field
//! into the bracket position would not produce this digit string by
//! accident.
//!
//! Two CLI invocations against one daemon spawn:
//!
//! (a) `covenant capabilities grant tool.call.echo --expires-at
//!     9999999999999` — signs and persists exactly one
//!     `SignedCapability` whose `capability.action` is `tool.call.echo`
//!     and whose `capability.expires_at` is `Some(9999999999999)`.
//!     The daemon's `grant_capability` does not reject past or future
//!     expirations at sign time (verification is the consumer's job),
//!     so the seed value is rendered unchanged on the read side.
//!
//! (b) `covenant capabilities recent` — reads the row back off the wire
//!     and renders it as `<subject> → <action> (<granted_by>) [<expiry>]`.
//!     Asserts stdout contains `→ tool.call.echo` (anchors the action
//!     to the render's arrow position) AND `[expires 9999999999999]`
//!     (anchors the `Some(ms)` branch to the bracket position with the
//!     full epoch ms, defending against a regression that dropped the
//!     `expires ` prefix or truncated the digit string).
//!
//! Hermetic — no Ollama, no external services. `#[ignore]`'d. Build
//! prereq: `cargo build -p covenant` (the test panics with a clear
//! message when the CLI binary isn't on disk). Run with
//! `cargo test -p covenantd --test live_cli_capabilities_grant_with_expiry -- --ignored live_`.

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
    // `live_cli_intent_dispatch.rs`, `live_cli_peers_list.rs`,
    // `live_cli_audit_recent.rs`, and `live_cli_capabilities_recent.rs`.
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
#[ignore = "live: spawns covenantd + runs `covenant capabilities grant --expires-at` subprocess"]
async fn live_cli_capabilities_grant_with_expiry_round_trip() {
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

    // ── Invocation (a): grant a fixture capability with a far-future
    //    expiry. `9_999_999_999_999` (~year 2286) is past any plausible
    //    `epoch_ms()` reading the daemon could produce and therefore a
    //    near-unique substring in stdout — a regression that swapped
    //    fields would not produce this digit string by accident.
    let action = "tool.call.echo";
    let expires_at_ms: u64 = 9_999_999_999_999;
    let expires_str = expires_at_ms.to_string();
    let cli_out = Command::new(&cli_exe)
        .arg("capabilities")
        .arg("grant")
        .arg(action)
        .arg("--expires-at")
        .arg(&expires_str)
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI (grant --expires-at)");
    let grant_stdout = String::from_utf8_lossy(&cli_out.stdout).to_string();
    let grant_stderr = String::from_utf8_lossy(&cli_out.stderr).to_string();
    assert!(
        cli_out.status.success(),
        "grant CLI exit non-zero: status={:?} stdout={grant_stdout:?} stderr={grant_stderr:?}",
        cli_out.status,
    );

    // ── Invocation (b): read the granted-capability set back and assert
    //    the seeded row appears with the expected render shape.
    let cli_out = Command::new(&cli_exe)
        .arg("capabilities")
        .arg("recent")
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI (capabilities recent)");
    let stdout = String::from_utf8_lossy(&cli_out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&cli_out.stderr).to_string();
    assert!(
        cli_out.status.success(),
        "capabilities recent CLI exit non-zero: status={:?} stdout={stdout:?} stderr={stderr:?}",
        cli_out.status,
    );

    // The action lives between the arrow and the granted-by bracket in
    // the CLI's `{subject} → {action} ({granted_by}) [{expiry}]` render
    // (`crates/covenant/src/main.rs::"capabilities" => "recent"` arm).
    // Anchoring on `→ tool.call.echo` pins the action to its render
    // position — a regression that swapped action and granted_by would
    // emit `→ user@local` and fail this assert.
    let arrow_action = format!("→ {action}");
    assert!(
        stdout.contains(&arrow_action),
        "stdout missing arrow + action {arrow_action:?}; got stdout={stdout:?} stderr={stderr:?}"
    );

    // The `expires_at: Some(ms)` branch renders as `expires {ms}` inside
    // the trailing bracket. Anchoring on `[expires 9999999999999]` pins
    // the bracket position, the `expires ` prefix, AND the full digit
    // string. A regression that dropped the prefix (rendered just `[ms]`)
    // or truncated the integer (e.g., a `format!("{:.10}", …)` accident)
    // fails here. Sibling `live_cli_capabilities_recent.rs` pins the
    // `[perpetual]` branch.
    let expires_render = format!("[expires {expires_str}]");
    assert!(
        stdout.contains(&expires_render),
        "stdout missing {expires_render:?}; got stdout={stdout:?} stderr={stderr:?}"
    );

    let _ = child.kill().await;
}
