//! Live integration test: the A2A queue-status handler hides one peer's tasks
//! from another over the real daemon socket.
//!
//! `a2a_queue` (covenantd/src/lib.rs) is **not** capability-gated — any
//! authenticated peer may call `Request::A2AQueue` — so the only thing keeping a
//! peer from enumerating every other peer's tasks is the
//! `a2a_entry_visible_to_peer` filter, which admits an entry only when the
//! caller is its sender, recipient, or current lessee. This test pins that
//! filter as a real cross-peer barrier: the operator sends and leases a task,
//! sees it in its own listing, and a seeded non-owner delegate calling the same
//! request sees nothing. Dropping the filter would leak the operator's task id,
//! sender, recipient, intent text, and lease state to the delegate.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_a2a_queue_cross_peer_visibility -- --ignored live_`.

use covenant_a2a::{A2ATask, A2ATaskQueueState};
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::Command;
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

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

async fn authenticated_stream(sock: &Path, token_b58: &str) -> UnixStream {
    let mut stream = UnixStream::connect(sock).await.expect("connect");
    match req(
        &mut stream,
        Request::Authenticate {
            token_b58: token_b58.to_string(),
        },
    )
    .await
    {
        Response::Authenticated { .. } => stream,
        other => panic!("authentication failed: {other:?}"),
    }
}

/// The operator's own pubkey, loaded from the identity the daemon created on
/// startup. The operator authenticates with `operator.token`, which resolves to
/// this identity, so a task it sends names this pubkey as sender/recipient.
fn operator_pubkey(home: &Path) -> [u8; 32] {
    covenant_identity::LocalIdentity::load_or_create(
        &home.join("identity").join("local.key"),
        "user@local",
    )
    .expect("load operator identity")
    .pubkey_bytes()
}

async fn grant(stream: &mut UnixStream, action: String) {
    match req(
        stream,
        Request::GrantCapability {
            action,
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

async fn a2a_queue(stream: &mut UnixStream) -> (Vec<covenant_a2a::A2ATaskQueueEntry>, Vec<covenant_a2a::A2ATaskResult>) {
    match req(
        stream,
        Request::A2AQueue {
            limit: 50,
            min_lease_age_ms: Some(0),
            deadline_within_ms: None,
            state_filter: None,
        },
    )
    .await
    {
        Response::A2AQueue { tasks, results } => (tasks, results),
        other => panic!("expected A2AQueue, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies A2A queue-status hides another peer's tasks"]
async fn live_covenantd_a2a_queue_hides_another_peers_tasks() {
    let home = tempfile::tempdir().expect("tempdir");

    // Seed a non-owner delegate whose pubkey is unrelated to any task the
    // operator sends, so the visibility filter is the only thing that can keep
    // the operator's tasks out of its listing.
    let delegate_token = PeerToken::from_bytes([91u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [92u8; 32];
    {
        let registry = JsonlPeerRegistry::open(home.path().join("peers").join("registry.jsonl"))
            .await
            .expect("open seed registry");
        registry
            .register(PeerEntry {
                token: delegate_token,
                agent_id: AgentId::new("delegate-a2a-queue@local", delegate_pubkey),
                registered_at: 1_700_000_000_000,
            })
            .await
            .expect("seed delegate");
    }

    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(exe)
        .env("COVENANT_HOME", home.path())
        .env("COVENANT_HTTP_PORT", pick_free_port().to_string())
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

    let operator_token = read_operator_token(home.path()).await;
    assert_ne!(
        operator_token, delegate_token_b58,
        "operator and delegate tokens must differ"
    );
    let op_pubkey = operator_pubkey(home.path());
    assert_ne!(
        op_pubkey, delegate_pubkey,
        "operator and delegate pubkeys must differ for this test to be meaningful"
    );

    // ── Phase 1: as the operator, send a task to itself and lease it so it sits
    //     in-flight in the shared queue.
    let mut op = authenticated_stream(&sock, &operator_token).await;
    let operator = AgentId::new("user@local", op_pubkey);
    grant(&mut op, format!("a2a.send.{}", operator.display)).await;

    let task = A2ATask {
        id: Uuid::new_v4(),
        sender: operator.clone(),
        recipient: operator.clone(),
        intent_text: "cross-peer-invisible payload".to_string(),
        task_kind: None,
        parent: None,
        deadline_ms: None,
        idempotency: None,
    };
    match req(&mut op, Request::SendA2ATask { task: task.clone() }).await {
        Response::A2ATaskQueued { task_id } => assert_eq!(task_id, task.id),
        other => panic!("send failed: {other:?}"),
    }
    match req(&mut op, Request::TryRecvA2ATask).await {
        Response::A2ATaskOpt { task: Some(got) } => assert_eq!(got.id, task.id),
        other => panic!("lease failed: {other:?}"),
    }

    // ── Phase 2: positive control — the operator DOES see its own leased task,
    //     so an empty delegate listing below is exclusion, not an empty queue.
    let (op_tasks, _) = a2a_queue(&mut op).await;
    let owned = op_tasks
        .iter()
        .find(|entry| entry.task.id == task.id)
        .expect("operator must see its own leased task in A2AQueue");
    assert_eq!(
        owned.state,
        A2ATaskQueueState::InFlight,
        "the operator's task is leased and must show in-flight"
    );

    // ── Phase 3: as the non-owner delegate, the same request must reveal none of
    //     the operator's task — the visibility filter is the sole barrier on this
    //     ungated read.
    let mut delegate = authenticated_stream(&sock, &delegate_token_b58).await;
    let (del_tasks, del_results) = a2a_queue(&mut delegate).await;
    assert!(
        del_tasks.iter().all(|entry| entry.task.id != task.id),
        "a non-owner delegate must not see the operator's task; leaked entries: {del_tasks:?}"
    );
    assert!(
        del_results.iter().all(|r| r.task_id != task.id),
        "a non-owner delegate must not see the operator's task results; leaked: {del_results:?}"
    );

    // ── Phase 4: the isolation is data-scoped, not a broken session — a bare
    //     Ping still round-trips on the delegate connection.
    match req(&mut delegate, Request::Ping).await {
        Response::Pong => {}
        other => panic!("delegate session must stay usable after a filtered read, got {other:?}"),
    }

    let _ = child.kill().await;
}
