//! Live integration test: spawns covenantd against a tempdir HOME
//! pre-seeded with a non-operator delegate peer, and verifies that a
//! delegate holding a valid `memory.repair.apply` capability still
//! cannot delete a memory record owned by another peer.
//!
//! `repair_memory` (lib.rs:14215-14251) checks the
//! `memory.repair.<mode>` capability first, THEN looks up the target
//! record and refuses it with "memory repair rejected: record {id} is
//! not visible to the authenticated peer" when
//! `record.owner.pubkey != peer.pubkey` (and "memory record {id} not
//! found" when absent) — both reached only after the capability gate
//! passes and before any mutation. The existing delegated-denial test
//! (`live_memory_repair_compact_delegated_denial.rs`) targets a random
//! id WITHOUT the capability, so it always stops at the capability gate
//! and never reaches the visibility gate. This pins that a held repair
//! capability does not grant cross-peer write: the owner's record
//! survives an Apply attempt, and the absent-record branch returns its
//! own distinct message.
//!
//! Hermetic: the daemon is pinned to the mock embedder via
//! `secrets.toml` so the seed read surfaces deterministically without
//! probing Ollama. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_memory_repair_cross_peer -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken};
use covenant_types::{AgentId, MemoryRepairCommand, MemoryRepairMode, MemoryRepairRequest};
use std::path::Path;
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

/// The 32-byte ed25519 key of the daemon's operator. `load_or_create`
/// loads the key the daemon already wrote, so it matches the owner
/// stamped on records the operator writes.
fn operator_pubkey(home: &Path) -> [u8; 32] {
    covenant_identity::LocalIdentity::load_or_create(
        &home.join("identity").join("local.key"),
        "user@local",
    )
    .expect("load identity")
    .pubkey_bytes()
}

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

async fn authenticated_stream(sock: &Path, token_b58: &str) -> UnixStream {
    let mut stream = UnixStream::connect(sock).await.expect("connect");
    match req(
        &mut stream,
        Request::Authenticate {
            token_b58: token_b58.to_string(),
        },
    )
    .await
    {
        Response::Authenticated { .. } => stream,
        other => panic!("authentication failed: {other:?}"),
    }
}

async fn grant(stream: &mut UnixStream, action: &str) {
    match req(
        stream,
        Request::GrantCapability {
            action: action.into(),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { .. } => {}
        other => panic!("expected Response::CapabilityGranted ({action}), got {other:?}"),
    }
}

fn search(query: &str) -> Request {
    Request::SearchMemory {
        query: query.into(),
        tier: None,
        limit: 10,
        min_relevance: None,
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + verifies a held repair cap cannot delete another peer's record"]
async fn live_covenantd_memory_repair_rejects_cross_peer_record() {
    let home = tempfile::tempdir().expect("tempdir");
    let home = home.path();

    let delegate_token = PeerToken::from_bytes([191u8; 32]);
    let delegate_token_b58 = delegate_token.to_b58();
    let delegate_pubkey = [192u8; 32];
    let delegate_display = "delegate-memory-repairer@local";
    {
        let registry = JsonlPeerRegistry::open(home.join("peers").join("registry.jsonl"))
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

    // Pin the mock embedder before spawn so the seed read surfaces
    // deterministically and the daemon never probes Ollama.
    std::fs::write(home.join("secrets.toml"), b"[embed]\nprovider = \"mock\"\n")
        .expect("write secrets.toml");

    let port = pick_free_port();
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(exe)
        .env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");

    let sock = home.join("sock");
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon never created its socket at {}", sock.display());
    }

    let operator_token = read_operator_token(home).await;
    let mut op = authenticated_stream(&sock, &operator_token).await;
    let mut del = authenticated_stream(&sock, &delegate_token_b58).await;
    assert_ne!(
        operator_pubkey(home),
        delegate_pubkey,
        "operator and delegate pubkeys must differ"
    );

    // ── Phase 1: the operator seeds a record it owns. A no-match
    //     intent stores `phase 0 echo (no agent matched): <text>` owned
    //     by the calling operator; poll until the async write surfaces.
    grant(&mut op, "memory.write").await;
    grant(&mut op, "memory.read").await;
    match req(
        &mut op,
        Request::SubmitIntent {
            text: "cross-peer repair probe".into(),
            prefer_stream: None,
        },
    )
    .await
    {
        Response::IntentResult { .. } => {}
        other => panic!("expected Response::IntentResult, got {other:?}"),
    }
    let echo = "phase 0 echo (no agent matched): cross-peer repair probe";
    let mut owner_record_id = None;
    for _ in 0..50 {
        match req(&mut op, search(echo)).await {
            Response::Memories { records } => {
                if let Some(hit) = records.iter().find(|r| r.text == echo) {
                    assert_eq!(
                        hit.owner.pubkey,
                        operator_pubkey(home),
                        "the seeded echo must be owned by the operator: {hit:?}"
                    );
                    owner_record_id = Some(hit.id);
                    break;
                }
                sleep(Duration::from_millis(50)).await;
            }
            other => panic!("expected Response::Memories while polling, got {other:?}"),
        }
    }
    let owner_record_id = owner_record_id.expect("the seeded echo memory must surface");

    // ── Phase 2: the delegate self-grants memory.repair.apply so it
    //     clears the capability gate — the visibility gate, not the
    //     capability gate, is what must refuse it.
    grant(&mut del, "memory.repair.apply").await;

    // ── Phase 3: the delegate tries to Apply-delete the operator's
    //     record. The visibility gate refuses: a held repair capability
    //     does not grant cross-peer write.
    match req(
        &mut del,
        Request::RepairMemory {
            request: MemoryRepairRequest {
                mode: MemoryRepairMode::Apply,
                command: MemoryRepairCommand::DeleteRecord {
                    id: owner_record_id,
                },
                reason: "cross-peer repair probe".into(),
            },
        },
    )
    .await
    {
        Response::Error { message } => assert!(
            message.contains("is not visible to the authenticated peer"),
            "a delegate must not repair another peer's record, got {message:?}"
        ),
        other => {
            panic!("delegate Apply-delete of the operator's record must be refused, got {other:?}")
        }
    }

    // ── Phase 4: the operator's record survives — the rejected Apply
    //     must not have committed any delete.
    match req(&mut op, search(echo)).await {
        Response::Memories { records } => assert!(
            records.iter().any(|r| r.id == owner_record_id),
            "the operator's record must survive the rejected cross-peer Apply: {records:?}"
        ),
        other => panic!("expected Response::Memories, got {other:?}"),
    }

    // ── Phase 5: an absent record id takes the distinct not-found
    //     branch (also reached only after the capability gate passes),
    //     so the two post-capability branches stay distinguishable.
    match req(
        &mut del,
        Request::RepairMemory {
            request: MemoryRepairRequest {
                mode: MemoryRepairMode::Apply,
                command: MemoryRepairCommand::DeleteRecord { id: Uuid::new_v4() },
                reason: "absent record probe".into(),
            },
        },
    )
    .await
    {
        Response::Error { message } => assert!(
            message.contains("not found"),
            "an absent record id must take the not-found branch, got {message:?}"
        ),
        other => panic!("delegate Apply-delete of an absent record must be refused, got {other:?}"),
    }

    // ── Phase 6: the delegate session is still usable. A bare
    //     `Request::Ping` round-trips, so the refusals are scoped to the
    //     bad repairs, not the whole session.
    match req(&mut del, Request::Ping).await {
        Response::Pong => {}
        other => panic!("delegate must remain live after the refusals, got {other:?}"),
    }

    let _ = child.kill().await;
}
