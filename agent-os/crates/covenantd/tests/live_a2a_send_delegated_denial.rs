//! Live integration test: spawns covenantd against a tempdir HOME
//! pre-seeded with a non-operator delegate peer, has the delegate
//! authenticate, construct an `A2ATask` with the delegate as sender
//! and the operator as recipient, and verifies that
//! `Request::SendA2ATask` is rejected by the capability gate before
//! the task can land in the operator mailbox.
//!
//! The delegate IS the sender, so the dispatch's spoof check
//! (`task.sender.pubkey == peer.pubkey`) passes; the rejection
//! happens at the `a2a.send.<recipient>` capability gate. A
//! delegated allowance to send arbitrary tasks to the operator
//! crosses an audience boundary and requires a human release
//! review, so per the matrix policy automation may only add
//! denial-only coverage here.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_a2a_send_delegated_denial -- --ignored live_`.

use covenant_a2a::A2ATask;
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
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
#[ignore = "live: spawns covenantd + verifies delegated denial for a2a.send.<recipient>"]
async fn live_covenantd_a2a_send_rejects_non_operator_without_grant() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([111u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [112u8; 32];
    let delegate_display = "delegate-a2a-sender@local";
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
    assert_ne!(
        operator_token, delegate_token_b58,
        "operator and delegate tokens must differ"
    );
    let operator = AgentId::new("user@local", read_operator_pubkey(home.path()));
    assert_ne!(
        operator.pubkey, delegate.pubkey,
        "operator and delegate pubkeys must differ"
    );

    let task = A2ATask {
        id: Uuid::new_v4(),
        sender: delegate.clone(),
        recipient: operator.clone(),
        intent_text: "delegated a2a.send denial probe".into(),
        task_kind: None,
        parent: None,
        deadline_ms: None,
        idempotency: None,
    };

    // ── Phase 1: a non-operator delegate sending to the operator
    //     without an `a2a.send.<operator_display>` (or pubkey-form)
    //     grant must be rejected by the capability gate. The
    //     dispatch returns Response::Error whose message names the
    //     capability so a CLI caller knows what grant is missing.
    //     The delegate is task.sender, so the spoof check passes
    //     before the capability gate fires; this isolates the
    //     denial to the capability layer.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, Request::SendA2ATask { task: task.clone() }).await {
            Response::Error { message } => assert!(
                message.contains("a2a.send"),
                "missing-grant a2a.send must reject by capability gate, got {message:?}"
            ),
            other => panic!(
                "non-operator delegate must not send a2a tasks without a grant, got {other:?}"
            ),
        }
    }

    // ── Phase 2: the rejected send must not enqueue. From the
    //     operator session, try_recv must return None — if it
    //     returned the rejected task, the gate would have failed
    //     open in a way the message check could not detect.
    {
        let mut stream = authenticated_stream(&sock, &operator_token).await;
        match req(&mut stream, Request::TryRecvA2ATask).await {
            Response::A2ATaskOpt { task: None } => {}
            other => panic!(
                "rejected a2a send must not enqueue; operator try_recv got {other:?}"
            ),
        }
    }

    // ── Phase 3: the delegate session is still usable. A bare
    //     `Request::Ping` round-trips, so the rejection is scoped
    //     to the privileged action, not the entire session.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, Request::Ping).await {
            Response::Pong => {}
            other => panic!("delegate must remain live after denial, got {other:?}"),
        }
    }

    let _ = child.kill().await;
}
