//! Live integration test: spawns covenantd against a tempdir HOME
//! pre-seeded with a non-operator delegate peer in
//! `peers/registry.jsonl`, authenticates as the delegate, and
//! verifies that the three chain-receipt verbs (`RecentReceipts`,
//! `ReceiptBatches`, `FlushReceipts`) are each rejected by the
//! capability gate without any prior `chain.receipts`,
//! `chain.batches`, or `chain.flush` grant.
//!
//! These three verbs share dispatch shape but cover distinct
//! namespaces, so a single multi-probe test keeps fixture cost low
//! while covering all three rows in the Signed Capabilities Live
//! Coverage Matrix as `delegated-denial-only`. A delegated allowance
//! for `chain.flush` would mutate batch-flush state, and an
//! allowance for either read namespace would expose settlement
//! receipt state outside the operator audience; both require a human
//! release review before automation expands.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_chain_delegated_denial -- --ignored live_`.

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
#[ignore = "live: spawns covenantd + verifies delegated denial for chain.receipts/batches/flush"]
async fn live_covenantd_chain_verbs_reject_non_operator_without_grant() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([91u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [92u8; 32];
    let delegate_display = "delegate-chain-reader@local";
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

    // ── Phase 1: RecentReceipts without `chain.receipts` grant is
    //     rejected by the capability gate. The rejection message
    //     names the required capability so a CLI caller can prompt
    //     the operator to grant it explicitly.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, Request::RecentReceipts { limit: 10 }).await {
            Response::Error { message } => assert!(
                message.contains("chain.receipts"),
                "missing-grant chain.receipts must reject by capability gate, got {message:?}"
            ),
            other => panic!("non-operator delegate must not read receipts, got {other:?}"),
        }
    }

    // ── Phase 2: ReceiptBatches without `chain.batches` grant is
    //     rejected by the capability gate.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, Request::ReceiptBatches { limit: 10 }).await {
            Response::Error { message } => assert!(
                message.contains("chain.batches"),
                "missing-grant chain.batches must reject by capability gate, got {message:?}"
            ),
            other => panic!("non-operator delegate must not read receipt batches, got {other:?}"),
        }
    }

    // ── Phase 3: FlushReceipts from a non-operator delegate is
    //     rejected before any pending batch is flushed. Flushing
    //     mutates batch state, so the dispatch path applies a
    //     stricter operator-identity gate ahead of the capability
    //     gate; a delegated allowance is structurally impossible
    //     without an explicit codepath change. Either layer
    //     produces a `Response::Error` that names the gate, which
    //     is what the test asserts.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, Request::FlushReceipts { limit: 10 }).await {
            Response::Error { message } => assert!(
                message.contains("operator identity") || message.contains("chain.flush"),
                "missing-grant chain.flush must reject by operator-identity or capability gate, \
                 got {message:?}"
            ),
            other => panic!("non-operator delegate must not flush receipts, got {other:?}"),
        }
    }

    // ── Phase 4: the delegate session is still usable. A bare
    //     `Request::Ping` round-trips, so the rejections above are
    //     scoped to the privileged actions, not the entire session.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, Request::Ping).await {
            Response::Pong => {}
            other => panic!("delegate must remain live after denial, got {other:?}"),
        }
    }

    let _ = child.kill().await;
}
