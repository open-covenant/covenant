//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::CompactMemory` (Apply) over the raw IPC socket, asserting the
//! daemon gates the apply on `memory.compact.apply`, deletes the seeded
//! working-tier record, and answers `Response::MemoryCompacted { outcome }`.
//!
//! The apply path is covered over the CLI (`live_cli_memory_compaction.rs`) and
//! HTTP (`live_http_memory_compact_apply.rs`); the only socket coverage today is
//! the delegate-denial gate (`live_memory_repair_compact_delegated_denial.rs`).
//! This pins the `Response::MemoryCompacted` wire shape
//! (covenant-ipc/src/lib.rs:780) and the operator apply path at the raw Unix
//! socket boundary the gateway and CLI are built on.
//!
//! The operator identity is created before spawn so the daemon loads the same
//! operator the token authenticates as (the compaction handler's
//! operator-identity guard requires it), and one operator-owned working record
//! is written into the store at `created_at: 1` so a cutoff of 2 selects it.
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_compact_memory -- --ignored live_`.

use covenant_identity::LocalIdentity;
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_memory::{MemoryStore, SqliteStore};
use covenant_types::{
    MemoryCompactionPolicy, MemoryCompactionRequest, MemoryRecord, MemoryRepairMode, MemoryTier,
};
use serde_json::json;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;
use uuid::Uuid;

fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().unwrap().port()
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

async fn spawn_daemon(home: &Path) -> Child {
    let port = pick_free_port();
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let child = Command::new(exe)
        .env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");
    if !wait_for_sock(&home.join("sock")).await {
        panic!("daemon never created its socket");
    }
    child
}

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

async fn authenticated_stream(home: &Path) -> UnixStream {
    let mut stream = UnixStream::connect(home.join("sock"))
        .await
        .expect("connect socket");
    let token = read_operator_token(home).await;
    match req(&mut stream, Request::Authenticate { token_b58: token }).await {
        Response::Authenticated { .. } => {}
        other => panic!("authenticate failed: {other:?}"),
    }
    stream
}

/// Create the operator identity and one operator-owned working-tier record
/// under `home` before the daemon takes ownership of the store, returning its
/// id. The identity is created first so the daemon loads the same operator the
/// token authenticates as. `created_at` is 1 so a cutoff of 2 selects it.
async fn seed_operator_record(home: &Path) -> Uuid {
    let identity = LocalIdentity::load_or_create(&home.join("identity").join("local.key"), "user@local")
        .expect("create identity");
    let operator = identity.agent_id();

    let id = Uuid::new_v4();
    let store = SqliteStore::open(&home.join("memory.db")).expect("open memory store");
    store
        .put(MemoryRecord {
            id,
            tier: MemoryTier::Working,
            owner: operator,
            text: "compactable working memory".into(),
            embedding: Vec::new(),
            metadata: json!({}),
            created_at: 1,
            parent: None,
        })
        .await
        .expect("put memory record");
    drop(store);
    id
}

fn compact_apply_request() -> MemoryCompactionRequest {
    MemoryCompactionRequest {
        mode: MemoryRepairMode::Apply,
        policy: MemoryCompactionPolicy {
            delete_working_before_ms: Some(2),
            ..Default::default()
        },
        reason: "ipc compaction apply probe".into(),
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + drives Request::CompactMemory apply over the socket"]
async fn live_ipc_compact_memory_applies_over_socket() {
    let home = tempfile::tempdir().expect("tempdir");
    let id = seed_operator_record(home.path()).await;
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    // Phase A: without the grant the apply is rejected by name before any
    // record is touched.
    match req(
        &mut stream,
        Request::CompactMemory {
            request: compact_apply_request(),
        },
    )
    .await
    {
        Response::Error { message } => {
            assert!(
                message.contains("memory.compact.apply"),
                "rejection must name the missing capability: {message}"
            );
            assert!(
                message.contains("requires capability"),
                "rejection must surface the requires-capability prefix: {message}"
            );
        }
        other => panic!("ungranted apply must be rejected, got {other:?}"),
    }

    // Phase B: self-grant memory.compact.apply over the same socket. An
    // action-only unscoped grant passes the compaction scope check.
    match req(
        &mut stream,
        Request::GrantCapability {
            action: "memory.compact.apply".into(),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { action, .. } => assert_eq!(
            action, "memory.compact.apply",
            "the grant must be for memory.compact.apply"
        ),
        other => panic!("grant must succeed, got {other:?}"),
    }

    // Phase C: the granted apply deletes the seeded record and reports it.
    match req(
        &mut stream,
        Request::CompactMemory {
            request: compact_apply_request(),
        },
    )
    .await
    {
        Response::MemoryCompacted { outcome } => {
            assert_eq!(
                outcome.mode,
                MemoryRepairMode::Apply,
                "the outcome must echo the requested apply mode"
            );
            assert!(
                outcome.would_change,
                "deleting a working record before the cutoff is a change"
            );
            assert!(
                outcome.changed,
                "an apply must report the change as committed"
            );
            assert!(
                outcome.deleted.contains(&id),
                "the seeded record id must appear in outcome.deleted: {:?}",
                outcome.deleted
            );
        }
        other => panic!("granted apply must return MemoryCompacted, got {other:?}"),
    }

    // No-op control: a second apply with the same policy finds nothing left,
    // proving the first apply committed rather than merely previewing.
    match req(
        &mut stream,
        Request::CompactMemory {
            request: compact_apply_request(),
        },
    )
    .await
    {
        Response::MemoryCompacted { outcome } => assert!(
            outcome.deleted.is_empty(),
            "the record is already gone, so a re-apply deletes nothing: {:?}",
            outcome.deleted
        ),
        other => panic!("re-apply must return MemoryCompacted, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;

    // Phase D: the deletion is durable in the store after the daemon exits.
    let store = SqliteStore::open(&home.path().join("memory.db")).expect("reopen memory store");
    assert!(
        store.get(id).await.expect("memory get").is_none(),
        "apply must delete the record from the store"
    );
}
