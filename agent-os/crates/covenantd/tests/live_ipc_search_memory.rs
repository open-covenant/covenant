//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::SearchMemory` over the raw IPC socket through its full gate —
//! ungranted rejection, granted-but-empty, then a semantically matched row.
//!
//! The verb is covered today over the CLI (`live_cli_memory_search.rs`) and
//! HTTP (`live_http_memory_search.rs`) but never over the raw Unix socket both
//! are built on. This pins that wire contract: the `memory.read` capability
//! gate (`search_memory`, covenantd/src/lib.rs:5319), the
//! `Response::Memories { records }` variant, and the owner-scoped read that
//! stamps every record with the calling operator (`MemoryRecord.owner`,
//! covenant-types/src/lib.rs:236).
//!
//! Hermetic on any host: the daemon is pinned to the mock embedder via
//! `secrets.toml` before spawn, so a no-match intent's `phase 0 echo` memory is
//! found deterministically without probing a local Ollama. The write is indexed
//! asynchronously, so the read polls until the echo surfaces. `#[ignore]`'d.
//! Run with `cargo test -p covenantd --test live_ipc_search_memory -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;

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

/// The 32-byte ed25519 key of the daemon's operator. `load_or_create` loads the
/// key the daemon already wrote, so it matches the owner stamped on records the
/// operator writes — `Response::Memories` never echoes the caller's pubkey back.
fn operator_pubkey(home: &Path) -> [u8; 32] {
    covenant_identity::LocalIdentity::load_or_create(
        &home.join("identity").join("local.key"),
        "user@local",
    )
    .expect("load identity")
    .pubkey_bytes()
}

async fn spawn_daemon(home: &Path) -> Child {
    // Pin the mock embedder before spawn so search scores are deterministic and
    // the daemon never probes Ollama at localhost:11434 (non-hermetic).
    std::fs::write(home.join("secrets.toml"), b"[embed]\nprovider = \"mock\"\n")
        .expect("write secrets.toml");
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

fn search(query: &str) -> Request {
    Request::SearchMemory {
        query: query.into(),
        tier: None,
        limit: 10,
        min_relevance: None,
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + drives Request::SearchMemory over the socket"]
async fn live_ipc_search_memory_gates_then_finds_seeded_record() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    // Ungranted baseline: search requires the memory.read capability, so the
    // daemon must reject the read before any grant.
    match req(&mut stream, search("socket search probe")).await {
        Response::Error { message } => assert!(
            message.contains("memory.read"),
            "ungranted search must name the missing capability: {message:?}"
        ),
        other => panic!("expected Response::Error before grant, got {other:?}"),
    }

    // Open the read.
    match req(
        &mut stream,
        Request::GrantCapability {
            action: "memory.read".into(),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { .. } => {}
        other => panic!("expected Response::CapabilityGranted (memory.read), got {other:?}"),
    }

    // Granted but empty store: a deterministic empty result.
    match req(&mut stream, search("socket search probe")).await {
        Response::Memories { records } => assert!(
            records.is_empty(),
            "the store is empty, so a granted search returns no records: {records:?}"
        ),
        other => panic!("expected empty Response::Memories after grant, got {other:?}"),
    }

    // Seed one owned record: a no-match intent makes the router store
    // `phase 0 echo (no agent matched): <text>` owned by the caller. memory.write
    // gates that persist.
    match req(
        &mut stream,
        Request::GrantCapability {
            action: "memory.write".into(),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { .. } => {}
        other => panic!("expected Response::CapabilityGranted (memory.write), got {other:?}"),
    }
    match req(
        &mut stream,
        Request::SubmitIntent {
            text: "socket search probe".into(),
            prefer_stream: None,
        },
    )
    .await
    {
        Response::IntentResult { .. } => {}
        other => panic!("expected Response::IntentResult, got {other:?}"),
    }

    // The write is indexed asynchronously; poll the mock-embedded search until
    // the echo surfaces.
    let echo = "phase 0 echo (no agent matched): socket search probe";
    let mut records = Vec::new();
    for _ in 0..50 {
        match req(&mut stream, search(echo)).await {
            Response::Memories { records: r } if !r.is_empty() => {
                records = r;
                break;
            }
            Response::Memories { .. } => sleep(Duration::from_millis(50)).await,
            other => panic!("expected Response::Memories while polling, got {other:?}"),
        }
    }
    assert!(
        !records.is_empty(),
        "the seeded echo memory must surface on the mock-embedded search"
    );

    let hit = records.iter().find(|r| r.text == echo).unwrap_or_else(|| {
        panic!("a returned record must carry the seeded echo text: {records:?}")
    });

    // Owner-scoped read: every returned record must belong to the caller, so a
    // dropped owner filter (cross-tenant leak) is caught even with one writer.
    let operator = operator_pubkey(home.path());
    assert_eq!(
        hit.owner.pubkey, operator,
        "the echo memory must be owned by the calling operator: {hit:?}"
    );
    for record in &records {
        assert_eq!(
            record.owner.pubkey, operator,
            "every returned record must be owned by the caller: {record:?}"
        );
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
