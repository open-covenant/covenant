//! Live integration test: spawns covenantd against a tempdir HOME
//! pre-seeded with one live guest peer and one revoked guest peer in
//! `peers/registry.jsonl`, then runs the real `covenant peers list`
//! CLI as a subprocess with `--live-only` and `--revoked-only` to
//! verify the status-filter end-to-end across two invocations against
//! the same daemon spawn.
//!
//! Closes the gap between the in-process registry-level filter tests
//! (`crates/covenant-peer-auth/src/lib.rs::tests::list_summaries_status_filter_*`)
//! and the CLI binary's `peers list --live-only` / `--revoked-only`
//! flags. The unit tests exercise the filter at the registry layer
//! with synthesised `PeerEntry`/`PeerSummary` slices; this test
//! exercises the full process boundary — argv parsing, the
//! `peers_list_status_filter` mutual-exclusion helper, IPC handshake
//! (auth via `peers/operator.token`), `Request::ListPeers` round-trip
//! with the new `status_filter` field on the wire, the daemon's
//! `Server::list_peers` call into `list_summaries`, and the binary's
//! stdout rendering.
//!
//! Two invocations, one daemon spawn:
//!
//! (a) `peers list --live-only` — registry has two live entries
//!     (operator + guest-live) and one tombstoned entry
//!     (guest-revoked). Asserts both live rows visible (operator with
//!     `(self)`, guest-live with `\tlive`), the revoked row is NOT
//!     visible, and the operator row's `(self)` marker is preserved.
//!
//! (b) `peers list --revoked-only` — same registry. Asserts the
//!     revoked guest row is the only row visible (carries `revoked@`
//!     status), the operator row is NOT visible (operator is live, so
//!     `--revoked-only` drops it), and the live guest row is NOT
//!     visible.
//!
//! Hermetic — no external services. `#[ignore]`'d. Build prereq:
//! `cargo build -p covenant` (the test panics with a clear message
//! when the CLI binary isn't on disk). Run with
//! `cargo test -p covenantd --test live_cli_peers_list_status_filter -- --ignored live_`.

