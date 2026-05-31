//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::RepairMemory` over the raw IPC socket — a `DeleteRecord` dry-run,
//! a non-draining re-read, then an apply that removes the record.
//!
//! The verb is covered today over the CLI (`live_cli_memory_repair.rs`) and HTTP
//! (`live_http_memory_repair_apply.rs`); the only socket coverage is the
//! delegate-denial gate (`live_memory_repair_compact_delegated_denial.rs`). This
//! pins the `Response::MemoryRepaired { outcome }` wire shape
//! (covenant-ipc/src/lib.rs:777) and the dry-run/apply contract: a dry-run
//! reports `would_change` without committing, an apply commits and the record
//! stops surfacing.
//!
//! Hermetic on any host: the daemon is pinned to the mock embedder via
//! `secrets.toml` before spawn, so a no-match intent's `phase 0 echo` memory is
//! found deterministically without probing a local Ollama. The write is indexed
//! asynchronously, so the seed read polls until it surfaces; deletion is
//! synchronous (`SqliteStore::delete` and `search_similar` share one guarded
//! store), so the post-apply read needs no poll. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_memory_repair -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_types::{MemoryRepairCommand, MemoryRepairMode, MemoryRepairRequest};
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
/// operator writes.
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
#[ignore = "live: spawns covenantd + drives Request::RepairMemory dry-run then apply over the socket"]
async fn live_ipc_memory_repair_dry_run_then_apply_round_trips() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    // Seed one owned record over the socket: a no-match intent makes the router
    // store `phase 0 echo (no agent matched): <text>` owned by the caller.
    grant(&mut stream, "memory.write").await;
    grant(&mut stream, "memory.read").await;
    match req(
        &mut stream,
        Request::SubmitIntent {
            text: "socket repair probe".into(),
            prefer_stream: None,
        },
    )
    .await
    {
        Response::IntentResult { .. } => {}
        other => panic!("expected Response::IntentResult, got {other:?}"),
    }

    // The write is indexed asynchronously; poll until the echo surfaces.
    let echo = "phase 0 echo (no agent matched): socket repair probe";
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
    let hit = records
        .iter()
        .find(|r| r.text == echo)
        .unwrap_or_else(|| panic!("the seeded echo memory must surface: {records:?}"));
    assert_eq!(
        hit.owner.pubkey,
        operator_pubkey(home.path()),
        "the echo memory must be owned by the calling operator: {hit:?}"
    );
    assert!(
        hit.parent.is_none(),
        "the seeded echo memory has no parent: {hit:?}"
    );
    let id = hit.id;

    // scope: None passes the repair scope gate.
    grant(&mut stream, "memory.repair.dry_run").await;
    grant(&mut stream, "memory.repair.apply").await;

    // Dry-run: reports the pending delete without committing it.
    match req(
        &mut stream,
        Request::RepairMemory {
            request: MemoryRepairRequest {
                mode: MemoryRepairMode::DryRun,
                command: MemoryRepairCommand::DeleteRecord { id },
                reason: "socket repair dry run".into(),
            },
        },
    )
    .await
    {
        Response::MemoryRepaired { outcome } => {
            assert_eq!(
                outcome.id, id,
                "the outcome must target the requested record"
            );
            assert_eq!(
                outcome.mode,
                MemoryRepairMode::DryRun,
                "mode must echo dry-run"
            );
            assert!(
                outcome.would_change,
                "deleting an existing record is a change"
            );
            assert!(!outcome.changed, "a dry-run must not commit");
            assert!(
                outcome.before.is_some(),
                "dry-run must report the pre-image"
            );
            assert!(outcome.after.is_none(), "a delete leaves no after-image");
        }
        other => panic!("expected Response::MemoryRepaired (dry-run), got {other:?}"),
    }

    // Non-draining: the dry-run must not have mutated the store.
    match req(&mut stream, search(echo)).await {
        Response::Memories { records } => assert!(
            records.iter().any(|r| r.id == id),
            "a dry-run must not mutate; the record must still surface: {records:?}"
        ),
        other => panic!("expected Response::Memories after dry-run, got {other:?}"),
    }

    // Apply: commits the delete.
    match req(
        &mut stream,
        Request::RepairMemory {
            request: MemoryRepairRequest {
                mode: MemoryRepairMode::Apply,
                command: MemoryRepairCommand::DeleteRecord { id },
                reason: "socket repair apply".into(),
            },
        },
    )
    .await
    {
        Response::MemoryRepaired { outcome } => {
            assert_eq!(
                outcome.mode,
                MemoryRepairMode::Apply,
                "mode must echo apply"
            );
            assert!(outcome.would_change, "the delete is still a change");
            assert!(outcome.changed, "an apply must commit the delete");
            assert!(outcome.after.is_none(), "a delete leaves no after-image");
        }
        other => panic!("expected Response::MemoryRepaired (apply), got {other:?}"),
    }

    // Gone: the applied delete is synchronous, so the record no longer surfaces.
    match req(&mut stream, search(echo)).await {
        Response::Memories { records } => assert!(
            !records.iter().any(|r| r.id == id),
            "the applied delete must remove the record from search: {records:?}"
        ),
        other => panic!("expected Response::Memories after apply, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
