//! Live integration test: covenantd restart resilience for the A2A mailbox.
//!
//! Spawns the real binary against a tempdir HOME, sends a task, kills the
//! daemon, respawns against the same HOME, then verifies that
//!
//! 1. the queued task survived the restart (`try_recv_task` returns it),
//! 2. the senders map survived the restart (`PostA2AResult` requires the
//!    sender-scoped `a2a.respond.<sender>` cap whose subject is the
//!    original sender of the task — only knowable if the senders map
//!    replayed correctly),
//! 3. the result lands and can be drained.
//!
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_restart_a2a -- --ignored live_`.
//!
//! Hermetic — no external services. Uses two different HTTP ports to
//! avoid bind contention while the OS releases the first.

use covenant_a2a::{A2ATask, A2ATaskResult};
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_mcp::Content;
use covenant_types::AgentId;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;
use uuid::Uuid;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
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

#[tokio::test]
#[ignore = "live: spawns covenantd twice + verifies JsonlMailbox replay across restart"]
async fn live_covenantd_a2a_survives_daemon_restart() {
    let home = tempfile::tempdir().expect("tempdir");
    let sock = home.path().join("sock");

    // ── Phase 1: daemon #1 — grant cap, send a task, then kill the
    //     process abruptly so the test exercises the on-disk replay path
    //     rather than any clean-shutdown drain logic.

    let task = A2ATask {
        id: Uuid::new_v4(),
        sender: AgentId::new("orch@local", [0u8; 32]),
        recipient: AgentId::new("research@local", [0u8; 32]),
        intent_text: "find recent papers".into(),
        parent: None,
        deadline_ms: None,
    };

    {
        let mut child = spawn_daemon(home.path(), pick_free_port());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon #1 never created its socket at {}", sock.display());
        }

        let mut stream = UnixStream::connect(&sock).await.expect("connect #1");
        authenticate(&mut stream, home.path()).await;

        match req(
            &mut stream,
            Request::GrantCapability {
                action: format!("a2a.send.{}", task.recipient.display),
                scope: None,
                expires_at: None,
            },
        )
        .await
        {
            Response::CapabilityGranted { .. } => {}
            other => panic!("grant a2a.send failed: {other:?}"),
        }

        match req(&mut stream, Request::SendA2ATask { task: task.clone() }).await {
            Response::A2ATaskQueued { task_id } => assert_eq!(task_id, task.id),
            other => panic!("expected A2ATaskQueued, got {other:?}"),
        }

        // The queued task is visible via the read-only recent endpoint
        // before the kill — sanity check that what we expect to replay is
        // actually on disk.
        match req(&mut stream, Request::RecentA2ATasks { limit: 10 }).await {
            Response::A2ATasks { tasks } => {
                assert_eq!(tasks.len(), 1);
                assert_eq!(tasks[0].id, task.id);
            }
            other => panic!("expected A2ATasks, got {other:?}"),
        }

        drop(stream);
        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    // SIGKILL leaves the socket file behind. Remove it so the
    // `wait_for_sock` after daemon #2 spawn is a real readiness signal
    // and not the stale file from #1. The daemon's own startup code
    // would also handle this, but doing it here keeps the test's
    // observation of "sock exists" aligned with "daemon #2 is listening".
    let _ = std::fs::remove_file(&sock);

    // The on-disk events.jsonl must contain the TaskSent event the
    // replay is supposed to read.
    let events_path = home.path().join("a2a").join("events.jsonl");
    assert!(
        events_path.exists(),
        "expected JsonlMailbox event log at {}",
        events_path.display()
    );
    let events = std::fs::read_to_string(&events_path).expect("read events.jsonl");
    assert!(
        events.contains("\"task_sent\""),
        "events.jsonl missing task_sent line: {events}"
    );

    // The granted capability must also persist — the rejection-after-restart
    // assertion below depends on send still being authorised.
    let granted_path = home.path().join("capabilities").join("granted.jsonl");
    assert!(
        granted_path.exists(),
        "expected capabilities log at {}",
        granted_path.display()
    );

    // ── Phase 2: daemon #2 — same HOME, different port. The mailbox
    //     should replay the queued task and the senders map.

    {
        let mut child = spawn_daemon(home.path(), pick_free_port());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon #2 never created its socket at {}", sock.display());
        }

        let mut stream = UnixStream::connect(&sock).await.expect("connect #2");
        authenticate(&mut stream, home.path()).await;

        // Replay assertion #1: the queued task is recv'able.
        match req(&mut stream, Request::TryRecvA2ATask).await {
            Response::A2ATaskOpt { task: Some(t) } => {
                assert_eq!(t.id, task.id, "replayed task id mismatch");
                assert_eq!(t.sender.display, "orch@local");
                assert_eq!(t.recipient.display, "research@local");
                assert_eq!(t.intent_text, "find recent papers");
            }
            other => panic!("expected replayed task, got {other:?}"),
        }

        // Replay assertion #2: the senders map replayed. Posting a result
        // for the task must require `a2a.respond.orch@local` — the sender
        // is only knowable if the TaskSent event was applied at open().
        let result = A2ATaskResult::ok(task.id, vec![Content::text("done")]);
        match req(
            &mut stream,
            Request::PostA2AResult {
                result: result.clone(),
            },
        )
        .await
        {
            Response::Error { message } => {
                assert!(
                    message.contains("a2a.respond.orch@local"),
                    "rejection should name the replayed sender-scoped cap: {message}"
                );
            }
            other => panic!("expected Error before grant, got {other:?}"),
        }

        // Replay assertion #3: the senders map kept the entry across the
        // recv above, so granting the sender-scoped cap and posting now
        // succeeds.
        match req(
            &mut stream,
            Request::GrantCapability {
                action: format!("a2a.respond.{}", task.sender.display),
                scope: None,
                expires_at: None,
            },
        )
        .await
        {
            Response::CapabilityGranted { .. } => {}
            other => panic!("grant a2a.respond failed: {other:?}"),
        }

        match req(&mut stream, Request::PostA2AResult { result }).await {
            Response::A2AResultPosted { task_id } => assert_eq!(task_id, task.id),
            other => panic!("expected A2AResultPosted, got {other:?}"),
        }

        match req(&mut stream, Request::TryRecvA2AResult).await {
            Response::A2AResultOpt { result: Some(r) } => {
                assert_eq!(r.task_id, task.id);
                assert_eq!(r.status, covenant_a2a::A2ATaskStatus::Ok);
            }
            other => panic!("expected queued result, got {other:?}"),
        }

        drop(stream);
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}