use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
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
#[ignore = "live: spawns covenantd + runs `covenant peers list --live-only|--revoked-only` subprocess"]
async fn live_cli_peers_list_status_filter_round_trip() {
    let home = tempfile::tempdir().expect("tempdir");

    // ── Pre-seed `peers/registry.jsonl` with two guest peers via the
    //    registry's own type-checked API: `register` for both, then
    //    `revoke` for the second token. The daemon's own
    //    `JsonlPeerRegistry::open` replays the resulting event log on
    //    boot and `bootstrap_operator_token` appends the operator entry
    //    alongside, so after replay the registry holds three entries:
    //    operator (live, registered last), guest-live (live, registered
    //    first by the seed), and guest-revoked (tombstoned by the seed).
    //
    //    Hand-crafting JSONL lines for the register+revoke events would
    //    be fragile across event-log shape changes; the type-checked
    //    `register` / `revoke` methods future-proof the seed against
    //    such changes.
    let live_pubkey = [42u8; 32];
    let live_full_b58 = bs58::encode(live_pubkey).into_string();
    let live_token = PeerToken::generate();
    let live_display = "guest-live@local";
    let live_entry = PeerEntry {
        token: live_token,
        agent_id: AgentId::new(live_display, live_pubkey),
        registered_at: 1_700_000_000_000,
    };

    let revoked_pubkey = [43u8; 32];
    let revoked_full_b58 = bs58::encode(revoked_pubkey).into_string();
    let revoked_token = PeerToken::generate();
    let revoked_display = "guest-revoked@local";
    let revoked_entry = PeerEntry {
        token: revoked_token,
        agent_id: AgentId::new(revoked_display, revoked_pubkey),
        registered_at: 1_700_000_001_000,
    };

    let registry_path = home.path().join("peers").join("registry.jsonl");
    {
        let registry = JsonlPeerRegistry::open(registry_path.clone())
            .await
            .expect("open seed registry");
        registry
            .register(live_entry.clone())
            .await
            .expect("seed live guest");
        registry
            .register(revoked_entry.clone())
            .await
            .expect("seed revoked guest");
        let revoked = registry
            .revoke(&revoked_token)
            .await
            .expect("revoke seed guest");
        assert!(
            revoked,
            "seed revoke must report a live binding existed before this call"
        );
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

    let cli_exe = covenant_cli_bin();

    // ── Invocation (a): `peers list --live-only`. Registry has two
    //    live rows (operator + guest-live) and one tombstoned row
    //    (guest-revoked). The filter must drop the tombstoned row and
    //    keep the two live rows.
    let cli_out = Command::new(&cli_exe)
        .arg("peers")
        .arg("list")
        .arg("--live-only")
        .arg("--json")
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI --live-only");
    let stdout_a = String::from_utf8_lossy(&cli_out.stdout).to_string();
    let stderr_a = String::from_utf8_lossy(&cli_out.stderr).to_string();
    assert!(
        cli_out.status.success(),
        "(a) CLI exit non-zero: status={:?} stdout={stdout_a:?} stderr={stderr_a:?}",
        cli_out.status,
    );

    let value_a: serde_json::Value =
        serde_json::from_str(stdout_a.trim()).expect("(a) peers list --json must be valid JSON");
    assert_eq!(
        value_a["kind"].as_str(),
        Some("peer_list"),
        "(a) kind mismatch: {value_a:?}"
    );
    let peers_a = value_a["peers"]
        .as_array()
        .expect("(a) peers list JSON must include peers array");
    assert_eq!(
        peers_a.len(),
        2,
        "(a) expected exactly 2 peer rows under --live-only; got peers={peers_a:?}"
    );
    assert!(
        peers_a.iter().all(|peer| peer["revoked_at"].is_null()),
        "(a) --live-only must not include revoked peers: peers={peers_a:?}"
    );
    let operator_peer = peers_a
        .iter()
        .find(|peer| peer["agent_id"]["display"].as_str() == Some("user@local"))
        .expect("(a) peers list must include operator row");
    assert!(
        operator_peer["agent_id"]["pubkey"].as_str().is_some(),
        "(a) operator row must include pubkey b58: peer={operator_peer:?}"
    );
    let live_peer = peers_a
        .iter()
        .find(|peer| peer["agent_id"]["display"].as_str() == Some(live_display))
        .expect("(a) peers list must include live guest row");
    assert_eq!(
        live_peer["agent_id"]["pubkey"].as_str(),
        Some(live_full_b58.as_str()),
        "(a) live guest pubkey mismatch: peer={live_peer:?}"
    );
    assert!(
        peers_a
            .iter()
            .all(|peer| peer["agent_id"]["display"].as_str() != Some(revoked_display)),
        "(a) --live-only must drop revoked guest row: peers={peers_a:?}"
    );

    // ── Invocation (b): `peers list --revoked-only`. The filter must
    //    drop both live rows (operator + guest-live) and surface only
    //    the tombstoned guest-revoked row.
    let cli_out = Command::new(&cli_exe)
        .arg("peers")
        .arg("list")
        .arg("--revoked-only")
        .arg("--json")
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI --revoked-only");
    let stdout_b = String::from_utf8_lossy(&cli_out.stdout).to_string();
    let stderr_b = String::from_utf8_lossy(&cli_out.stderr).to_string();
    assert!(
        cli_out.status.success(),
        "(b) CLI exit non-zero: status={:?} stdout={stdout_b:?} stderr={stderr_b:?}",
        cli_out.status,
    );

    let value_b: serde_json::Value =
        serde_json::from_str(stdout_b.trim()).expect("(b) peers list --json must be valid JSON");
    assert_eq!(
        value_b["kind"].as_str(),
        Some("peer_list"),
        "(b) kind mismatch: {value_b:?}"
    );
    let peers_b = value_b["peers"]
        .as_array()
        .expect("(b) peers list JSON must include peers array");
    assert_eq!(
        peers_b.len(),
        1,
        "(b) expected exactly 1 peer row under --revoked-only; got peers={peers_b:?}"
    );
    let revoked_peer = &peers_b[0];
    assert_eq!(
        revoked_peer["agent_id"]["display"].as_str(),
        Some(revoked_display),
        "(b) revoked-only must return revoked guest row: peer={revoked_peer:?}"
    );
    assert_eq!(
        revoked_peer["agent_id"]["pubkey"].as_str(),
        Some(revoked_full_b58.as_str()),
        "(b) revoked guest pubkey mismatch: peer={revoked_peer:?}"
    );
    assert!(
        !revoked_peer["revoked_at"].is_null(),
        "(b) revoked guest must carry revoked_at timestamp: peer={revoked_peer:?}"
    );

    let _ = child.kill().await;
}
