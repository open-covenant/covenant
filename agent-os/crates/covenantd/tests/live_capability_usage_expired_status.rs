//! Live integration test: the operator capability-usage query reports
//! `effective: expired` once the daemon clock carries a grant past its
//! `expires_at`, and keeps reporting it across a restart.
//!
//! The sibling live test (`live_capability_usage_effective_status.rs`) drives the
//! `exhausted`, `live`, and `revoked` verdicts, but every grant it makes is
//! perpetual (`expires_at: None`), so the `expired` branch — `now > expires_at`
//! against the daemon clock — is only exercised by the in-process unit tests
//! against `verify_with_clock`. This drives it through the real daemon: grant a
//! `tool.call.echo` capability with an expiry a few seconds out, confirm the
//! query first reports it `live`, then poll until the daemon clock passes the
//! expiry and the verdict flips to `expired` — so the flip is shown to be the
//! daemon's own clock-driven decision, not a constructed status.
//!
//! A kill/respawn against the same HOME re-asserts `expired`, showing the verdict
//! is re-derived from the durable grant ledger plus the reloaded daemon clock,
//! not an in-memory value a restart would reset.
//!
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_capability_usage_expired_status -- --ignored live_`.

use covenant_ipc::{
    read_frame, write_frame, CapabilityEffectiveStatus, CapabilityUsageEntry, Request, Response,
};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis() as u64
}

async fn wait_for_sock(path: &Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

async fn read_operator_token(home: &Path) -> String {
    let path = home.join("peers").join("operator.token");
    for _ in 0..50 {
        if let Ok(s) = std::fs::read_to_string(&path) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("operator token never appeared at {}", path.display());
}

async fn authenticate(stream: &mut UnixStream, home: &Path) {
    let token_b58 = read_operator_token(home).await;
    match req(stream, Request::Authenticate { token_b58 }).await {
        Response::Authenticated { .. } => {}
        other => panic!("authenticate failed: {other:?}"),
    }
}

fn spawn_daemon(home: &Path, port: u16) -> Child {
    let exe = env!("CARGO_BIN_EXE_covenantd");
    Command::new(exe)
        .env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd")
}

/// Grants `tool.call.echo` with `expires_at` as the authenticated operator and
/// returns the grant's signature — the usage-query join key.
async fn grant_expiring(stream: &mut UnixStream, expires_at: u64) -> String {
    match req(
        stream,
        Request::GrantCapability {
            action: "tool.call.echo".into(),
            scope: Some(serde_json::json!({ "version": 1, "tool": "echo" })),
            expires_at: Some(expires_at),
        },
    )
    .await
    {
        Response::CapabilityGranted {
            signature_b58,
            action,
            ..
        } => {
            assert_eq!(action, "tool.call.echo");
            signature_b58
        }
        other => panic!("grant tool.call.echo failed: {other:?}"),
    }
}

async fn usage_snapshot(stream: &mut UnixStream) -> Vec<CapabilityUsageEntry> {
    match req(stream, Request::CapabilityUsage).await {
        Response::CapabilityUsage { grants } => grants,
        other => panic!("expected CapabilityUsage, got {other:?}"),
    }
}

/// The grant identified by `signature_b58` — the ed25519 join key — or a panic if
/// the snapshot does not carry it.
fn entry_for<'a>(grants: &'a [CapabilityUsageEntry], signature_b58: &str) -> &'a CapabilityUsageEntry {
    grants
        .iter()
        .find(|g| g.signature_b58 == signature_b58)
        .unwrap_or_else(|| panic!("grant {signature_b58} must appear in the usage snapshot"))
}

/// Polls the usage query until the named grant reports `expired`, returning once
/// it does or panicking if the daemon never flips the verdict within the bound.
/// The expiry is a few seconds out, so the grant must pass through `live` first;
/// a daemon that ignored expiry would stay `live` and trip this panic.
async fn await_expired(stream: &mut UnixStream, signature_b58: &str) {
    for _ in 0..80 {
        let entry = {
            let grants = usage_snapshot(stream).await;
            entry_for(&grants, signature_b58).effective
        };
        match entry {
            CapabilityEffectiveStatus::Expired => return,
            CapabilityEffectiveStatus::Live => sleep(Duration::from_millis(150)).await,
            other => panic!("the expiring grant must flip live -> expired, got {other:?}"),
        }
    }
    panic!("the grant never reported expired within the poll bound");
}

#[tokio::test]
#[ignore = "live: spawns covenantd twice + verifies a real past-expiry grant reports effective expired through the daemon and survives restart"]
async fn live_covenantd_capability_usage_expired_status_across_restart() {
    let home = tempfile::tempdir().expect("tempdir");
    let sock = home.path().join("sock");

    // A few seconds out: comfortably longer than the grant + immediate query
    // round-trip, so the first snapshot is unambiguously `live`, yet short enough
    // to keep the opt-in test quick once the clock passes it.
    let expires_at = now_ms() + 3_000;
    let signature_b58;

    // ── Phase 1: grant with a near-future expiry, confirm it starts `live`, then
    //     poll until the daemon clock carries it past the expiry and the verdict
    //     flips to `expired`.
    {
        let mut child = spawn_daemon(home.path(), pick_free_port());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon #1 never created its socket at {}", sock.display());
        }
        let mut stream = UnixStream::connect(&sock).await.expect("connect #1");
        authenticate(&mut stream, home.path()).await;

        signature_b58 = grant_expiring(&mut stream, expires_at).await;

        // Immediately — well inside the expiry window — the grant is live, so the
        // flip below is a real live -> expired transition, not a grant that was
        // already expired when it was made.
        let grants = usage_snapshot(&mut stream).await;
        let entry = entry_for(&grants, &signature_b58);
        assert_eq!(
            entry.effective,
            CapabilityEffectiveStatus::Live,
            "a freshly granted, not-yet-expired grant reports live",
        );
        assert_eq!(
            entry.expires_at,
            Some(expires_at),
            "the entry reports the expiry it was granted with",
        );

        await_expired(&mut stream, &signature_b58).await;

        drop(stream);
        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    // SIGKILL leaves the socket file behind; remove it so wait_for_sock after
    // daemon #2 is a real readiness signal, not the stale file from #1.
    let _ = std::fs::remove_file(&sock);

    // ── Phase 2: respawn against the same HOME. The expiry is now well in the
    //     past, so the verdict must be re-derived as `expired` from the durable
    //     grant ledger plus the reloaded daemon clock — an in-memory verdict would
    //     not survive the restart.
    {
        let mut child = spawn_daemon(home.path(), pick_free_port());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon #2 never created its socket at {}", sock.display());
        }
        let mut stream = UnixStream::connect(&sock).await.expect("connect #2");
        authenticate(&mut stream, home.path()).await;

        let grants = usage_snapshot(&mut stream).await;
        let entry = entry_for(&grants, &signature_b58);
        assert_eq!(
            entry.effective,
            CapabilityEffectiveStatus::Expired,
            "the expired verdict is re-derived from the durable ledger across restart",
        );
        assert!(!entry.revoked, "the grant expired, it was not revoked");

        drop(stream);
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}
