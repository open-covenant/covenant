//! Live integration test: spawns covenantd against a tempdir HOME
//! pre-seeded with a non-operator delegate peer in
//! `peers/registry.jsonl`, authenticates as the delegate, and
//! verifies that the memory read paths (`RecentMemory`,
//! `SearchMemory`) and the memory purge path (`PurgeMemory`) are
//! each rejected by the capability gate without any prior
//! `memory.read` / `memory.read.<tier>` or `memory.purge` grant.
//!
//! Three rows in the Signed Capabilities Live Coverage Matrix
//! (`memory.read`, `memory.read.<tier>`; `memory.purge`) are touched
//! by this file. The first test closes the denial side for all three.
//! The second test (`live_covenantd_memory_read_allows_tier_scoped_delegate`)
//! closes the allowance side for `memory.read` / `memory.read.<tier>`
//! under a tier-scoped grant, leaving `memory.purge` denial-only by
//! design (mutates retention state, requires explicit human release
//! review before automation expands).
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_memory_read_purge_delegated_denial -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::{AgentId, MemoryTier};
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
#[ignore = "live: spawns covenantd + verifies delegated denial for memory.read and memory.purge"]
async fn live_covenantd_memory_read_and_purge_reject_non_operator_without_grant() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([101u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [102u8; 32];
    let delegate_display = "delegate-memory-admin@local";
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

    // ── Phase 1: RecentMemory with no tier filter requires the
    //     untiered `memory.read` capability. Without any grant, the
    //     dispatch returns Response::Error whose message names both
    //     the untiered and tier-specific alternatives so a CLI
    //     caller knows what to grant.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::RecentMemory {
                tier: None,
                limit: 10,
            },
        )
        .await
        {
            Response::Error { message } => assert!(
                message.contains("memory.read"),
                "missing-grant memory.read must reject by capability gate, got {message:?}"
            ),
            other => panic!("non-operator delegate must not read memory recents, got {other:?}"),
        }
    }

    // ── Phase 2: SearchMemory shares the same `memory.read` gate but
    //     hits it through a different code path (`search_memory`
    //     instead of `recent_memory`). Asserting both paths protects
    //     against a future regression that only repairs one entry
    //     point.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::SearchMemory {
                query: "covenant".into(),
                tier: None,
                limit: 10,
                min_relevance: None,
            },
        )
        .await
        {
            Response::Error { message } => assert!(
                message.contains("memory.read"),
                "missing-grant memory.read on search must reject by capability gate, got {message:?}"
            ),
            other => panic!(
                "non-operator delegate must not search memory, got {other:?}"
            ),
        }
    }

    // ── Phase 3: PurgeMemory mutates retention state across tiers.
    //     Without `memory.purge` the dispatch returns
    //     `Response::Error` naming that capability before any record
    //     is dropped.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::PurgeMemory {
                tier: None,
                before_ms: 1,
            },
        )
        .await
        {
            Response::Error { message } => assert!(
                message.contains("memory.purge"),
                "missing-grant memory.purge must reject by capability gate, got {message:?}"
            ),
            other => panic!("non-operator delegate must not purge memory, got {other:?}"),
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

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies tier-scoped delegated memory.read allowance"]
async fn live_covenantd_memory_read_allows_tier_scoped_delegate() {
    // Promotes the `memory.read` / `memory.read.<tier>` matrix row
    // from delegated-denial-only to delegated-covered by proving the
    // *allowance* side under an explicit tier-scoped grant. The
    // grant is `memory.read.working` (the tier-specific action form);
    // the test asserts the action gate admits RecentMemory{tier:Working}
    // and rejects RecentMemory{tier:Episodic} for the same delegate,
    // so a refactor that collapsed tier-specific actions into a
    // single wildcard or that dropped the action-list construction in
    // `memory_read_actions` would surface here.

    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([111u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [112u8; 32];
    let delegate_display = "delegate-memory-reader@local";
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
    let _operator_token = read_operator_token(home.path()).await;

    // ── Phase 1: baseline denial. RecentMemory{tier:Working} with
    //     no grant is rejected by the capability gate. Establishing
    //     the denial first ensures the subsequent allowance is not
    //     vacuous (a grant the daemon already implicitly issued, for
    //     example, would let this phase pass too).
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::RecentMemory {
                tier: Some(MemoryTier::Working),
                limit: 10,
            },
        )
        .await
        {
            Response::Error { message } => assert!(
                message.contains("memory.read"),
                "missing-grant memory.read.working must reject by capability gate, got {message:?}"
            ),
            other => panic!(
                "non-operator delegate must not read working memory pre-grant, got {other:?}"
            ),
        }
    }

    // ── Phase 2: self-grant a tier-scoped capability. The action is
    //     `memory.read.working` (tier-bound at the action layer) with
    //     an empty scope object. Covenantd's `grant_capability` binds
    //     `subject = peer` and signs with the daemon identity, so any
    //     authenticated delegate can self-grant — the test mirrors
    //     the pattern established in `live_peers_revoke.rs`.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::GrantCapability {
                action: "memory.read.working".into(),
                scope: Some(json!({})),
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
                assert_eq!(action, "memory.read.working");
            }
            other => panic!("tier-scoped grant must succeed, got {other:?}"),
        }
    }

    // ── Phase 3: allowance. RecentMemory{tier:Working} now passes
    //     the action gate because `memory_read_actions(Some(Working))`
    //     emits `[memory.read, memory.read.working]` and the delegate
    //     holds `memory.read.working`. The store is empty in this
    //     hermetic tempdir, so the response is `Memories{records: []}`;
    //     the assertion is on the response *kind*, not the contents.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::RecentMemory {
                tier: Some(MemoryTier::Working),
                limit: 10,
            },
        )
        .await
        {
            Response::Memories { records } => {
                assert!(
                    records.is_empty(),
                    "fresh tempdir should have no records; got {} records",
                    records.len()
                );
            }
            other => {
                panic!("tier-scoped grant must admit RecentMemory{{tier:Working}}, got {other:?}")
            }
        }
    }

    // ── Phase 4: tier mismatch rejection. RecentMemory{tier:Episodic}
    //     reduces to action list `[memory.read, memory.read.episodic]`
    //     and the delegate holds neither — the gate rejects. This is
    //     the load-bearing tier-isolation assertion: the same delegate
    //     grant must not leak coverage across tiers.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::RecentMemory {
                tier: Some(MemoryTier::Episodic),
                limit: 10,
            },
        )
        .await
        {
            Response::Error { message } => assert!(
                message.contains("memory.read"),
                "tier-scoped memory.read.working must not satisfy tier=Episodic, got {message:?}"
            ),
            other => panic!("tier-scoped working grant must reject Episodic read, got {other:?}"),
        }
    }

    // ── Phase 5: delegate session remains live. A bare Ping
    //     round-trips, proving the tier-mismatch rejection in Phase 4
    //     is scoped to the verb, not the connection.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(&mut stream, Request::Ping).await {
            Response::Pong => {}
            other => {
                panic!("delegate must remain live after tier-mismatch rejection, got {other:?}")
            }
        }
    }

    let _ = child.kill().await;
}
