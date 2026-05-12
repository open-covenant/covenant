//! Live CLI coverage for operator-driven A2A repair.
//!
//! Hermetic and ignored by default. Build the CLI first, then run with:
//! `cargo build -p covenant && cargo test -p covenantd --test live_cli_a2a_repair -- --ignored live_`.

use covenant_a2a::A2ATask;
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_types::AgentId;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
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

async fn authenticate(stream: &mut UnixStream, home: &Path) {
    let token_b58 = read_operator_token(home).await;
    match req(stream, Request::Authenticate { token_b58 }).await {
        Response::Authenticated { .. } => {}
        other => panic!("authenticate failed: {other:?}"),
    }
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

fn task(peer: &AgentId, text: &str) -> A2ATask {
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

async fn send_and_lease(stream: &mut UnixStream, task: &A2ATask) -> Uuid {
    match req(stream, Request::SendA2ATask { task: task.clone() }).await {
        Response::A2ATaskQueued { task_id } => assert_eq!(task_id, task.id),
        other => panic!("send failed: {other:?}"),
    }
    match req(stream, Request::TryRecvA2ATask).await {
        Response::A2ATaskOpt { task: Some(got) } => assert_eq!(got.id, task.id),
        other => panic!("lease failed: {other:?}"),
    }
    match req(
        stream,
        Request::A2AQueue {
            limit: 20,
            min_lease_age_ms: Some(0),
            deadline_within_ms: None,
        },
    )
    .await
    {
        Response::A2AQueue { tasks, .. } => tasks
            .into_iter()
            .find(|entry| entry.task.id == task.id)
            .and_then(|entry| entry.lease_id)
            .expect("leased task entry with lease id"),
        other => panic!("queue lookup failed: {other:?}"),
    }
}

async fn run_cli_raw(home: &Path, cli: &Path, args: &[&str]) -> Output {
    Command::new(cli)
        .args(args)
        .env("COVENANT_HOME", home)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI")
}

async fn run_cli(home: &Path, cli: &Path, args: &[&str]) -> String {
    let out = run_cli_raw(home, cli, args).await;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "CLI failed: args={args:?} status={:?} stdout={stdout:?} stderr={stderr:?}",
        out.status,
    );
    stdout
}

fn status_task(stdout: &str, task_id: Uuid) -> Value {
    let wanted = task_id.to_string();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let value: Value = serde_json::from_str(line).expect("status line json");
        if value.get("type").and_then(Value::as_str) == Some("task")
            && value
                .pointer("/entry/task/id")
                .and_then(Value::as_str)
                .is_some_and(|id| id == wanted)
        {
            return value["entry"].clone();
        }
    }
    panic!("missing task {wanted} in CLI status stdout: {stdout}");
}

fn status_result(stdout: &str, task_id: Uuid) -> Value {
    let wanted = task_id.to_string();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let value: Value = serde_json::from_str(line).expect("status line json");
        if value.get("type").and_then(Value::as_str) == Some("result")
            && value
                .pointer("/result/task_id")
                .and_then(Value::as_str)
                .is_some_and(|id| id == wanted)
        {
            return value["result"].clone();
        }
    }
    panic!("missing result {wanted} in CLI status stdout: {stdout}");
}

