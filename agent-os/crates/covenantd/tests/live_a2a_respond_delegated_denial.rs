//! Live integration test: spawns covenantd against a tempdir HOME
//! pre-seeded with a non-operator delegate peer. The operator sends
//! an A2A task to the delegate so the task lands durably in the
//! mailbox, then the delegate calls `Request::PostA2AResult` for
//! that task without holding the corresponding
//! `a2a.respond.<operator_display>` capability. The daemon's
//! respond gate (lib.rs:1201-1218) rejects the post; the test
//! asserts the rejection names the respond capability.
//!
//! The task must be enqueued first or the dispatch returns
//! "task_id was never dispatched" before the respond capability
//! gate gets a chance to fire. The setup grants are:
//! - operator: a2a.send.<delegate_display>  (scoped to delegate
//!   pubkey b58)
//! - delegate: a2a.recv.<operator_display>  (scoped to operator
//!   pubkey b58 and the task_id) — required for the send to
//!   succeed
//!
//! The delegate intentionally does NOT grant itself
//! a2a.respond.<operator_display>; that is the gate the test pins.
//!
//! Allowance for delegated respond paths requires a human release
//! review: a response can carry attacker-controlled content that
//! the operator's audit feed records under the trusted responder.
//! Per the matrix policy, automation may only add denial-only
//! coverage here.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_a2a_respond_delegated_denial -- --ignored live_`.

use covenant_a2a::{A2ATask, A2ATaskResult};
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
use serde_json::json;
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

async fn wait_for_sock(path: &std::path::Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn read_operator_token(home: &std::path::Path) -> String {
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

fn read_operator_pubkey(home: &std::path::Path) -> [u8; 32] {
    let path = home.join("identity").join("local.key");
    let id = covenant_identity::LocalIdentity::load_or_create(&path, "user@local")
        .expect("load identity");
    id.pubkey_bytes()
}

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

async fn authenticated_stream(sock: &std::path::Path, token_b58: &str) -> UnixStream {
    let mut stream = UnixStream::connect(sock).await.expect("connect");
    let request = Request::Authenticate {
        token_b58: token_b58.to_string(),
    };
    match req(&mut stream, request).await {
        Response::Authenticated { .. } => stream,
        other => panic!("authentication failed: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies delegated denial for a2a.respond.<sender>"]
async fn live_covenantd_a2a_respond_rejects_recipient_without_grant() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([161u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [162u8; 32];
    let delegate_display = "delegate-a2a-responder@local";
    let delegate = AgentId::new(delegate_display, delegate_pubkey);
    let registry_path = home.path().join("peers").join("registry.jsonl");
    {
        let registry = JsonlPeerRegistry::open(registry_path)
            .await
            .expect("open seed registry");
        registry
            .register(PeerEntry {
                token: delegate_token,
                agent_id: delegate.clone(),
                registered_at: 1_700_000_000_000,
            })
            .await
            .expect("seed delegate");
    }

    let port = pick_free_port();
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(exe)
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

    let operator_token = read_operator_token(home.path()).await;
    let operator = AgentId::new("user@local", read_operator_pubkey(home.path()));
    assert_ne!(
        operator.pubkey, delegate.pubkey,
        "operator and delegate pubkeys must differ"
    );

    let task = A2ATask {
        id: Uuid::new_v4(),
        sender: operator.clone(),
        recipient: delegate.clone(),
        intent_text: "delegated a2a.respond denial probe".into(),
        task_kind: None,
        parent: None,
        deadline_ms: None,
        idempotency: None,
    };

    // ── Phase 1: operator grants itself a2a.send.<delegate> so the
    //     send-side capability gate passes.
    {
        let mut stream = authenticated_stream(&sock, &operator_token).await;
        match req(
            &mut stream,
            Request::GrantCapability {
                action: format!("a2a.send.{delegate_display}"),
                scope: Some(json!({
                    "version": 1,
                    "peer_pubkey_b58": delegate.pubkey_base58(),
                    "task_id": task.id.to_string()
                })),
                expires_at: None,
            },
        )
        .await
        {
            Response::CapabilityGranted { .. } => {}
            other => panic!("operator send grant failed: {other:?}"),
        }
    }

    // ── Phase 2: delegate grants itself a2a.recv.<operator> so the
    //     recipient-side recv gate passes when the operator sends.
    //     Without this, the send would fail at the recv gate (which
    //     is what the previous slice already covers); this slice
    //     instead targets the respond gate, so we have to step
    //     past the recv gate.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::GrantCapability {
                action: format!("a2a.recv.{}", operator.display),
                scope: Some(json!({
                    "version": 1,
                    "peer_pubkey_b58": operator.pubkey_base58(),
                    "task_id": task.id.to_string()
                })),
                expires_at: None,
            },
        )
        .await
        {
            Response::CapabilityGranted { .. } => {}
            other => panic!("delegate recv grant failed: {other:?}"),
        }
    }

    // ── Phase 3: operator sends the task. With both prerequisite
    //     grants in place, the send succeeds and the task lands
    //     durably in the mailbox.
    {
        let mut stream = authenticated_stream(&sock, &operator_token).await;
        match req(&mut stream, Request::SendA2ATask { task: task.clone() }).await {
            Response::A2ATaskQueued { task_id } => assert_eq!(task_id, task.id),
            other => panic!("operator send must succeed with both grants, got {other:?}"),
        }
    }

    // ── Phase 4: delegate tries to PostA2AResult for the task
    //     WITHOUT a2a.respond.<operator_display>. The respond gate
    //     fires and rejects the post; the message names the
    //     respond capability.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::PostA2AResult {
                result: A2ATaskResult::ok(task.id, vec![]),
            },
        )
        .await
        {
            Response::Error { message } => assert!(
                message.contains("a2a.respond"),
                "missing-grant a2a.respond must reject the post, got {message:?}"
            ),
            other => panic!(
                "delegate must not post a result without an a2a.respond grant, got {other:?}"
            ),
        }
    }

    // ── Phase 5: the operator must NOT receive a result for this
    //     task. `TryRecvA2AResult` returns None because the
    //     rejected post never reached the result mailbox.
    {
        let mut stream = authenticated_stream(&sock, &operator_token).await;
        match req(&mut stream, Request::TryRecvA2AResult).await {
            Response::A2AResultOpt { result: None } => {}
            other => panic!(
                "rejected a2a respond must not surface to operator try_recv_result, got {other:?}"
            ),
        }
    }

    // ── Phase 6: the delegate session is still usable. A bare
    //     `Request::Ping` round-trips, so the rejection is scoped
    //     to the privileged action.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, Request::Ping).await {
            Response::Pong => {}
            other => panic!("delegate must remain live after denial, got {other:?}"),
        }
    }

    let _ = child.kill().await;
}
