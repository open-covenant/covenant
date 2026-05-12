//! Live CLI coverage for the `covenant a2a status --deadline-within-ms`
//! filter. Sends three queued tasks against the same operator-as-self
//! peer: one urgent (deadline within the filter window), one far (well
//! outside the window), one with no deadline at all. The filter must
//! keep only the urgent row.
//!
//! Hermetic and ignored by default. Build the CLI first, then run with:
//! `cargo build -p covenant && cargo test -p covenantd --test live_cli_a2a_status_deadline_filter -- --ignored live_`.

use covenant_a2a::A2ATask;
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_types::AgentId;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is post-epoch")
        .as_millis() as u64
}

fn task_with(peer: &AgentId, text: &str, deadline_ms: Option<u64>) -> A2ATask {
    A2ATask {
        id: Uuid::new_v4(),
        sender: peer.clone(),
        recipient: peer.clone(),
        intent_text: text.to_string(),
        task_kind: None,
        parent: None,
        deadline_ms,
        idempotency: None,
    }
}

async fn send_task(stream: &mut UnixStream, task: &A2ATask) {
    match req(stream, Request::SendA2ATask { task: task.clone() }).await {
        Response::A2ATaskQueued { task_id } => assert_eq!(task_id, task.id),
        other => panic!("send failed: {other:?}"),
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

#[tokio::test]
#[ignore = "live: spawns covenantd + sends three a2a tasks and asserts --deadline-within-ms keeps only urgent ones"]
async fn live_cli_a2a_status_deadline_filter_keeps_urgent_only() {
    let home = tempfile::tempdir().expect("tempdir");

    let mut child = spawn_daemon(home.path());
    let sock = home.path().join("sock");
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon never created its socket at {}", sock.display());
    }
    let _ = read_operator_token(home.path()).await;

    let pubkey = read_peer_pubkey(home.path());
    let peer = AgentId::new("user@local", pubkey);

    let mut stream = UnixStream::connect(&sock).await.expect("connect");
    authenticate(&mut stream, home.path()).await;
    grant(&mut stream, format!("a2a.send.{}", peer.display)).await;

    let issued_at = now_ms();
    let urgent = task_with(&peer, "urgent", Some(issued_at + 1_000));
    let far = task_with(&peer, "far", Some(issued_at + 300_000));
    let no_deadline = task_with(&peer, "no-deadline", None);

    send_task(&mut stream, &urgent).await;
    send_task(&mut stream, &far).await;
    send_task(&mut stream, &no_deadline).await;
    drop(stream);

    let cli = covenant_cli_bin();
    let unfiltered = run_cli_json(
        home.path(),
        &cli,
        &["a2a", "status", "--limit", "20", "--json"],
    )
    .await;
    let unfiltered_ids: Vec<String> = unfiltered["tasks"]
        .as_array()
        .expect("unfiltered tasks array")
        .iter()
        .filter_map(|t| t.pointer("/task/id").and_then(Value::as_str))
        .map(String::from)
        .collect();
    assert!(
        unfiltered_ids.iter().any(|id| id == &urgent.id.to_string())
            && unfiltered_ids.iter().any(|id| id == &far.id.to_string())
            && unfiltered_ids
                .iter()
                .any(|id| id == &no_deadline.id.to_string()),
        "without the deadline filter, all three sent tasks must be visible; got {unfiltered_ids:?}",
    );

    let filtered = run_cli_json(
        home.path(),
        &cli,
        &[
            "a2a",
            "status",
            "--limit",
            "20",
            "--deadline-within-ms",
            "60000",
            "--json",
        ],
    )
    .await;
    assert_eq!(filtered["kind"], "a2a_status");
    assert_eq!(filtered["deadline_within_ms"], 60_000);
    assert!(
        filtered["min_lease_age_ms"].is_null(),
        "min_lease_age_ms must remain null when only the deadline filter is active; got {:?}",
        filtered["min_lease_age_ms"]
    );

    let filtered_ids: Vec<String> = filtered["tasks"]
        .as_array()
        .expect("filtered tasks array")
        .iter()
        .filter_map(|t| t.pointer("/task/id").and_then(Value::as_str))
        .map(String::from)
        .collect();
    assert!(
        filtered_ids.iter().any(|id| id == &urgent.id.to_string()),
        "urgent task within the 60s window must survive the filter; got {filtered_ids:?}",
    );
    assert!(
        !filtered_ids.iter().any(|id| id == &far.id.to_string()),
        "far task outside the 60s window must be filtered out; got {filtered_ids:?}",
    );
    assert!(
        !filtered_ids.iter().any(|id| id == &no_deadline.id.to_string()),
        "no-deadline task must be filtered out under an active deadline filter; got {filtered_ids:?}",
    );

    let _ = child.kill().await;
}
