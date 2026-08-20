//! Live integration test: spawns covenantd against a tempdir HOME
//! pre-seeded with a non-operator delegate peer, and verifies that a
//! peer cannot revoke a capability whose subject is a different peer by
//! replaying its signature.
//!
//! `revoke_capability` (lib.rs:15014-15066) only revokes capabilities
//! whose subject is the authenticated peer: it lists
//! `list_for_subject(peer.pubkey)` and, when the replayed signature is
//! neither the caller's own cap nor already revoked, records a
//! daemon-issued `AuditKind::CapabilityRevokeRejected { reason: "peer
//! is not the subject of this capability" }` and returns
//! `Response::Error "revoke rejected: capability subject does not match
//! authenticated peer"`. The source comment names this as the defense
//! against a different peer replaying a signature visible on
//! `/capabilities/recent`.
//!
//! The owner's-own revoke is covered by `live_ipc_revoke_capability.rs`;
//! this pins the cross-peer rejection branch: the operator's capability
//! survives, the rejection lands on the operator's audit feed (the row
//! is daemon-issued), and the delegate session stays usable.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_revoke_capability_cross_peer -- --ignored live_`.

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
#[ignore = "live: spawns covenantd + verifies cross-peer capability revoke is refused by the subject gate"]
async fn live_covenantd_revoke_capability_rejects_cross_peer_subject() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([181u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [182u8; 32];
    let delegate_display = "delegate-cap-revoker@local";
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

    // ── Phase 1: the operator grants itself a capability and captures
    //     its signature — the value a different peer would see on
    //     /capabilities/recent and try to replay.
    let operator_sig = {
        let mut stream = authenticated_stream(&sock, &operator_token).await;
        match req(
            &mut stream,
            Request::GrantCapability {
                action: "tool.call.echo".into(),
                scope: None,
                expires_at: None,
            },
        )
        .await
        {
            Response::CapabilityGranted { signature_b58, .. } => signature_b58,
            other => panic!("operator grant must succeed, got {other:?}"),
        }
    };

    // ── Phase 2: the delegate replays the operator's signature to
    //     revoke it. The subject-ownership gate refuses: the message
    //     names the subject mismatch, not a generic error.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::RevokeCapability {
                signature_b58: operator_sig.clone(),
            },
        )
        .await
        {
            Response::Error { message } => assert!(
                message.contains("capability subject does not match authenticated peer"),
                "a peer must not revoke another peer's capability, got {message:?}"
            ),
            other => panic!(
                "delegate revoke of the operator's capability must be refused, got {other:?}"
            ),
        }
    }

    // ── Phase 3: the rejection is recorded on the OPERATOR's audit
    //     feed (the row is daemon-issued so it surfaces for the
    //     subject, not the rejected peer).
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
                    AuditKind::CapabilityRevokeRejected { signature_b58, reason }
                        if signature_b58 == &operator_sig
                            && reason == "peer is not the subject of this capability"
                )),
                "the cross-peer revoke must record a daemon-issued \
                 CapabilityRevokeRejected row on the operator feed: {events:?}"
            ),
            other => panic!("expected Response::AuditEvents, got {other:?}"),
        }
    }

    // ── Phase 4: the operator's capability is untouched — the rejected
    //     revoke must not have removed it.
    {
        let mut stream = authenticated_stream(&sock, &operator_token).await;
        match req(&mut stream, Request::RecentCapabilities { limit: 100 }).await {
            Response::Capabilities { capabilities } => assert!(
                capabilities
                    .iter()
                    .any(|c| bs58::encode(c.signature).into_string() == operator_sig),
                "the operator's capability must remain active after a rejected \
                 cross-peer revoke: {capabilities:?}"
            ),
            other => panic!("expected Response::Capabilities, got {other:?}"),
        }
    }

    // ── Phase 5: the delegate session is still usable. A bare
    //     `Request::Ping` round-trips, so the refusal is scoped to the
    //     revoke, not the whole session.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, Request::Ping).await {
            Response::Pong => {}
            other => panic!("delegate must remain live after the refusal, got {other:?}"),
        }
    }

    let _ = child.kill().await;
}
