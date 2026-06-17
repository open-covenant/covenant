//! Live CLI coverage for the `covenant a2a status --min-lease-age-ms` filter
//! across a daemon restart.
//!
//! The min-lease-age filter keeps only in-flight tasks whose lease age
//! (`now_ms - leased_at_ms`) is at least the threshold. Sibling filters are
//! exercised at runtime (`live_cli_a2a_status_state_filter`,
//! `live_cli_a2a_status_deadline_filter`) and the restart path is exercised
//! with every filter disabled (`live_restart_a2a` passes `min_lease_age_ms:
//! None`), so nothing proves the filter still discriminates after the daemon
//! reloads the queue from its event log. On restart the daemon replays
//! `MailboxEvent::TaskLeased { leased_at_ms }` verbatim, so a lease's age must
//! be measured from the ORIGINAL lease time against the new daemon clock — not
//! reset to restart time.
//!
//! This test leases a task, captures its `leased_at_ms`, restarts the daemon,
//! and asserts (1) `leased_at_ms` replays verbatim (deterministic) and (2) the
//! aged lease passes a small `--min-lease-age-ms` but is excluded by a
//! far-future one. Assertion (1) catches a reset regardless of timing;
//! assertion (2) proves the filter ages the replayed timestamp against the
//! live clock.
//!
//! Hermetic and ignored by default. Build the CLI first, then run:
//! `cargo build -p covenant && cargo test -p covenantd --test live_cli_a2a_status_min_lease_age_restart -- --ignored live_`.

use covenant_a2a::A2ATask;
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_types::AgentId;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;
use uuid::Uuid;

fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().unwrap().port()
}

fn covenant_cli_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/covenant")
        .canonicalize()
        .expect("covenant CLI binary not built; run `cargo build -p covenant` first")
}

