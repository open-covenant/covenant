//! Live integration test: spawns covenantd against a tempdir HOME
//! pre-seeded with a non-operator delegate peer in
//! `peers/registry.jsonl`, authenticates as the delegate, and
//! verifies that both `Request::ListPeers` and `Request::PurgePeers`
//! are rejected by the capability gate without any prior grant.
//!
//! The mock tests in covenantd lib unit cover the in-process gate
//! shape (`list_peers_rejects_non_operator_with_audit_row`,
//! `purge_peers_rejects_without_capability`); this test covers the
//! full process boundary — JSONL replay, authenticated IPC frames,
//! and the dispatch-time enforcement chain that closes the
//! `peers.list` and `peers.purge` rows from `gap` to
//! `delegated-denial-only` in the Signed Capabilities Live Coverage
//! Matrix.
//!
//! Allowance paths are out of scope: granting a delegated allowance
//! for either action is operator administration that requires a human
//! release review. Per the matrix policy, automation may only add
//! denial-only delegated coverage here.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_peers_list_purge_delegated_denial -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::AgentId;
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
#[ignore = "live: spawns covenantd + verifies delegated denial for peers.list and peers.purge"]
async fn live_covenantd_peers_list_and_purge_reject_non_operator_without_grant() {
    let home = tempfile::tempdir().expect("tempdir");

    // ── Pre-seed `peers/registry.jsonl` with a delegate entry by
    //     opening a `JsonlPeerRegistry` ourselves and registering. The
    //     daemon replays the file on boot, then `bootstrap_operator_token`
    //     appends the operator entry alongside, so the delegate
    //     authenticates with its own (non-operator) token.
    let delegate_token = PeerToken::from_bytes([71u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [72u8; 32];
    let delegate_display = "delegate-peers-admin@local";
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

    // ── Phase 1: a non-operator delegate without any `peers.list`
    //     grant must be rejected by the capability gate. The dispatch
    //     code returns `Response::Error` whose message names the
    //     operator identity or the required capability so the
    //     rejection is unambiguous to a CLI caller.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::ListPeers {
                limit: 10,
                pubkey_prefix: None,
                status_filter: None,
            },
        )
        .await
        {
            Response::Error { message } => assert!(
                message.contains("operator identity") || message.contains("peers.list"),
                "missing-grant peers.list must reject by capability gate, got {message:?}"
            ),
            other => panic!(
                "non-operator delegate must not enumerate the registry, got {other:?}"
            ),
        }
    }

    // ── Phase 2: a non-operator delegate without any `peers.purge`
    //     grant must be rejected by the capability gate. Purging
    //     retention tombstones is operator administration; an
    //     unauthenticated delegated path here would silently mutate
    //     `peers/registry.jsonl`.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, Request::PurgePeers { before_ms: 1 }).await {
            Response::Error { message } => assert!(
                message.contains("peers.purge"),
                "missing-grant peers.purge must reject by capability gate, got {message:?}"
            ),
            other => panic!(
                "non-operator delegate must not purge the registry, got {other:?}"
            ),
        }
    }

    // ── Phase 3: the delegate connection is still usable. A bare
    //     `Request::Ping` round-trips, so the rejections above are
    //     scoped to the privileged action, not the entire session.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, Request::Ping).await {
            Response::Pong => {}
            other => panic!("delegate must remain live after denial, got {other:?}"),
        }
    }

    let _ = child.kill().await;
}
