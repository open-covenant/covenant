//! Live integration test: spawns covenantd against a tempdir HOME
//! with a guest peer pre-seeded in `peers/registry.jsonl`, then runs
//! the real `covenant capabilities grant a2a.send.<prefix>` CLI as a
//! subprocess and verifies that the pubkey-prefix auto-expand lands
//! as the full b58 form on disk.
//!
//! Three assertions:
//!
//! (a) **stderr breadcrumb** — `expanding <prefix> → <full-b58>` is the
//!     operator's only confirmation that the rewrite happened.
//! (b) **stdout grant line** — `granted: ... → a2a.send.<full-b58>`,
//!     full 44-char tail, not the prefix.
//! (c) **on-disk `capabilities/granted.jsonl`** — `capability.action`
//!     equals `a2a.send.<full-b58>`. This is the load-bearing
//!     assertion: subsequent `check_capabilities` reads compare
//!     against the persisted action, so a regression that prints the
//!     full form to stdout but persists the prefix would pass (b)
//!     and silently break enforcement in production.
//!
//! The 16 mock tests in `crates/covenant/src/main.rs` cover the
//! `expand_a2a_action` helper in isolation; this test covers the
//! full process boundary — IPC handshake, ListPeers round-trip,
//! GrantCapability round-trip, and JSONL persistence — when the
//! operator runs the ergonomic shorthand against the binary they
//! actually invoke.
//!
//! Hermetic — no external services. `#[ignore]`'d. Build prereq:
//! `cargo build -p covenant` (the test panics with a clear message
//! when the CLI binary isn't on disk). Run with
//! `cargo test -p covenantd --test live_cli_grant_expand -- --ignored live_`.

use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_permissions::SignedCapability;
use covenant_types::AgentId;
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
    // the cross-crate binary lookup in `live_agent_dispatch.rs`.
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
#[ignore = "live: spawns covenantd + runs `covenant capabilities grant` subprocess"]
async fn live_cli_capabilities_grant_expands_pubkey_prefix() {
    let home = tempfile::tempdir().expect("tempdir");

    // ── Pre-seed `peers/registry.jsonl` with a guest peer using a
    //    deterministic pubkey (`[42u8; 32]`). The daemon's own
    //    `JsonlPeerRegistry::open` replays this on boot, then
    //    `bootstrap_operator_token` appends the operator entry
    //    alongside.
    let guest_pubkey = [42u8; 32];
    let guest_full_b58 = bs58::encode(guest_pubkey).into_string();
    let guest_token = PeerToken::generate();
    let guest_display = "guest@local";
    let guest_entry = PeerEntry {
        token: guest_token,
        agent_id: AgentId::new(guest_display, guest_pubkey),
        registered_at: 1_700_000_000_000,
    };
    let registry_path = home.path().join("peers").join("registry.jsonl");
    {
        let registry = JsonlPeerRegistry::open(registry_path.clone())
            .await
            .expect("open seed registry");
        registry
            .register(guest_entry.clone())
            .await
            .expect("seed guest");
    }

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

    // ── A 12-char prefix of the guest b58 is collision-safe against
    //    the daemon-minted operator pubkey (1/58^12 ≈ 10^-21). The
    //    fixture has exactly two peers (operator + guest); the prefix
    //    will match guest only, so `expand_a2a_action` returns
    //    `Rewritten`.
    let prefix: String = guest_full_b58.chars().take(12).collect();
    let action_arg = format!("a2a.send.{prefix}");
    let expected_full_action = format!("a2a.send.{guest_full_b58}");

    let cli_exe = covenant_cli_bin();
    let cli_out = Command::new(&cli_exe)
        .arg("capabilities")
        .arg("grant")
        .arg(&action_arg)
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

    // (a) stderr breadcrumb — operator's only confirmation that the
    //     rewrite happened. Shape is `expanding <prefix> → <full-action>`,
    //     where the full action is the entire rewritten string
    //     (`a2a.send.<full-b58>`), not just the b58 tail.
    let breadcrumb = format!("expanding {prefix} → {expected_full_action}");
    assert!(
        stderr.contains(&breadcrumb),
        "stderr missing expanding breadcrumb {breadcrumb:?}; got {stderr:?}"
    );

    // (b) stdout grant line — the action printed must be the full b58
    //     form (44-char tail, not the prefix).
    assert!(
        stdout.contains(&format!("→ {expected_full_action}")),
        "stdout missing full-form grant line; got {stdout:?}"
    );

    // (c) On-disk `granted.jsonl` carries the full b58. This is the
    //     load-bearing assertion — subsequent `check_capabilities`
    //     reads compare against this row, so a regression that
    //     printed the full form to stdout but persisted the prefix
    //     would pass (b) and silently break enforcement.
    let granted_path = home.path().join("capabilities").join("granted.jsonl");
    let granted_text = std::fs::read_to_string(&granted_path).expect("read granted.jsonl");
    let lines: Vec<&str> = granted_text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "expected exactly one grant line; got {lines:?}"
    );
    let signed: SignedCapability =
        serde_json::from_str(lines[0]).expect("parse SignedCapability JSONL");
    assert_eq!(
        signed.capability.action, expected_full_action,
        "persisted action must be the expanded full b58 form, not the prefix"
    );

    let _ = child.kill().await;
}
