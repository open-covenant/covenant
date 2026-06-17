//! Live CLI coverage for the A2A requeue attempt counter surviving daemon
//! restart.
//!
//! `covenant a2a requeue` bumps a task's attempt counter and `covenant a2a
//! status` reports it per entry. `live_cli_a2a_repair` asserts `attempt == 1`
//! after one requeue but never restarts, and `live_restart_a2a` restarts
//! without ever requeuing, so no test proves the counter is durable. On restart
//! the daemon replays `MailboxEvent::TaskRequeued { attempt }` verbatim
//! (`requeue_task` does `attempts.insert(task_id, attempt)`), so the counter
//! must reload from the log rather than reset.
//!
//! This test drives two send/lease/requeue cycles across two restarts: a
//! requeue takes the counter to 1, a restart confirms the replayed entry still
//! reports 1, a second lease+requeue takes it to 2, and a further restart
//! confirms 2. Reaching 2 (not just re-reading 1) proves the value is replayed
//! from the log, not recomputed to a post-lease default.
//!
//! Hermetic and ignored by default. Build the CLI first, then run:
//! `cargo build -p covenant && cargo test -p covenantd --test live_cli_a2a_status_attempt_counter_restart -- --ignored live_`.

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

async fn lease(stream: &mut UnixStream, expect: Uuid) {
    match req(stream, Request::TryRecvA2ATask).await {
        Response::A2ATaskOpt { task: Some(got) } => {
            assert_eq!(got.id, expect, "leased the wrong task")
        }
        other => panic!("expected to lease the task, got {other:?}"),
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

async fn requeue(home: &Path, cli: &Path, task_id: &str, reason: &str) -> Value {
    run_cli_json(
        home,
        cli,
        &[
            "a2a",
            "requeue",
            task_id,
            "--reason",
            reason,
            "--duplicate-risk",
            "idempotent",
        ],
    )
    .await
}

async fn status_entry(home: &Path, cli: &Path, id: &str) -> Value {
    let status = run_cli_json(home, cli, &["a2a", "status", "--limit", "20", "--json"]).await;
    status["tasks"]
        .as_array()
        .expect("tasks array")
        .iter()
        .find(|t| t.pointer("/task/id").and_then(Value::as_str) == Some(id))
        .cloned()
        .unwrap_or_else(|| panic!("task {id} not present in status: {status}"))
}

#[tokio::test]
#[ignore = "live: spawns covenantd, requeues a task twice across two restarts, and asserts the attempt counter replays"]
async fn live_cli_a2a_status_attempt_counter_survives_restart() {
    let home = tempfile::tempdir().expect("tempdir");
    let sock = home.path().join("sock");
    let cli = covenant_cli_bin();

    let peer = AgentId::new("user@local", read_peer_pubkey(home.path()));
    let task = task_for(&peer, "attempt-counter probe");
    let id = task.id.to_string();

    // ── Phase A: send, lease, requeue. lease bumps the counter to 1 and
    //     requeue preserves it, so the requeue outcome reports attempt 1.
    {
        let mut child = spawn_daemon(home.path());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon never created its socket at {}", sock.display());
        }
        let _ = read_operator_token(home.path()).await;

        let mut stream = UnixStream::connect(&sock).await.expect("connect");
        authenticate(&mut stream, home.path()).await;
        grant(&mut stream, format!("a2a.send.{}", peer.display)).await;
        grant(&mut stream, "a2a.repair.requeue").await;

        match req(&mut stream, Request::SendA2ATask { task: task.clone() }).await {
            Response::A2ATaskQueued { task_id } => assert_eq!(task_id, task.id),
            other => panic!("send failed: {other:?}"),
        }
        lease(&mut stream, task.id).await;
        drop(stream);

        let outcome = requeue(home.path(), &cli, &id, "first cycle").await;
        assert_eq!(outcome["action"], "requeued");
        assert_eq!(outcome["state"], "queued");
        assert_eq!(
            outcome["attempt"].as_u64(),
            Some(1),
            "first requeue must report attempt 1",
        );

        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    let _ = std::fs::remove_file(&sock);

    // ── Phase B: restart. The requeued task must replay as queued with the
    //     attempt counter intact, then a second lease+requeue takes it to 2.
    {
        let mut child = spawn_daemon(home.path());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon #2 never created its socket at {}", sock.display());
        }
        let _ = read_operator_token(home.path()).await;

        let entry = status_entry(home.path(), &cli, &id).await;
        assert_eq!(
            entry["state"], "queued",
            "the requeued task must replay as queued after restart",
        );
        assert_eq!(
            entry["attempt"].as_u64(),
            Some(1),
            "attempt 1 must replay from the durable log, not reset",
        );

        let mut stream = UnixStream::connect(&sock).await.expect("connect #2");
        authenticate(&mut stream, home.path()).await;
        lease(&mut stream, task.id).await;
        drop(stream);

        let outcome = requeue(home.path(), &cli, &id, "second cycle").await;
        assert_eq!(
            outcome["attempt"].as_u64(),
            Some(2),
            "second requeue must accumulate to attempt 2",
        );

        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    let _ = std::fs::remove_file(&sock);

    // ── Phase C: restart again. The accumulated attempt 2 must replay, proving
    //     the counter is reloaded from the log rather than recomputed.
    {
        let mut child = spawn_daemon(home.path());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon #3 never created its socket at {}", sock.display());
        }
        let _ = read_operator_token(home.path()).await;

        let entry = status_entry(home.path(), &cli, &id).await;
        assert_eq!(
            entry["state"], "queued",
            "the twice-requeued task must replay as queued after the second restart",
        );
        assert_eq!(
            entry["attempt"].as_u64(),
            Some(2),
            "attempt 2 must replay from the durable log across a second restart",
        );

        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}
