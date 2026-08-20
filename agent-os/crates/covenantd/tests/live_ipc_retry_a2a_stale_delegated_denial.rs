//! Live integration test: spawns covenantd against a tempdir HOME
//! pre-seeded with a non-operator delegate peer, and verifies that the
//! delegate cannot trigger A2A auto-retry of stale tasks.
//!
//! `retry_a2a_stale` (lib.rs:3020) checks `peer.pubkey !=
//! self.identity.agent_id().pubkey` FIRST — returning `Response::Error
//! "a2a auto retry requires the operator identity"` — and only past that
//! gate checks the `a2a.repair.requeue` capability when the policy is
//! enabled. The existing live test (`live_ipc_retry_a2a_stale.rs`)
//! authenticates as the operator; its own Step A comment states it
//! "exercises the capability gate, not the operator-identity guard", so
//! the identity gate never runs over the socket for a non-operator.
//!
//! This pins two distinct properties of the gate:
//!   - Phase 1 (ordering): a delegate WITHOUT the capability is refused
//!     with the operator-identity message and NOT the capability message,
//!     proving identity is checked before capability (an enabled policy
//!     with no capability would otherwise surface "a2a.repair.requeue").
//!   - Phase 2 (operator-only): a delegate that self-grants
//!     `a2a.repair.requeue` is STILL refused with the operator-identity
//!     message and never gets `Response::A2AAutoRetried`, proving the gate
//!     is operator-only and not satisfiable by holding the capability.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_retry_a2a_stale_delegated_denial -- --ignored live_`.

use covenant_a2a::A2AAutoRetryPolicy;
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

/// An enabled policy with a zero lease-age floor: with no operator-identity
/// gate it would reach the capability check, so the gate's precedence is
/// observable from the refusal message.
fn enabled_policy() -> A2AAutoRetryPolicy {
    A2AAutoRetryPolicy {
        enabled: true,
        min_lease_age_ms: 0,
        max_attempts: 2,
        max_requeues: 1,
        scan_limit: 5,
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies a non-operator delegate cannot trigger A2A auto-retry"]
async fn live_covenantd_retry_a2a_stale_rejects_non_operator() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([193u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [194u8; 32];
    let delegate_display = "delegate-a2a-retrier@local";
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

    // ── Phase 1: a delegate WITHOUT the capability is refused by the
    //     operator-identity gate, not the capability gate. An enabled
    //     policy with no capability would surface "a2a.repair.requeue" if
    //     the identity gate were checked second — so the message pins the
    //     ordering.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::RetryA2AStale {
                policy: enabled_policy(),
            },
        )
        .await
        {
            Response::Error { message } => {
                assert!(
                    message.contains("operator identity"),
                    "a non-operator must be refused by the identity gate first: {message:?}"
                );
                assert!(
                    !message.contains("a2a.repair.requeue"),
                    "the identity gate must precede the capability gate, so the refusal must \
                     not name the capability: {message:?}"
                );
            }
            other => panic!("delegate auto-retry must be refused, got {other:?}"),
        }
    }

    // ── Phase 2: the delegate self-grants a2a.repair.requeue (the
    //     capability subject is the authenticated peer) and re-tries. The
    //     gate is operator-only: holding the capability does not satisfy
    //     it, so the delegate is STILL refused and never gets a report.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::GrantCapability {
                action: "a2a.repair.requeue".into(),
                scope: None,
                expires_at: None,
            },
        )
        .await
        {
            Response::CapabilityGranted { .. } => {}
            other => {
                panic!("delegate self-grant of a2a.repair.requeue must succeed, got {other:?}")
            }
        }
        match req(
            &mut stream,
            Request::RetryA2AStale {
                policy: enabled_policy(),
            },
        )
        .await
        {
            Response::Error { message } => assert!(
                message.contains("operator identity"),
                "holding a2a.repair.requeue must not satisfy the operator-identity gate: {message:?}"
            ),
            other => panic!(
                "a delegate holding a2a.repair.requeue must still be refused, never \
                 Response::A2AAutoRetried: got {other:?}"
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
