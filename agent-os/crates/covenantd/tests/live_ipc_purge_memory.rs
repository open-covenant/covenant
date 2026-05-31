//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::PurgeMemory` over the raw IPC socket, asserting the granted purge
//! deletes only records strictly below the `before_ms` cutoff.
//!
//! The verb is covered today over the CLI (`live_cli_memory_purge.rs`) and HTTP
//! (`live_http_memory_purge.rs`) but never over the raw Unix socket both are
//! built on. This pins that wire contract: `Response::MemoryPurged { purged }`
//! (covenant-ipc/src/lib.rs:774) reached through the `memory.purge` gate, and
//! the tier + strict-cutoff selectivity of the underlying purge.
//!
//! Two operator-owned working-tier records are written directly into the SQLite
//! store before spawn (the daemon takes ownership on boot), so the records'
//! owner matches the operator the grant's subject resolves to. After the purge
//! the store is reopened post-teardown to confirm the older row is gone and the
//! newer one survives. Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_purge_memory -- --ignored live_`.

use covenant_identity::LocalIdentity;
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_memory::{MemoryStore, SqliteStore};
use covenant_types::{MemoryRecord, MemoryTier};
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

/// Write two operator-owned working-tier records with distinct `created_at`
/// before the daemon takes ownership of the store, returning `(older, newer)`.
async fn seed_two_working_records(home: &Path) -> (Uuid, Uuid) {
    let identity =
        LocalIdentity::load_or_create(&home.join("identity").join("local.key"), "user@local")
            .expect("create identity");
    let operator = identity.agent_id();

    let older = Uuid::new_v4();
    let newer = Uuid::new_v4();
    let store = SqliteStore::open(&home.join("memory.db")).expect("open memory store");
    for (id, created_at, text) in [
        (older, 10, "older working memory"),
        (newer, 1000, "newer working memory"),
    ] {
        store
            .put(MemoryRecord {
                id,
                tier: MemoryTier::Working,
                owner: operator.clone(),
                text: text.into(),
                embedding: Vec::new(),
                metadata: json!({}),
                created_at,
                parent: None,
            })
            .await
            .expect("put memory record");
    }
    drop(store);

    (older, newer)
}

async fn record_exists(home: &Path, id: Uuid) -> bool {
    let store = SqliteStore::open(&home.join("memory.db")).expect("reopen memory store");
    store.get(id).await.expect("memory get").is_some()
}

#[tokio::test]
#[ignore = "live: spawns covenantd + drives Request::PurgeMemory success round-trip over the socket"]
async fn live_ipc_purge_memory_applies_over_socket() {
    let home = tempfile::tempdir().expect("tempdir");
    let (older, newer) = seed_two_working_records(home.path()).await;
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    // The operator self-grants memory.purge; an empty scope object is permissive.
    match req(
        &mut stream,
        Request::GrantCapability {
            action: "memory.purge".into(),
            scope: Some(json!({})),
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { action, .. } => {
            assert_eq!(action, "memory.purge", "the grant must be for memory.purge")
        }
        other => panic!("grant must succeed, got {other:?}"),
    }

    // before_ms sits strictly between the two seeded created_at values, so a
    // correct cutoff removes only the older row.
    match req(
        &mut stream,
        Request::PurgeMemory {
            tier: Some(MemoryTier::Working),
            before_ms: 500,
        },
    )
    .await
    {
        Response::MemoryPurged { purged } => assert_eq!(
            purged, 1,
            "only the one row below the cutoff must be purged: got {purged}"
        ),
        other => panic!("granted purge must return MemoryPurged, got {other:?}"),
    }

    // Release the SQLite store so it can be reopened for the selectivity check.
    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;

    assert!(
        !record_exists(home.path(), older).await,
        "the record below the cutoff must be gone after purge"
    );
    assert!(
        record_exists(home.path(), newer).await,
        "the record at or above the cutoff must survive the purge"
    );
}
