//! Live integration test: spawns covenantd against a tempdir HOME
//! pre-seeded with a guest peer in `peers/registry.jsonl`,
//! authenticates with the bootstrap operator token, revokes the guest
//! by token-prefix via `Request::RevokePeer`, then opens a fresh
//! connection and verifies that (a) the revoked guest token fails the
//! `Authenticate` handshake with a "revoked"/"unknown" reason and the
//! connection closes, while (b) the operator token still authenticates.
//!
//! The mock tests in covenantd lib unit cover the IPC framing seam;
//! this test covers the full process boundary — JSONL replay on
//! `JsonlPeerRegistry::open()`, the bootstrap operator-token flow
//! appending alongside a pre-seeded guest, and the `respond` →
//! `revoke_by_token_prefix` → `revoked` map → `resolve` →
//! `AuthenticationFailed` chain through real IPC frames.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_peers_revoke -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, IpcError, Request, Response};
use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken, RevokeOutcome};
use covenant_types::AgentId;
use serde_json::json;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::time::sleep;

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

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

async fn authenticate(stream: &mut UnixStream, token_b58: &str) -> Response {
    req(
        stream,
        Request::Authenticate {
            token_b58: token_b58.to_string(),
        },
    )
    .await
}

async fn authenticated_stream(sock: &std::path::Path, token_b58: &str) -> UnixStream {
    let mut stream = UnixStream::connect(sock).await.expect("connect");
    match authenticate(&mut stream, token_b58).await {
        Response::Authenticated { .. } => stream,
        other => panic!("authentication failed: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + drives peers revoke through real IPC"]
async fn live_covenantd_peers_revoke_terminates_authentication() {
    let home = tempfile::tempdir().expect("tempdir");

    // ── Pre-seed `peers/registry.jsonl` with a guest entry by opening
    //     a `JsonlPeerRegistry` ourselves, registering, and dropping.
    //     The daemon's own `JsonlPeerRegistry::open()` replays the file
    //     on boot, then `bootstrap_operator_token` appends the
    //     operator entry alongside.
    let guest_token = PeerToken::generate();
    let guest_token_b58 = guest_token.to_b58();
    let guest_pubkey = [42u8; 32];
    let guest_display = "guest@local";
    let guest_entry = PeerEntry {
        token: guest_token,
        agent_id: AgentId::new(guest_display, guest_pubkey),
        registered_at: 1_700_000_000_000,
    };
    let registry_path = home.path().join("peers").join("registry.jsonl");
    {
        let registry = JsonlPeerRegistry::open(registry_path.clone())
            .await
            .expect("open seed registry");
        registry
            .register(guest_entry.clone())
            .await
            .expect("seed guest");
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
        operator_token, guest_token_b58,
        "operator and guest tokens must differ"
    );

    // ── Phase 1: the pre-seeded guest token authenticates pre-revoke.
    //     Confirms the JSONL replay actually loaded our entry — a
    //     regression here would mean the revoke path's "negative
    //     result" in Phase 3 is vacuous.
    {
        let mut stream = UnixStream::connect(&sock).await.expect("connect");
        match authenticate(&mut stream, &guest_token_b58).await {
            Response::Authenticated { display } => {
                assert_eq!(display, guest_display, "guest display mismatch");
            }
            other => panic!("guest auth pre-revoke must succeed, got {other:?}"),
        }
    }

    // ── Phase 2: operator authenticates and revokes the guest by
    //     token-prefix. Use the full b58 of the guest token as the
    //     prefix to guarantee a unique match (the chance that the
    //     daemon-minted operator token b58 has the guest's full b58
    //     as its own prefix is 1 / 58^44 — effectively zero).
    {
        let mut stream = UnixStream::connect(&sock).await.expect("connect");
        match authenticate(&mut stream, &operator_token).await {
            Response::Authenticated { .. } => {}
            other => panic!("operator auth failed: {other:?}"),
        }
        let resp = req(
            &mut stream,
            Request::RevokePeer {
                token_prefix: guest_token_b58.clone(),
                force: false,
                match_limit: None,
            },
        )
        .await;
        match resp {
            Response::PeerRevoked {
                outcome: RevokeOutcome::Revoked(summary),
            } => {
                assert_eq!(summary.agent_id.display, guest_display);
                assert_eq!(summary.agent_id.pubkey, guest_pubkey);
                assert!(
                    summary.revoked_at.is_some(),
                    "revoked_at must be set on success"
                );
                let expected_prefix: String = guest_token_b58.chars().take(6).collect();
                assert_eq!(
                    summary.token_prefix, expected_prefix,
                    "summary carries 6-char prefix, never the full token"
                );
            }
            other => panic!("expected RevokeOutcome::Revoked, got {other:?}"),
        }
    }

    // ── Phase 3: a fresh connection with the now-revoked guest token
    //     must be rejected. The reason names the token state and the
    //     daemon closes the connection (matches the
    //     `live_covenantd_rejects_unauthenticated_connection` shape).
    {
        let mut stream = UnixStream::connect(&sock).await.expect("connect");
        match authenticate(&mut stream, &guest_token_b58).await {
            Response::AuthenticationFailed { reason } => {
                assert!(
                    reason.contains("unknown") || reason.contains("revoked"),
                    "rejection must name token state; got {reason:?}"
                );
            }
            other => panic!("expected AuthenticationFailed, got {other:?}"),
        }
        let next = read_frame::<_, Response>(&mut stream).await;
        match next {
            Err(IpcError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
            other => panic!("expected EOF after auth failure, got {other:?}"),
        }
    }

    // ── Phase 4: operator token still authenticates (revocation is
    //     scoped to the token-prefix match, not a global reset).
    {
        let mut stream = UnixStream::connect(&sock).await.expect("connect");
        match authenticate(&mut stream, &operator_token).await {
            Response::Authenticated { .. } => {}
            other => panic!("operator auth post-revoke must succeed, got {other:?}"),
        }
        match req(&mut stream, Request::Ping).await {
            Response::Pong => {}
            other => panic!("post-revoke operator ping failed: {other:?}"),
        }
    }

    let _ = child.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies scoped delegated peers.revoke over IPC"]
async fn live_covenantd_peer_revoke_honors_scoped_delegation() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([21u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [22u8; 32];
    let delegate_display = "delegate-revoke@local";

    let allowed_token = PeerToken::from_bytes([23u8; 32]);
    let allowed_token_b58 = allowed_token.to_b58();
    let allowed_scope_prefix: String = allowed_token_b58.chars().take(10).collect();
    let allowed_summary_prefix: String = allowed_token_b58.chars().take(6).collect();
    let allowed_pubkey = [24u8; 32];
    let allowed_display = "allowed-revoke-target@local";

    let denied_token = PeerToken::from_bytes([25u8; 32]);
    let denied_token_b58 = denied_token.to_b58();
    let denied_pubkey = [26u8; 32];
    let denied_display = "denied-revoke-target@local";

    let registry_path = home.path().join("peers").join("registry.jsonl");
    {
        let registry = JsonlPeerRegistry::open(registry_path)
            .await
            .expect("open seed registry");
        for entry in [
            PeerEntry {
                token: delegate_token,
                agent_id: AgentId::new(delegate_display, delegate_pubkey),
                registered_at: 1_700_000_000_000,
            },
            PeerEntry {
                token: allowed_token,
                agent_id: AgentId::new(allowed_display, allowed_pubkey),
                registered_at: 1_700_000_000_001,
            },
            PeerEntry {
                token: denied_token,
                agent_id: AgentId::new(denied_display, denied_pubkey),
                registered_at: 1_700_000_000_002,
            },
        ] {
            registry.register(entry).await.expect("seed peer");
        }
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
    let _operator_token = read_operator_token(home.path()).await;

    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::RevokePeer {
                token_prefix: allowed_token_b58.clone(),
                force: false,
                match_limit: None,
            },
        )
        .await
        {
            Response::Error { message } => assert!(
                message.contains("capability"),
                "missing delegated grant must be rejected by capability gate: {message:?}"
            ),
            other => panic!("missing grant must reject delegated revoke, got {other:?}"),
        }
    }

    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::GrantCapability {
                action: "peers.revoke".into(),
                scope: Some(json!({
                    "version": 1,
                    "token_prefix": allowed_scope_prefix,
                    "force": false
                })),
                expires_at: None,
            },
        )
        .await
        {
            Response::CapabilityGranted {
                subject_display,
                action,
                ..
            } => {
                assert_eq!(subject_display, delegate_display);
                assert_eq!(action, "peers.revoke");
            }
            other => panic!("delegated scoped grant must succeed, got {other:?}"),
        }
    }

    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::RevokePeer {
                token_prefix: denied_token_b58.clone(),
                force: false,
                match_limit: None,
            },
        )
        .await
        {
            Response::Error { message } => assert!(
                message.contains("capability scope"),
                "mismatched delegated token_prefix must be rejected by scope: {message:?}"
            ),
            other => panic!("scoped delegated revoke must reject wrong token, got {other:?}"),
        }
    }

    {
        let mut stream = authenticated_stream(&sock, &denied_token_b58).await;
        match req(&mut stream, Request::Ping).await {
            Response::Pong => {}
            other => panic!("denied target must remain live after scoped rejection, got {other:?}"),
        }
    }

    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::RevokePeer {
                token_prefix: allowed_token_b58.clone(),
                force: false,
                match_limit: None,
            },
        )
        .await
        {
            Response::PeerRevoked {
                outcome: RevokeOutcome::Revoked(summary),
            } => {
                assert_eq!(summary.agent_id.display, allowed_display);
                assert_eq!(summary.agent_id.pubkey, allowed_pubkey);
                assert_eq!(summary.token_prefix, allowed_summary_prefix);
            }
            other => panic!("scoped delegated revoke must allow matching token, got {other:?}"),
        }
    }

    {
        let mut stream = UnixStream::connect(&sock).await.expect("connect");
        match authenticate(&mut stream, &allowed_token_b58).await {
            Response::AuthenticationFailed { reason } => assert!(
                reason.contains("unknown") || reason.contains("revoked"),
                "revoked target rejection must name token state: {reason:?}"
            ),
            other => panic!("allowed target must be revoked, got {other:?}"),
        }
    }

    let _ = child.kill().await;
}
