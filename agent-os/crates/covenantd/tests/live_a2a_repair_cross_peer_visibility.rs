//! Live integration test: the A2A repair handler refuses to repair a task the
//! caller is not a party to, even when the caller holds an UNSCOPED repair grant.
//!
//! `repair_a2a_task` (covenantd/src/lib.rs) checks the `a2a.repair.<action>`
//! capability, then filters the queue by `a2a_entry_visible_to_peer`, then runs
//! the per-task scope check. An UNSCOPED grant (`scope {}`) clears the scope
//! check — `a2a_scope_allows` returns `Ok(true)` for an empty object — and the
//! mailbox `repair_task` checks only the in-flight lease, never peer ownership.
//! So for a non-owner holding an unscoped grant, the visibility filter is the
//! sole barrier stopping it from requeuing another peer's in-flight task. This is
//! DISTINCT from `live_covenantd_a2a_repair_rejects_peer_mismatched_delegation`,
//! which grants a SCOPED capability pointed at the wrong counterparty and is
//! caught earlier by the scope check — a different gate.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_a2a_repair_cross_peer_visibility -- --ignored live_`.

use covenant_a2a::{
    A2ADuplicateRisk, A2ARepairCommand, A2ARepairRequest, A2ATask, A2ATaskQueueState,
};
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

fn operator_pubkey(home: &Path) -> [u8; 32] {
    covenant_identity::LocalIdentity::load_or_create(
        &home.join("identity").join("local.key"),
        "user@local",
    )
    .expect("load operator identity")
    .pubkey_bytes()
}

fn requeue(task_id: Uuid) -> Request {
    Request::RepairA2ATask {
        request: A2ARepairRequest {
            task_id,
            command: A2ARepairCommand::Requeue {
                // The delegate does not know (and cannot see) the operator's
                // lease; None skips the lease check, so if the visibility filter
                // were gone the requeue would actually succeed.
                lease_id: None,
                duplicate_risk: A2ADuplicateRisk::Idempotent,
            },
            reason: "cross-peer repair visibility probe".into(),
        },
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies A2A repair refuses a non-owner with an unscoped grant"]
async fn live_covenantd_a2a_repair_refuses_unscoped_non_owner() {
    let home = tempfile::tempdir().expect("tempdir");

    // Seed a non-owner delegate — neither sender, recipient, nor lessee of any
    // task the operator creates, so the visibility filter is the only barrier.
    let delegate_token = PeerToken::from_bytes([81u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [82u8; 32];
    {
        let registry = JsonlPeerRegistry::open(home.path().join("peers").join("registry.jsonl"))
            .await
            .expect("open seed registry");
        registry
            .register(PeerEntry {
                token: delegate_token,
                agent_id: AgentId::new("delegate-a2a-repair-vis@local", delegate_pubkey),
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
    let op_pubkey = operator_pubkey(home.path());
    assert_ne!(
        op_pubkey, delegate_pubkey,
        "operator and delegate pubkeys must differ for this test to be meaningful"
    );

    // ── Phase 1: the operator sends a task to itself and leases it in-flight.
    let mut op = authenticated_stream(&sock, &operator_token).await;
    let operator = AgentId::new("user@local", op_pubkey);
    match req(
        &mut op,
        Request::GrantCapability {
            action: format!("a2a.send.{}", operator.display),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { .. } => {}
        other => panic!("operator send grant failed: {other:?}"),
    }
    let task = A2ATask {
        id: Uuid::new_v4(),
        sender: operator.clone(),
        recipient: operator.clone(),
        intent_text: "operator-owned in-flight task".to_string(),
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
    let original_lease = match req(
        &mut op,
        Request::A2AQueue {
            limit: 10,
            min_lease_age_ms: Some(0),
            deadline_within_ms: None,
            state_filter: None,
        },
    )
    .await
    {
        Response::A2AQueue { tasks, .. } => tasks
            .into_iter()
            .find(|entry| entry.task.id == task.id)
            .and_then(|entry| entry.lease_id)
            .expect("operator's leased task carries a lease id"),
        other => panic!("operator queue lookup failed: {other:?}"),
    };

    let mut delegate = authenticated_stream(&sock, &delegate_token_b58).await;

    // ── Phase 2: with NO grant, the capability gate fires first — proving the
    //     visibility refusal below is not just a missing-capability denial.
    match req(&mut delegate, requeue(task.id)).await {
        Response::Error { message } => assert!(
            message.contains("a2a.repair.requeue"),
            "a delegate without a grant must be refused by the capability gate, got {message:?}"
        ),
        other => panic!("expected a capability denial, got {other:?}"),
    }

    // ── Phase 3: the delegate self-grants an UNSCOPED a2a.repair.requeue. The
    //     empty scope clears the scope check (a2a_scope_allows -> Ok(true)), so
    //     only the visibility filter can stop the repair.
    match req(
        &mut delegate,
        Request::GrantCapability {
            action: "a2a.repair.requeue".into(),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { action, .. } => assert_eq!(action, "a2a.repair.requeue"),
        other => panic!("delegate self-grant failed: {other:?}"),
    }

    // ── Phase 4: the repair is STILL refused — by the visibility filter, not the
    //     capability gate (Phase 2) and not the scope check (which an unscoped
    //     grant clears). The message must name the visibility layer specifically.
    match req(&mut delegate, requeue(task.id)).await {
        Response::Error { message } => {
            assert!(
                message.contains("is not visible to the authenticated peer"),
                "an unscoped non-owner must be refused by the visibility filter, got {message:?}"
            );
            assert!(
                !message.contains("a2a.repair.requeue"),
                "the refusal must be the visibility message, not the capability message: {message:?}"
            );
            assert!(
                !message.contains("capability scope"),
                "the refusal must be the visibility message, not the scope message: {message:?}"
            );
        }
        other => panic!("a non-owner must not repair the operator's task, got {other:?}"),
    }

    // ── Phase 5: the rejected repair mutated nothing — the operator's task is
    //     still in-flight under the same lease. If the visibility filter were
    //     gone, the unscoped grant + lease_id:None would have requeued it.
    match req(
        &mut op,
        Request::A2AQueue {
            limit: 10,
            min_lease_age_ms: Some(0),
            deadline_within_ms: None,
            state_filter: None,
        },
    )
    .await
    {
        Response::A2AQueue { tasks, .. } => {
            let entry = tasks
                .iter()
                .find(|entry| entry.task.id == task.id)
                .expect("operator's task must survive the rejected cross-peer repair");
            assert_eq!(
                entry.state,
                A2ATaskQueueState::InFlight,
                "the task must stay in-flight, not be requeued by a non-owner"
            );
            assert_eq!(
                entry.lease_id,
                Some(original_lease),
                "the original lease must be intact"
            );
        }
        other => panic!("operator queue lookup after denial failed: {other:?}"),
    }

    // ── Phase 6: the isolation is verb-scoped — the delegate session still works.
    match req(&mut delegate, Request::Ping).await {
        Response::Pong => {}
        other => panic!("delegate session must stay usable after the denial, got {other:?}"),
    }

    let _ = child.kill().await;
}
