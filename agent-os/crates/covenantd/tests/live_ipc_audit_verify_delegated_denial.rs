//! Live integration test: spawns covenantd against a tempdir HOME
//! pre-seeded with a non-operator delegate peer, and verifies that the
//! delegate cannot run global audit-integrity verification.
//!
//! `verify_audit_integrity` (lib.rs:3891) gates on the operator identity
//! with no capability fallback: when `peer.pubkey !=
//! self.identity.agent_id().pubkey` it returns `Response::Error "audit
//! integrity verification requires the operator identity"`; only the
//! operator reaches `self.audit.verify_integrity()` and the global
//! `Response::AuditIntegrity` report. Unlike `recent_audit`, which is
//! feed-scoped to the caller's own issuer, this is a whole-chain read —
//! its verdict and root hash must not leak to a non-operator.
//!
//! The happy path is covered by `live_ipc_audit_verify.rs`, which
//! authenticates as the operator (its baseline comment notes "A broken
//! gate answers Response::Error"); this pins the delegate-denial branch
//! over the real socket while confirming the operator still gets a valid
//! report, so the refusal is identity-scoped and not a broken verb.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_audit_verify_delegated_denial -- --ignored live_`.

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
#[ignore = "live: spawns covenantd + verifies a non-operator delegate cannot run audit-integrity verification"]
async fn live_covenantd_verify_audit_integrity_rejects_non_operator() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([195u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [196u8; 32];
    let delegate_display = "delegate-audit-verifier@local";
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

    // ── Phase 1: the delegate asks for the global integrity verdict and
    //     is refused by name. The whole-chain verdict and root hash never
    //     leak to a non-operator (recent_audit is feed-scoped; this is
    //     not).
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, Request::VerifyAuditIntegrity).await {
            Response::Error { message } => assert!(
                message.contains("audit integrity verification requires the operator identity"),
                "a non-operator must be refused by the identity gate, got {message:?}"
            ),
            other => panic!(
                "delegate audit verification must be refused, never Response::AuditIntegrity: got {other:?}"
            ),
        }
    }

    // ── Phase 2: the operator clears the gate and gets a valid report,
    //     so the refusal is identity-scoped, not a broken verb.
    {
        let mut stream = authenticated_stream(&sock, &operator_token).await;
        match req(&mut stream, Request::VerifyAuditIntegrity).await {
            Response::AuditIntegrity { report } => {
                assert!(
                    report.valid,
                    "a fresh daemon's chain must verify clean: {report:?}"
                );
                assert_eq!(
                    report.root_hash_hex.len(),
                    64,
                    "the integrity root is a 64-hex sha256 chain hash: {report:?}"
                );
            }
            other => panic!("operator audit verification must succeed, got {other:?}"),
        }
    }

    // ── Phase 3: the delegate session is still usable. A bare
    //     `Request::Ping` round-trips, so the refusal is scoped to the
    //     verb, not the whole session.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, Request::Ping).await {
            Response::Pong => {}
            other => panic!("delegate must remain live after the refusal, got {other:?}"),
        }
    }

    let _ = child.kill().await;
}
