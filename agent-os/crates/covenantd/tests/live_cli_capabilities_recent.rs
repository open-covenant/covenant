//! Live integration test: spawns covenantd against a tempdir HOME, seeds
//! one granted capability by running `covenant capabilities grant
//! tool.call.echo` as a subprocess, then runs `covenant capabilities
//! recent` and asserts the seeded row's action and `perpetual` expiry
//! render appear in stdout.
//!
//! Closes the gap between the in-process `recent_capabilities_*` unit
//! tests in `crates/covenantd/src/lib.rs` (which exercise
//! `Server::recent_capabilities` directly with synthesised
//! `SignedCapability` rows) and the CLI binary's existing `capabilities
//! recent` verb. The unit tests pin the daemon-side filter; this test
//! pins the full process boundary — the CLI's argv parsing, IPC
//! handshake (auth via `peers/operator.token`), `Request::GrantCapability`
//! and `Request::RecentCapabilities` round-trips, the daemon's
//! per-subject capability store, and the CLI's stdout rendering of
//! `Response::Capabilities`.
//!
//! Two CLI invocations against one daemon spawn:
//!
//! (a) `covenant capabilities grant tool.call.echo` — signs and persists
//!     exactly one `SignedCapability` whose `capability.action` is
//!     `tool.call.echo` and whose `capability.expires_at` is `None`.
//!
//! (b) `covenant capabilities recent` — reads the row back off the wire
//!     and renders it as `<subject> → <action> (<granted_by>) [<expiry>]`.
//!     Asserts stdout contains `→ tool.call.echo` (anchors the action to
//!     the render's arrow position) AND `[perpetual]` (anchors the
//!     `expires_at: None` branch to the bracket position). Both halves
//!     pin both ends of the round-trip: the arrow assert alone would let
//!     a regression that rendered every cap with an `expires N` bracket
//!     slip past, and the `[perpetual]` assert alone would let a
//!     regression that mangled the action field slip past.
//!
//! Hermetic — no Ollama, no external services. `#[ignore]`'d. Build
//! prereq: `cargo build -p covenant` (the test panics with a clear
//! message when the CLI binary isn't on disk). Run with
//! `cargo test -p covenantd --test live_cli_capabilities_recent -- --ignored live_`.

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
    // `live_cli_intent_dispatch.rs`, `live_cli_peers_list.rs`, and
    // `live_cli_audit_recent.rs`.
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
#[ignore = "live: spawns covenantd + runs `covenant capabilities recent` subprocess"]
async fn live_cli_capabilities_recent_round_trip() {
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

    // ── Invocation (a): grant a fixture capability with no expiry. The
    //    action `tool.call.echo` is unambiguous and won't collide with
    //    any other rendered substring. Omitting `--expires-at` exercises
    //    the `expires_at: None` branch on the daemon's signing path AND
    //    on the CLI's `[perpetual]` render — the orthogonal axis the
    //    second assertion below pins.
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

    // The `expires_at: None` branch renders as the literal `perpetual`
    // inside the trailing bracket. Anchoring on `[perpetual]` pins both
    // the bracket position and the None render. A regression that
    // emitted a numeric epoch ms (or dropped the bracket) fails here.
    assert!(
        stdout.contains("[perpetual]"),
        "stdout missing `[perpetual]` expiry render; got stdout={stdout:?} stderr={stderr:?}"
    );

    let _ = child.kill().await;
}