#[tokio::test]
#[ignore = "live: spawns covenantd + drives A2A repair through real CLI subprocesses"]
async fn live_cli_a2a_repair_round_trip() {
    let home = tempfile::tempdir().expect("tempdir");
    let sock = home.path().join("sock");
    let cli = covenant_cli_bin();
    let mut child = spawn_daemon(home.path());

    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon never created its socket at {}", sock.display());
    }

    let mut stream = UnixStream::connect(&sock).await.expect("connect");
    authenticate(&mut stream, home.path()).await;

    let peer = AgentId::new("user@local", read_peer_pubkey(home.path()));
    grant(&mut stream, format!("a2a.send.{}", peer.display)).await;
    grant(&mut stream, "a2a.repair.requeue").await;
    grant(&mut stream, "a2a.repair.force_error").await;

    let requeue_task = task(&peer, "live cli requeue");
    let requeue_lease = send_and_lease(&mut stream, &requeue_task).await;
    let status = run_cli(
        home.path(),
        &cli,
        &["a2a", "status", "--min-lease-age-ms", "0"],
    )
    .await;
    let entry = status_task(&status, requeue_task.id);
    assert_eq!(entry["state"], "in_flight");

    let requeue_task_id = requeue_task.id.to_string();
    let requeue_lease_id = requeue_lease.to_string();
    assert_eq!(entry["lease_id"].as_str(), Some(requeue_lease_id.as_str()));

    let stale_lease_id = Uuid::nil().to_string();
    assert_ne!(stale_lease_id, requeue_lease_id);
    let rejected = run_cli_raw(
        home.path(),
        &cli,
        &[
            "a2a",
            "requeue",
            &requeue_task_id,
            "--lease-id",
            &stale_lease_id,
            "--reason",
            "live cli stale lease guard",
            "--duplicate-risk",
            "idempotent",
        ],
    )
    .await;
    let rejected_stdout = String::from_utf8_lossy(&rejected.stdout);
    let rejected_stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(
        !rejected.status.success(),
        "stale lease repair must fail: status={:?} stdout={rejected_stdout:?} stderr={rejected_stderr:?}",
        rejected.status
    );
    assert!(
        rejected_stdout.trim().is_empty(),
        "failed stale lease repair must not emit a success outcome: {rejected_stdout:?}"
    );
    assert!(
        rejected_stderr.contains("lease mismatch")
            && rejected_stderr.contains(&requeue_task_id)
            && rejected_stderr.contains(&stale_lease_id)
            && rejected_stderr.contains(&requeue_lease_id),
        "stale lease repair should be operator-classifiable: {rejected_stderr:?}"
    );

    let status = run_cli(
        home.path(),
        &cli,
        &["a2a", "status", "--min-lease-age-ms", "0"],
    )
    .await;
    let entry = status_task(&status, requeue_task.id);
    assert_eq!(entry["state"], "in_flight");
    assert_eq!(entry["lease_id"].as_str(), Some(requeue_lease_id.as_str()));

    let outcome = run_cli(
        home.path(),
        &cli,
        &[
            "a2a",
            "requeue",
            &requeue_task_id,
            "--lease-id",
            &requeue_lease_id,
            "--reason",
            "live cli repair test",
            "--duplicate-risk",
            "idempotent",
        ],
    )
    .await;
    let outcome: Value = serde_json::from_str(outcome.trim()).expect("requeue outcome json");
    assert_eq!(outcome["action"], "requeued");
    assert_eq!(outcome["state"], "queued");
    assert_eq!(outcome["attempt"], 1);

    let status = run_cli(home.path(), &cli, &["a2a", "status"]).await;
    let entry = status_task(&status, requeue_task.id);
    assert_eq!(entry["state"], "queued");
    assert_eq!(entry["attempt"], 1);

    match req(&mut stream, Request::TryRecvA2ATask).await {
        Response::A2ATaskOpt { task: Some(got) } => assert_eq!(got.id, requeue_task.id),
        other => panic!("requeued task was not receivable: {other:?}"),
    }

    let error_task = task(&peer, "live cli force error");
    let error_lease = send_and_lease(&mut stream, &error_task).await;
    let status = run_cli(
        home.path(),
        &cli,
        &["a2a", "status", "--min-lease-age-ms", "0"],
    )
    .await;
    assert_eq!(status_task(&status, error_task.id)["state"], "in_flight");

    let error_task_id = error_task.id.to_string();
    let error_lease_id = error_lease.to_string();
    let outcome = run_cli(
        home.path(),
        &cli,
        &[
            "a2a",
            "force-error",
            &error_task_id,
            "--lease-id",
            &error_lease_id,
            "--reason",
            "live cli repair test",
            "--message",
            "operator forced stale lease failure",
        ],
    )
    .await;
    let outcome: Value = serde_json::from_str(outcome.trim()).expect("force-error outcome json");
    assert_eq!(outcome["action"], "forced_error");
    assert_eq!(outcome["state"], "result_pending");
    assert_eq!(outcome["result"]["status"], "error");
    assert_eq!(
        outcome["result"]["error_message"],
        "operator forced stale lease failure"
    );

    let status = run_cli(home.path(), &cli, &["a2a", "status"]).await;
    let result = status_result(&status, error_task.id);
    assert_eq!(result["status"], "error");
    assert_eq!(
        result["error_message"],
        "operator forced stale lease failure"
    );

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
