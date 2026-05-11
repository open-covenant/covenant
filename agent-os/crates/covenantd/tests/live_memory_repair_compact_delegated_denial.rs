//! Live integration test: spawns covenantd against a tempdir HOME
//! pre-seeded with a non-operator delegate peer, and verifies that
//! the memory repair and memory compaction verbs reject the
//! delegate at the capability gate without any prior
//! `memory.repair.<mode>` or `memory.compact.<mode>` grant.
//!
//! Both verbs check the capability before any mutation runs and
//! return `Response::Error` whose message names the required
//! capability. The matrix marks both `memory.repair.*` and
//! `memory.compact.*` as delegated-denial-only because a delegated
//! allowance for either mutates retention state across tiers and
//! requires a human release review.
//!
//! The repair probe uses `DeleteRecord` with a random `Uuid`. The
//! capability check happens before the visibility check, so the
//! capability rejection lands even though the record does not
//! exist; that ordering is exactly what the test is pinning down.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_memory_repair_compact_delegated_denial -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::{
    AgentId, MemoryCompactionPolicy, MemoryCompactionRequest, MemoryRepairCommand,
    MemoryRepairMode, MemoryRepairRequest,
};
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
#[ignore = "live: spawns covenantd + verifies delegated denial for memory.repair.* and memory.compact.*"]
async fn live_covenantd_memory_repair_and_compact_reject_non_operator_without_grant() {
    let home = tempfile::tempdir().expect("tempdir");

    let delegate_token = PeerToken::from_bytes([121u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [122u8; 32];
    let delegate_display = "delegate-memory-mutator@local";
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

    // ── Phase 1: RepairMemory in DryRun mode targeting a random
    //     record id is rejected at the capability gate before the
    //     record-visibility check runs. The order matters: the
    //     capability gate must fire first so a delegate cannot
    //     probe for record presence by inferring from the error
    //     message which path it took.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::RepairMemory {
                request: MemoryRepairRequest {
                    mode: MemoryRepairMode::DryRun,
                    command: MemoryRepairCommand::DeleteRecord {
                        id: Uuid::new_v4(),
                    },
                    reason: "delegated memory.repair denial probe".into(),
                },
            },
        )
        .await
        {
            Response::Error { message } => assert!(
                message.contains("memory.repair.dry_run"),
                "missing-grant memory.repair.dry_run must reject by capability gate, \
                 got {message:?}"
            ),
            other => panic!(
                "non-operator delegate must not repair memory without a grant, got {other:?}"
            ),
        }
    }

    // ── Phase 2: CompactMemory with an empty policy in DryRun mode
    //     is rejected at the capability gate. The dispatch checks
    //     the capability before the operator-identity gate, so the
    //     rejection names the required capability rather than the
    //     identity layer; either denial protects retention.
    {
        let mut stream = authenticated_stream(&sock, &delegate_token_b58).await;
        match req(
            &mut stream,
            Request::CompactMemory {
                request: MemoryCompactionRequest {
                    mode: MemoryRepairMode::DryRun,
                    policy: MemoryCompactionPolicy::default(),
                    reason: "delegated memory.compact denial probe".into(),
                },
            },
        )
        .await
        {
            Response::Error { message } => assert!(
                message.contains("memory.compact.dry_run"),
                "missing-grant memory.compact.dry_run must reject by capability gate, \
                 got {message:?}"
            ),
            other => panic!(
                "non-operator delegate must not compact memory without a grant, got {other:?}"
            ),
        }
    }

    // ── Phase 3: the delegate session is still usable. A bare
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
