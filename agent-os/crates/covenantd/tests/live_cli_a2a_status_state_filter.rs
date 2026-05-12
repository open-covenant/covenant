//! Live CLI coverage for the `covenant a2a status --state` filter.
//! Sends two queued tasks against the same operator-as-self peer,
//! then leases one of them to flip its `A2ATaskQueueState` to
//! `InFlight`. The filter must keep only the matching state under
//! each `--state` value and the JSON envelope must echo the active
//! `state_filter` so machine consumers can distinguish a state-only
//! result from a pre-filter empty result.
//!
//! Hermetic and ignored by default. Build the CLI first, then run:
//! `cargo build -p covenant && cargo test -p covenantd --test live_cli_a2a_status_state_filter -- --ignored live_`.

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

async fn send_task(stream: &mut UnixStream, task: &A2ATask) {
    match req(stream, Request::SendA2ATask { task: task.clone() }).await {
        Response::A2ATaskQueued { task_id } => assert_eq!(task_id, task.id),
        other => panic!("send failed: {other:?}"),
    }
}

async fn lease_one(stream: &mut UnixStream) -> Uuid {
    match req(stream, Request::TryRecvA2ATask).await {
        Response::A2ATaskOpt { task: Some(got) } => got.id,
        other => panic!("expected to lease one task, got {other:?}"),
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

fn entry_state(value: &Value, id: &str) -> Option<String> {
    value["tasks"]
        .as_array()
        .expect("tasks array")
        .iter()
        .find(|t| t.pointer("/task/id").and_then(Value::as_str) == Some(id))
        .and_then(|t| t.get("state").and_then(Value::as_str))
        .map(String::from)
}

#[tokio::test]
#[ignore = "live: spawns covenantd, sends two a2a tasks, leases one, and asserts --state queued|in_flight narrows correctly"]
async fn live_cli_a2a_status_state_filter_narrows_each_state() {
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

    let queued = task_for(&peer, "queued probe");
    let in_flight = task_for(&peer, "in-flight probe");

    send_task(&mut stream, &in_flight).await;
    let leased_id = lease_one(&mut stream).await;
    assert_eq!(
        leased_id, in_flight.id,
        "TryRecvA2ATask must lease the only queued task; got {leased_id} expected {}",
        in_flight.id,
    );

    send_task(&mut stream, &queued).await;
    drop(stream);

    let cli = covenant_cli_bin();

    let unfiltered = run_cli_json(
        home.path(),
        &cli,
        &["a2a", "status", "--limit", "20", "--json"],
    )
    .await;
    assert_eq!(unfiltered["kind"], "a2a_status");
    assert!(
        unfiltered["state_filter"].is_null(),
        "state_filter must be null when --state is not passed; got {:?}",
        unfiltered["state_filter"]
    );
    let unfiltered_ids = entry_ids(&unfiltered);
    assert!(
        unfiltered_ids.iter().any(|id| id == &queued.id.to_string())
            && unfiltered_ids
                .iter()
                .any(|id| id == &in_flight.id.to_string()),
        "without --state, both queued and in-flight tasks must be visible; got {unfiltered_ids:?}",
    );
    assert_eq!(
        entry_state(&unfiltered, &queued.id.to_string()).as_deref(),
        Some("queued"),
        "queued task must report state=queued",
    );
    assert_eq!(
        entry_state(&unfiltered, &in_flight.id.to_string()).as_deref(),
        Some("in_flight"),
        "leased task must report state=in_flight",
    );

    let queued_only = run_cli_json(
        home.path(),
        &cli,
        &[
            "a2a", "status", "--limit", "20", "--state", "queued", "--json",
        ],
    )
    .await;
    assert_eq!(queued_only["kind"], "a2a_status");
    assert_eq!(
        queued_only["state_filter"], "queued",
        "JSON envelope must echo the active state_filter for machine consumers",
    );
    let queued_only_ids = entry_ids(&queued_only);
    assert!(
        queued_only_ids.iter().any(|id| id == &queued.id.to_string()),
        "queued task must survive --state queued; got {queued_only_ids:?}",
    );
    assert!(
        !queued_only_ids
            .iter()
            .any(|id| id == &in_flight.id.to_string()),
        "in-flight task must be filtered out under --state queued; got {queued_only_ids:?}",
    );

    let in_flight_only = run_cli_json(
        home.path(),
        &cli,
        &[
            "a2a",
            "status",
            "--limit",
            "20",
            "--state",
            "in_flight",
            "--json",
        ],
    )
    .await;
    assert_eq!(in_flight_only["kind"], "a2a_status");
    assert_eq!(
        in_flight_only["state_filter"], "in_flight",
        "JSON envelope must echo --state in_flight back to the operator",
    );
    let in_flight_only_ids = entry_ids(&in_flight_only);
    assert!(
        in_flight_only_ids
            .iter()
            .any(|id| id == &in_flight.id.to_string()),
        "leased task must survive --state in_flight; got {in_flight_only_ids:?}",
    );
    assert!(
        !in_flight_only_ids
            .iter()
            .any(|id| id == &queued.id.to_string()),
        "queued task must be filtered out under --state in_flight; got {in_flight_only_ids:?}",
    );

    let typo = Command::new(&cli)
        .args(["a2a", "status", "--state", "Queued", "--json"])
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI for typo");
    assert!(
        !typo.status.success(),
        "case-sensitive --state must reject 'Queued' so a typo cannot silently disable the filter",
    );
    let stderr = String::from_utf8_lossy(&typo.stderr);
    assert!(
        stderr.contains("unknown a2a state"),
        "stderr must explain the rejection: {stderr:?}",
    );

    let _ = child.kill().await;
}
