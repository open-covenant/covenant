//! Live integration test for `covenant_tui::ipc::recent_a2a_tasks`.
//!
//! Flow:
//!   1. Grant `a2a.send.<self>` so the operator can send to itself.
//!   2. Send an A2A task addressed to the operator's own identity.
//!   3. Call recent_a2a_tasks via the TUI IPC client and assert the
//!      sent task_id appears.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenant-tui --test live_recent_a2a_tasks -- --ignored live_`.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use covenant_a2a::A2ATask;
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_tui::ipc::{
    grant_capability, read_operator_token, recent_a2a_tasks, A2aFetchOutcome, GrantOutcome,
};
use covenant_types::AgentId;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::time::sleep;
use uuid::Uuid;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
}

fn covenantd_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/covenantd")
        .canonicalize()
        .expect("covenantd binary not built; run `cargo build -p covenantd` first")
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

fn read_peer_pubkey(home: &std::path::Path) -> [u8; 32] {
    let path = home.join("identity").join("local.key");
    let id = covenant_identity::LocalIdentity::load_or_create(&path, "user@local")
        .expect("load identity");
    id.pubkey_bytes()
}

#[tokio::test]
#[ignore = "live: spawns covenantd + sends an A2A task to self and reads it back via recent_a2a_tasks"]
async fn live_recent_a2a_tasks_lists_sent_task() {
    let home = tempfile::tempdir().expect("tempdir");

    let port = pick_free_port();
    let exe = covenantd_bin();
    let mut child = Command::new(&exe)
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

    let pubkey = read_peer_pubkey(home.path());
    let peer = AgentId::new("user@local", pubkey);

    match grant_capability(
        home.path(),
        &format!("a2a.send.{}", peer.display),
        None,
        None,
    )
    .await
    .expect("grant_capability: wire-level error")
    {
        GrantOutcome::Granted { .. } => {}
        GrantOutcome::Failed { message } => panic!("grant failed: {message}"),
    }

    let task = A2ATask {
        id: Uuid::new_v4(),
        sender: peer.clone(),
        recipient: peer.clone(),
        intent_text: "ping".into(),
        task_kind: None,
        parent: None,
        deadline_ms: None,
        idempotency: None,
    };

    let token_b58 = read_operator_token(home.path())
        .await
        .expect("read operator token");
    let mut stream = UnixStream::connect(&sock).await.expect("connect");
    write_frame(&mut stream, &Request::Authenticate { token_b58 })
        .await
        .expect("authenticate frame");
    match read_frame::<_, Response>(&mut stream)
        .await
        .expect("authenticate reply")
    {
        Response::Authenticated { .. } => {}
        other => panic!("authenticate: {other:?}"),
    }
    write_frame(&mut stream, &Request::SendA2ATask { task: task.clone() })
        .await
        .expect("send frame");
    match read_frame::<_, Response>(&mut stream)
        .await
        .expect("send reply")
    {
        Response::A2ATaskQueued { task_id } => assert_eq!(task_id, task.id),
        other => panic!("send: {other:?}"),
    }
    drop(stream);

    let outcome = recent_a2a_tasks(home.path(), 10)
        .await
        .expect("recent_a2a_tasks: wire-level error");
    let tasks = match outcome {
        A2aFetchOutcome::Fetched { tasks } => tasks,
        A2aFetchOutcome::Failed { message } => panic!("recent_a2a_tasks failed: {message}"),
    };
    assert!(
        tasks.iter().any(|t| t.id == task.id),
        "recent_a2a_tasks must list the sent task; got {tasks:?}"
    );

    let _ = child.kill().await;
}
