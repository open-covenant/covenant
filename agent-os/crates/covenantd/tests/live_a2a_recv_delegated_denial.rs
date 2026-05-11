//! Live integration test: spawns covenantd against a tempdir HOME
//! pre-seeded with a non-operator delegate peer, has the operator
//! grant itself `a2a.send.<delegate_display>`, then attempts to
//! send an A2A task to the delegate. Because the delegate has not
//! granted `a2a.recv.<operator_display>` to itself, the daemon
//! refuses the send at the recv gate before the task lands in the
//! mailbox.
//!
//! The recv gate (lib.rs:1056-1133) is the recipient-side
//! capability check: a send is only durable when the recipient peer
//! has granted itself an `a2a.recv.<sender_display>` capability. A
//! delegated allowance for arbitrary peers to receive operator-
//! originated tasks requires a human release review; per the
//! matrix policy, automation may only add denial-only coverage
//! here.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_a2a_recv_delegated_denial -- --ignored live_`.

use covenant_a2a::A2ATask;
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
#[ignore = "live: spawns covenantd + verifies delegated denial for a2a.recv.<sender>"]
async fn live_covenantd_a2a_recv_rejects_send_when_recipient_lacks_grant() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([151u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [152u8; 32];
    let delegate_display = "delegate-a2a-recipient@local";
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

    // ── Phase 1: operator grants itself `a2a.send.<delegate_display>`
    //     so the sender-side capability gate passes and the recv
    //     gate gets the chance to fire on the recipient.
    {
        let mut stream = authenticated_stream(&sock, &operator_token).await;
        match req(
            &mut stream,
            Request::GrantCapability {
                action: format!("a2a.send.{delegate_display}"),
                scope: Some(json!({
                    "version": 1,
                    "peer_pubkey_b58": delegate.pubkey_base58()
                })),
                expires_at: None,
            },
        )
        .await
        {
            Response::CapabilityGranted { action, .. } => assert_eq!(
                action,
                format!("a2a.send.{delegate_display}"),
                "send grant action must match"
            ),
            other => panic!("operator send grant failed: {other:?}"),
        }
    }

    let task = A2ATask {
        id: Uuid::new_v4(),
        sender: operator.clone(),
        recipient: delegate.clone(),
        intent_text: "delegated a2a.recv denial probe".into(),
        task_kind: None,
        parent: None,
        deadline_ms: None,
        idempotency: None,
    };

    // ── Phase 2: operator sends to the delegate. The delegate has
    //     NOT granted itself `a2a.recv.<operator_display>`, so the
    //     daemon's recv gate refuses the send. The rejection
    //     message names the recv capability so a CLI caller can
    //     ask the recipient to grant it.
    {
        let mut stream = authenticated_stream(&sock, &operator_token).await;
        match req(&mut stream, Request::SendA2ATask { task: task.clone() }).await {
            Response::Error { message } => assert!(
                message.contains("a2a.recv"),
                "missing recv grant must reject the send, got {message:?}"
            ),
            other => panic!(
                "operator send must be rejected when recipient has no recv grant, got {other:?}"
            ),
        }
    }

    // ── Phase 3: the rejected send must not enqueue. The
    //     delegate's `TryRecvA2ATask` returns None — if the recv
    //     gate failed open in a way the message check could not
    //     detect, the task would surface here.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, Request::TryRecvA2ATask).await {
            Response::A2ATaskOpt { task: None } => {}
            other => panic!("rejected a2a send must not enqueue; delegate try_recv got {other:?}"),
        }
    }

    // ── Phase 4: the delegate session is still usable. A bare
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