fn spawn_daemon(home: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_covenantd"))
        .env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", pick_free_port().to_string())
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd")
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

async fn read_operator_token(home: &Path) -> String {
    let path = home.join("peers").join("operator.token");
    for _ in 0..50 {
        if let Ok(raw) = std::fs::read_to_string(&path) {
            let token = raw.trim();
            if !token.is_empty() {
                return token.to_string();
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("operator token never appeared at {}", path.display());
}

fn read_peer_pubkey(home: &Path) -> [u8; 32] {
    let id = covenant_identity::LocalIdentity::load_or_create(
        &home.join("identity").join("local.key"),
        "user@local",
    )
    .expect("load identity");
    id.pubkey_bytes()
}

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

async fn authenticate(stream: &mut UnixStream, home: &Path) {
    let token_b58 = read_operator_token(home).await;
    match req(stream, Request::Authenticate { token_b58 }).await {
        Response::Authenticated { .. } => {}
        other => panic!("authenticate failed: {other:?}"),
    }
}

async fn grant(stream: &mut UnixStream, action: impl Into<String>) {
    match req(
        stream,
        Request::GrantCapability {
            action: action.into(),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { .. } => {}
        other => panic!("grant failed: {other:?}"),
    }
}

fn task_for(peer: &AgentId, text: &str) -> A2ATask {
    A2ATask {
        id: Uuid::new_v4(),
        sender: peer.clone(),
        recipient: peer.clone(),
        intent_text: text.to_string(),
        task_kind: None,
        parent: None,
        deadline_ms: None,
        idempotency: None,
    }
}

async fn run_cli_json(home: &Path, cli: &Path, args: &[&str]) -> Value {
    let out = Command::new(cli)
        .args(args)
        .env("COVENANT_HOME", home)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "CLI failed: args={args:?} status={:?} stdout={stdout:?} stderr={stderr:?}",
        out.status,
    );
    serde_json::from_str(stdout.trim()).expect("CLI stdout is a single JSON object")
}

fn entry_ids(value: &Value) -> Vec<String> {
    value["tasks"]
        .as_array()
        .expect("tasks array")
        .iter()
        .filter_map(|t| t.pointer("/task/id").and_then(Value::as_str))
        .map(String::from)
        .collect()
}

fn entry<'a>(value: &'a Value, id: &str) -> Option<&'a Value> {
    value["tasks"]
        .as_array()
        .expect("tasks array")
        .iter()
        .find(|t| t.pointer("/task/id").and_then(Value::as_str) == Some(id))
}

#[tokio::test]
#[ignore = "live: spawns covenantd, leases a task, restarts, and asserts --min-lease-age-ms ages the replayed lease"]
async fn live_cli_a2a_status_min_lease_age_filter_survives_restart() {
    let home = tempfile::tempdir().expect("tempdir");
    let sock = home.path().join("sock");
    let cli = covenant_cli_bin();

    let pubkey = read_peer_pubkey(home.path());
    let peer = AgentId::new("user@local", pubkey);
    let task = task_for(&peer, "lease-age probe");
    let id = task.id.to_string();

    // ── Phase 1: spawn, lease the task so leased_at_ms is stamped, capture it.
    let pre_leased_at = {
        let mut child = spawn_daemon(home.path());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon never created its socket at {}", sock.display());
        }
        let _ = read_operator_token(home.path()).await;

        let mut stream = UnixStream::connect(&sock).await.expect("connect");
        authenticate(&mut stream, home.path()).await;
        grant(&mut stream, format!("a2a.send.{}", peer.display)).await;

        match req(&mut stream, Request::SendA2ATask { task: task.clone() }).await {
            Response::A2ATaskQueued { task_id } => assert_eq!(task_id, task.id),
            other => panic!("send failed: {other:?}"),
        }
        match req(&mut stream, Request::TryRecvA2ATask).await {
            Response::A2ATaskOpt { task: Some(got) } => assert_eq!(got.id, task.id),
            other => panic!("expected to lease the task, got {other:?}"),
        }
        drop(stream);

        // min-lease-age 0 admits any in-flight lease, so this reads back the
        // freshly stamped leased_at_ms regardless of how young the lease is.
        let pre = run_cli_json(
            home.path(),
            &cli,
            &["a2a", "status", "--limit", "20", "--min-lease-age-ms", "0", "--json"],
        )
        .await;
        let leased_at = entry(&pre, &id)
            .and_then(|e| e["leased_at_ms"].as_u64())
            .expect("leased task must expose leased_at_ms before restart");

        // Hold the lease long enough that its age comfortably exceeds the
        // small post-restart threshold (1500ms) no matter how fast the restart
        // is — the small-threshold inclusion below must not be flaky.
        sleep(Duration::from_millis(2500)).await;

        let _ = child.kill().await;
        let _ = child.wait().await;
        leased_at
    };

    // SIGKILL leaves the socket file behind; remove it so the post-restart
    // wait_for_sock is a real readiness signal for daemon #2.
    let _ = std::fs::remove_file(&sock);

    // ── Phase 2: same HOME, fresh daemon. The lease replays from events.jsonl.
    let mut child = spawn_daemon(home.path());
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon #2 never created its socket at {}", sock.display());
    }
    let _ = read_operator_token(home.path()).await;

    // Deterministic replay proof: leased_at_ms must be the original lease time,
    // not re-stamped to restart time. This catches a reset independent of any
    // wall-clock timing.
    let post = run_cli_json(
        home.path(),
        &cli,
        &["a2a", "status", "--limit", "20", "--min-lease-age-ms", "0", "--json"],
    )
    .await;
    let post_entry = entry(&post, &id).expect("lease must survive the restart");
    assert_eq!(
        post_entry["state"].as_str(),
        Some("in_flight"),
        "the replayed lease must still be in_flight",
    );
    assert_eq!(
        post_entry["leased_at_ms"].as_u64(),
        Some(pre_leased_at),
        "leased_at_ms must replay verbatim, not reset to restart time",
    );

    // Filter discrimination over the replayed timestamp: the lease is older
    // than the 2500ms hold plus restart latency, so a small threshold admits
    // it. A reset leased_at_ms would make the age ~0 and wrongly exclude it.
    let small = run_cli_json(
        home.path(),
        &cli,
        &["a2a", "status", "--limit", "20", "--min-lease-age-ms", "1500", "--json"],
    )
    .await;
    assert_eq!(small["min_lease_age_ms"].as_u64(), Some(1500));
    assert!(
        entry_ids(&small).contains(&id),
        "the aged lease must pass a 1500ms min-lease-age after restart, proving its age is measured from the original leased_at_ms; got {:?}",
        entry_ids(&small),
    );

    // A far-future threshold (1h) exceeds the lease age, so it must be excluded
    // — the filter is genuinely applied, not bypassed on the replayed entry.
    let huge = run_cli_json(
        home.path(),
        &cli,
        &["a2a", "status", "--limit", "20", "--min-lease-age-ms", "3600000", "--json"],
    )
    .await;
    assert_eq!(huge["min_lease_age_ms"].as_u64(), Some(3_600_000));
    assert!(
        !entry_ids(&huge).contains(&id),
        "a lease younger than 1h must be excluded under --min-lease-age-ms 3600000; got {:?}",
        entry_ids(&huge),
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
