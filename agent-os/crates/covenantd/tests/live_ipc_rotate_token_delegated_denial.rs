//! Live integration test: spawns covenantd against a tempdir HOME
//! pre-seeded with a non-operator delegate peer, and verifies that the
//! delegate cannot rotate the operator token.
//!
//! `rotate_operator_token` (lib.rs:3265) gates on the operator identity
//! with no capability fallback: when `peer.pubkey !=
//! self.identity.agent_id().pubkey` it records a daemon-issued
//! `AuditKind::OperatorTokenRotationRejected { peer_display,
//! peer_pubkey_b58 }` via `record_daemon_event_required` and returns
//! `Response::Error "operator token rotation requires the operator
//! identity"`. The happy path is covered by `live_rotate_token.rs`,
//! which authenticates as the operator and rotates; this pins the
//! delegate-denial branch — the gate that stops a non-operator from
//! seizing the root authority — over the real socket.
//!
//! The rejection row is daemon-issued, so it surfaces on the OPERATOR's
//! audit feed (recent_audit filters `issuer.pubkey == peer.pubkey`), and
//! `peer_pubkey_b58` is the unforgeable identifier of the rejected peer.
//! The test also proves no rotation happened: the operator's original
//! token still authenticates and the on-disk `operator.token` is
//! byte-unchanged.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_rotate_token_delegated_denial -- --ignored live_`.

use covenant_audit::AuditKind;
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::time::sleep;

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
    let request = Request::Authenticate {
        token_b58: token_b58.to_string(),
    };
    match req(&mut stream, request).await {
        Response::Authenticated { .. } => stream,
        other => panic!("authentication failed: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies a non-operator delegate cannot rotate the operator token"]
async fn live_covenantd_rotate_operator_token_rejects_non_operator() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([191u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [192u8; 32];
    let delegate_display = "delegate-token-rotator@local";
    let registry_path = home.path().join("peers").join("registry.jsonl");
    {
        let registry = JsonlPeerRegistry::open(registry_path)
            .await
            .expect("open seed registry");
        registry
            .register(PeerEntry {
                token: delegate_token,
                agent_id: AgentId::new(delegate_display, delegate_pubkey),
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
    let token_path = home.path().join("peers").join("operator.token");
    let on_disk_before = std::fs::read_to_string(&token_path).expect("read operator.token");

    // ── Phase 1: the delegate tries to rotate the operator token. The
    //     operator-identity gate refuses by name — not with a new token.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, Request::RotateOperatorToken).await {
            Response::Error { message } => assert!(
                message.contains("operator token rotation requires the operator identity"),
                "a non-operator must be refused by the identity gate, got {message:?}"
            ),
            other => panic!(
                "delegate rotation must be refused, never Response::OperatorTokenRotated: got {other:?}"
            ),
        }
    }

    // ── Phase 2: the rejection is recorded on the OPERATOR's audit feed
    //     (daemon-issued), and the rejected peer is named by its
    //     unforgeable pubkey, not just its wire-supplied display.
    let delegate_pubkey_b58 = bs58::encode(delegate_pubkey).into_string();
    {
        let mut stream = authenticated_stream(&sock, &operator_token).await;
        match req(
            &mut stream,
            Request::RecentAudit {
                limit: 50,
                since_ms: None,
                prefer_stream: None,
            },
        )
        .await
        {
            Response::AuditEvents { events } => assert!(
                events.iter().any(|event| matches!(
                    &event.kind,
                    AuditKind::OperatorTokenRotationRejected { peer_display, peer_pubkey_b58 }
                        if peer_pubkey_b58 == &delegate_pubkey_b58
                            && peer_display == delegate_display
                )),
                "the refused rotation must record a daemon-issued \
                 OperatorTokenRotationRejected row naming the delegate on the operator feed: {events:?}"
            ),
            other => panic!("expected Response::AuditEvents, got {other:?}"),
        }
    }

    // ── Phase 3: no rotation happened. The operator's original token
    //     still authenticates and the on-disk token is byte-unchanged, so
    //     the refusal cannot have mutated state before the gate.
    {
        let mut stream = authenticated_stream(&sock, &operator_token).await;
        match req(&mut stream, Request::Ping).await {
            Response::Pong => {}
            other => panic!("the operator's original token must still authenticate, got {other:?}"),
        }
    }
    let on_disk_after = std::fs::read_to_string(&token_path).expect("read operator.token");
    assert_eq!(
        on_disk_before, on_disk_after,
        "a refused rotation must not rewrite peers/operator.token"
    );

    // ── Phase 4: the delegate session is still usable. A bare
    //     `Request::Ping` round-trips, so the refusal is scoped to the
    //     rotation, not the whole session.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, Request::Ping).await {
            Response::Pong => {}
            other => panic!("delegate must remain live after the refusal, got {other:?}"),
        }
    }

    let _ = child.kill().await;
}
