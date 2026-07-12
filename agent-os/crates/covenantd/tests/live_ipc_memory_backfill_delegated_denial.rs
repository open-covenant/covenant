//! Live integration test: spawns covenantd against a tempdir HOME
//! pre-seeded with a non-operator delegate peer, and verifies that a
//! delegate which SELF-GRANTS `memory.backfill.dry_run` is still refused
//! by the operator-identity gate that sits behind the capability gate.
//!
//! `backfill_memory_records` (lib.rs:14567) checks the
//! `memory.backfill.<mode>` capability FIRST (14582) and only past it the
//! operator-identity gate (14597) `peer.pubkey !=
//! self.identity.agent_id().pubkey`, returning `Response::Error
//! "memory backfill requires the operator identity"` with no capability
//! fallback. The happy-path live test (`live_memory_backfill.rs`)
//! authenticates as the operator, so the operator-identity line never runs
//! for a non-operator that holds the capability. `grant_capability`
//! (lib.rs:5177) stores a `None` scope as the empty object `{}` and records
//! the capability subject as the authenticated peer, so a delegate can
//! self-grant the capability and clear the first gate; an empty scope is
//! unbounded and clears the post-gate scope probe (which checks
//! `before_ms = u64::MAX`), leaving the operator-identity gate as the sole
//! remaining barrier between the delegate and a metadata.receipt_id rewrite
//! over the operator's memory records.
//!
//! This also pins the gate ordering empirically: the capability gate is
//! checked BEFORE the identity gate, so a self-granting delegate does pass
//! the capability layer (the refusal names the identity, not the
//! capability) — and is then stopped only because the verb is
//! operator-only, not satisfiable by holding the grant. Drop the
//! operator-identity gate body and the same request returns
//! `Response::MemoryRecordsBackfilled`, so the gate is load-bearing.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_memory_backfill_delegated_denial -- --ignored live_`.

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
#[ignore = "live: spawns covenantd + verifies a self-granted delegate cannot backfill memory records"]
async fn live_covenantd_memory_backfill_rejects_self_granted_non_operator() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([161u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [162u8; 32];
    let delegate_display = "delegate-memory-backfiller@local";
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

    // ── Phase 1: the delegate self-grants memory.backfill.dry_run. The
    //     capability subject is the authenticated peer, so the grant
    //     succeeds and clears the capability gate for the next request.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::GrantCapability {
                action: "memory.backfill.dry_run".into(),
                scope: None,
                expires_at: None,
            },
        )
        .await
        {
            Response::CapabilityGranted { .. } => {}
            other => {
                panic!("delegate self-grant of memory.backfill.dry_run must succeed, got {other:?}")
            }
        }
    }

    // ── Phase 2: holding the capability, the delegate sends a dry-run
    //     BackfillMemoryRecords with no scope_pubkey. It is past the
    //     capability gate and the empty granted scope clears the
    //     before_ms = u64::MAX scope probe, so the operator-identity gate is
    //     the only barrier left. The delegate is refused with the identity
    //     message — NOT the capability message, and NOT
    //     Response::MemoryRecordsBackfilled (which is exactly what the same
    //     request returns once the gate is removed).
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::BackfillMemoryRecords {
                dry_run: true,
                scope_pubkey: None,
            },
        )
        .await
        {
            Response::Error { message } => {
                assert!(
                    message.contains("requires the operator identity"),
                    "a self-granted delegate must be refused by the operator-identity gate: {message:?}"
                );
                assert!(
                    !message.contains("memory.backfill"),
                    "the delegate cleared the capability gate, so the refusal must name the \
                     identity layer, not the capability: {message:?}"
                );
            }
            other => panic!(
                "a delegate holding memory.backfill.dry_run must still be refused, never \
                 Response::MemoryRecordsBackfilled: got {other:?}"
            ),
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
