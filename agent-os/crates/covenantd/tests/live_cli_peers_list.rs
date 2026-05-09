//! Live integration test: spawns covenantd against a tempdir HOME with
//! a guest peer pre-seeded in `peers/registry.jsonl`, then runs the
//! real `covenant peers list` CLI as a subprocess and verifies the
//! render path end-to-end across two invocations against the same
//! daemon.
//!
//! Closes the gap between the in-process render-helper unit tests
//! (`crates/covenant/src/main.rs::tests::peer_list_lines_*`) and the
//! CLI binary's `peers list` verb. The unit tests exercise
//! `peer_list_lines` directly with synthesised `PeerSummary` slices;
//! this test exercises the full process boundary — the CLI's argv
//! parsing, IPC handshake (auth via `peers/operator.token`),
//! `Request::ListPeers` round-trip, the daemon's `Server::list_peers`
//! reply with the operator-pubkey sidecar and `truncated` flag, and
//! the binary's stdout rendering.
//!
//! Two invocations, one daemon spawn:
//!
//! (a) `peers list` — default `--limit 20`, two peers in registry
//!     (operator + guest). Asserts both rows visible, `(self)` marker
//!     on the operator's line (Sprint 67 self-marker), no truncation
//!     hint (count fits the limit).
//!
//! (b) `peers list --limit 1` — same registry, smaller limit. Asserts
//!     the truncation hint `(truncated; 1 shown — narrow with
//!     --prefix or raise --limit)` is rendered (Sprint 78 helper),
//!     and the daemon honoured the override (one row, not two).
//!
//! Hermetic — no external services. `#[ignore]`'d. Build prereq:
//! `cargo build -p covenant` (the test panics with a clear message
//! when the CLI binary isn't on disk). Run with
//! `cargo test -p covenantd --test live_cli_peers_list -- --ignored live_`.

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
    // the cross-crate binary lookup in `live_cli_grant_expand.rs` and
    // `live_cli_intent_dispatch.rs`.
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
#[ignore = "live: spawns covenantd + runs `covenant peers list` subprocess"]
async fn live_cli_peers_list_round_trip() {
    let home = tempfile::tempdir().expect("tempdir");

    // ── Pre-seed `peers/registry.jsonl` with one guest peer using a
    //    deterministic pubkey. The daemon's own `JsonlPeerRegistry::open`
    //    replays this on boot, then `bootstrap_operator_token` appends
    //    the operator entry alongside. After replay+bootstrap the
    //    registry contains two live entries: guest (registered first,
    //    by the seed) and operator (registered second, on boot).
    //    `list_summaries` iterates `entries.iter().rev()`, so the
    //    operator row comes first in the wire response — and with
    //    `--limit 1` the truncation branch fires deterministically.
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

    let cli_exe = covenant_cli_bin();

    // ── Invocation (a): default `--limit 20`. Both rows fit; no
    //    truncation hint should appear.
    let cli_out = Command::new(&cli_exe)
        .arg("peers")
        .arg("list")
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI");
    let stdout_a = String::from_utf8_lossy(&cli_out.stdout).to_string();
    let stderr_a = String::from_utf8_lossy(&cli_out.stderr).to_string();
    assert!(
        cli_out.status.success(),
        "(a) CLI exit non-zero: status={:?} stdout={stdout_a:?} stderr={stderr_a:?}",
        cli_out.status,
    );

    // The daemon registers the operator with display `user@local` (see
    // `crates/covenantd/src/main.rs::main` — `LocalIdentity::load_or_create(..,
    // "user@local")`), so the operator row is whichever line contains
    // `user@local`. Find it and assert it also carries the `(self)`
    // marker emitted by `peer_list_lines` when `pubkey == operator_pubkey_b58`.
    let operator_line = stdout_a
        .lines()
        .find(|l| l.contains("user@local"))
        .unwrap_or_else(|| {
            panic!("(a) stdout missing operator row; got stdout={stdout_a:?} stderr={stderr_a:?}");
        });
    assert!(
        operator_line.contains("(self)"),
        "(a) operator line missing `(self)` marker: line={operator_line:?}"
    );
    assert!(
        operator_line.ends_with("\tlive") || operator_line.contains("\tlive"),
        "(a) operator line missing `live` status: line={operator_line:?}"
    );

    // Guest row — find by display, assert pubkey b58 and `live` status
    // both land on the same line. The display alone or the pubkey
    // alone could match a regression that mangles either field; both
    // together pin the row shape end-to-end.
    let guest_line = stdout_a
        .lines()
        .find(|l| l.contains(guest_display))
        .unwrap_or_else(|| {
            panic!("(a) stdout missing guest row; got stdout={stdout_a:?} stderr={stderr_a:?}");
        });
    assert!(
        guest_line.contains(&guest_full_b58),
        "(a) guest line missing pubkey b58 {guest_full_b58:?}: line={guest_line:?}"
    );
    assert!(
        guest_line.contains("\tlive"),
        "(a) guest line missing `live` status: line={guest_line:?}"
    );
    // Guest row must NOT carry `(self)` — only the operator does.
    assert!(
        !guest_line.contains("(self)"),
        "(a) guest line incorrectly carries `(self)` marker: line={guest_line:?}"
    );

    // No truncation hint — two rows fit `--limit 20`. A regression
    // that always emits the hint would fail this assertion before it
    // erodes the operator's trust in the truthy-flag invariant.
    assert!(
        !stdout_a.contains("(truncated;"),
        "(a) stdout unexpectedly contains truncation hint: stdout={stdout_a:?}"
    );

    // ── Invocation (b): `--limit 1` against the same daemon. Two
    //    peers in the registry; `list_summaries` peeks `take(2)`,
    //    reports `truncated: true` and trims to 1 row (the operator,
    //    since `entries.iter().rev()` puts the most-recently-registered
    //    first).
    let cli_out = Command::new(&cli_exe)
        .arg("peers")
        .arg("list")
        .arg("--limit")
        .arg("1")
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI");
    let stdout_b = String::from_utf8_lossy(&cli_out.stdout).to_string();
    let stderr_b = String::from_utf8_lossy(&cli_out.stderr).to_string();
    assert!(
        cli_out.status.success(),
        "(b) CLI exit non-zero: status={:?} stdout={stdout_b:?} stderr={stderr_b:?}",
        cli_out.status,
    );

    // The truncation hint is the load-bearing assertion — a regression
    // that drops the flag from the wire (slice (a)) or from the render
    // (slice (b) Sprint 78) would silently let the operator believe a
    // 1-row response is exhaustive when more peers exist behind it.
    let expected_hint = "(truncated; 1 shown — narrow with --prefix or raise --limit)";
    assert!(
        stdout_b.contains(expected_hint),
        "(b) stdout missing truncation hint {expected_hint:?}; got stdout={stdout_b:?} stderr={stderr_b:?}"
    );

    // Exactly one peer row should render under `--limit 1`. Counting
    // tab-separated rows (each peer-list line carries 4 tabs between
    // the 5 fields) catches a regression that emits the hint AND
    // overshoots the limit.
    let row_lines: Vec<&str> = stdout_b
        .lines()
        .filter(|l| l.matches('\t').count() == 4)
        .collect();
    assert_eq!(
        row_lines.len(),
        1,
        "(b) expected exactly 1 peer row under --limit 1; got rows={row_lines:?}"
    );

    let _ = child.kill().await;
}
