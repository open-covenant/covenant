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

    // The daemon registers the operator with display `user@local`
    // (`crates/covenantd/src/main.rs::main` — `LocalIdentity::load_or_create(..,
    // "user@local")`). Find that line and assert it carries `(self)`
    // and `live` status.
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
        operator_line.contains("\tlive"),
        "(a) operator line missing `live` status: line={operator_line:?}"
    );

    // Live guest row: present, carries pubkey b58 + `\tlive`, no
    // `(self)`. Both pubkey-and-status together pin the row shape end-
    // to-end — display alone could match a regression that mangles the
    // pubkey field; pubkey alone could match a regression that mangles
    // the display field.
    let live_line = stdout_a
        .lines()
        .find(|l| l.contains(live_display))
        .unwrap_or_else(|| {
            panic!(
                "(a) stdout missing live guest row; got stdout={stdout_a:?} stderr={stderr_a:?}"
            );
        });
    assert!(
        live_line.contains(&live_full_b58),
        "(a) live guest line missing pubkey b58 {live_full_b58:?}: line={live_line:?}"
    );
    assert!(
        live_line.contains("\tlive"),
        "(a) live guest line missing `live` status: line={live_line:?}"
    );
    assert!(
        !live_line.contains("(self)"),
        "(a) live guest line incorrectly carries `(self)` marker: line={live_line:?}"
    );

    // The load-bearing assertion for `--live-only`: the revoked guest
    // must be absent. A regression that inverts the filter or fails to
    // apply it would let `guest-revoked@local` appear in stdout.
    assert!(
        !stdout_a.contains(revoked_display),
        "(a) --live-only must drop revoked guest row; got stdout={stdout_a:?}"
    );
    assert!(
        !stdout_a.contains(&revoked_full_b58),
        "(a) --live-only must drop revoked guest pubkey {revoked_full_b58:?}; got stdout={stdout_a:?}"
    );
    // No `revoked@` status anywhere — a regression that drops the
    // display string but keeps the b58 row would still be caught here.
    assert!(
        !stdout_a.contains("revoked@"),
        "(a) --live-only must not render any `revoked@` status; got stdout={stdout_a:?}"
    );

    // Exactly two peer rows render under `--live-only` (operator +
    // live guest). Counting tab-separated rows (each peer-list line
    // carries 4 tabs between the 5 fields) catches a regression that
    // either over-renders or silently drops a live row.
    let row_lines_a: Vec<&str> = stdout_a
        .lines()
        .filter(|l| l.matches('\t').count() == 4)
        .collect();
    assert_eq!(
        row_lines_a.len(),
        2,
        "(a) expected exactly 2 peer rows under --live-only; got rows={row_lines_a:?}"
    );

    // ── Invocation (b): `peers list --revoked-only`. The filter must
    //    drop both live rows (operator + guest-live) and surface only
    //    the tombstoned guest-revoked row.
    let cli_out = Command::new(&cli_exe)
        .arg("peers")
        .arg("list")
        .arg("--revoked-only")
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

    // The revoked guest row must appear, with `revoked@` in its status
    // column. The render-side helper writes `revoked@<ts>` from the
    // wire's `revoked_at: Some(_)` — a regression that drops the
    // tombstone timestamp from the wire would render `live` here and
    // the assertion would fail.
    let revoked_line = stdout_b
        .lines()
        .find(|l| l.contains(revoked_display))
        .unwrap_or_else(|| {
            panic!(
                "(b) stdout missing revoked guest row; got stdout={stdout_b:?} stderr={stderr_b:?}"
            );
        });
    assert!(
        revoked_line.contains(&revoked_full_b58),
        "(b) revoked guest line missing pubkey b58 {revoked_full_b58:?}: line={revoked_line:?}"
    );
    assert!(
        revoked_line.contains("\trevoked@"),
        "(b) revoked guest line missing `revoked@<ts>` status: line={revoked_line:?}"
    );

    // The load-bearing negative-substring assertions for
    // `--revoked-only`: both live rows (operator + live guest) must be
    // absent. Operator-row absence is the more pointed pin — the
    // operator is always live, so a regression that fails to apply the
    // filter would let `user@local` appear in stdout, which the unit-
    // layer tests can't catch end-to-end.
    assert!(
        !stdout_b.contains("user@local"),
        "(b) --revoked-only must drop operator row; got stdout={stdout_b:?}"
    );
    assert!(
        !stdout_b.contains("(self)"),
        "(b) --revoked-only must not render `(self)` marker; got stdout={stdout_b:?}"
    );
    assert!(
        !stdout_b.contains(live_display),
        "(b) --revoked-only must drop live guest row; got stdout={stdout_b:?}"
    );
    assert!(
        !stdout_b.contains(&live_full_b58),
        "(b) --revoked-only must drop live guest pubkey {live_full_b58:?}; got stdout={stdout_b:?}"
    );

    // Exactly one peer row renders under `--revoked-only` (the
    // tombstoned guest). A regression that lets a live row through
    // would push this count to 2 or 3.
    let row_lines_b: Vec<&str> = stdout_b
        .lines()
        .filter(|l| l.matches('\t').count() == 4)
        .collect();
    assert_eq!(
        row_lines_b.len(),
        1,
        "(b) expected exactly 1 peer row under --revoked-only; got rows={row_lines_b:?}"
    );

    let _ = child.kill().await;
}
