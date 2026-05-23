//! Live integration tests for the memory record backfill mutation.
//! Each test pre-creates the operator identity + memory store + receipts
//! store inside a fresh tempdir HOME, spawns covenantd against it, and
//! drives the mutation through real IPC.
//!
//! Three scenarios:
//! - happy_path: operator with `memory.backfill.apply` repairs one
//!   pre-seeded memory record whose owner matches a pre-seeded legacy
//!   memory receipt (no `memory_record_id`); the response carries
//!   `row_count=1` with the canonical SAVEPOINT name, the on-disk
//!   memory store now carries `metadata.receipt_id` merged into the
//!   record, prior metadata keys are preserved, and the
//!   `MemoryRecordBackfillApplied` audit row surfaces on the operator
//!   feed with matching `row_count` and `savepoint_name`.
//! - unauthorized_scope: operator holds `memory.backfill.dry_run` but
//!   requests an apply; the daemon rejects with a permission error
//!   that names the missing `memory.backfill.apply` specifically, the
//!   memory record's metadata stays untouched on disk, and no apply
//!   audit row is emitted. The dry_run cap is granted first so the
//!   failure cannot be confused with a missing-grant arm.
//! - tamper_audit: a legitimate apply lands the audit row, the daemon
//!   is killed, and the `MemoryRecordBackfillApplied` row in
//!   `audit/events.jsonl` is mutated in place. The audit hash chain
//!   was computed over the original line, so `verify_integrity` must
//!   report `valid=false` with a per-entry digest-mismatch failure.
//!   The SAVEPOINT itself is in-memory only — what the chain actually
//!   guards is the durable audit row about the mutation, and that is
//!   the post-hoc target an attacker would alter.
//!
//! Hermetic — no external services. `#[ignore]`'d so they only run
//! under `--ignored live_`. Each test uses its own tempdir to avoid
//! racing under `--test-threads` parallelism.

use covenant_audit::{AuditKind, AuditLog, JsonlAuditLog};
use covenant_identity::LocalIdentity;
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_memory::{MemoryStore, SqliteStore};
use covenant_types::{MemoryRecord, MemoryTier, ResourceKind, SettlementReceipt};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
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

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

async fn authenticate(stream: &mut UnixStream, token_b58: &str) -> Response {
    req(
        stream,
        Request::Authenticate {
            token_b58: token_b58.to_string(),
        },
    )
    .await
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
    let sock = home.join("sock");
    if !wait_for_sock(&sock).await {
        panic!("daemon never created its socket at {}", sock.display());
    }
    child
}

async fn auth_as_operator(home: &Path, sock: &Path) -> UnixStream {
    let token = read_operator_token(home).await;
    let mut stream = UnixStream::connect(sock).await.expect("connect");
    match authenticate(&mut stream, &token).await {
        Response::Authenticated { .. } => stream,
        other => panic!("operator auth failed: {other:?}"),
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
        other => panic!("grant {action} failed: {other:?}"),
    }
}

/// Pre-create the operator identity, one legacy memory receipt (no
/// `memory_record_id`), and one matching uncorrelated memory record
/// (same owner pubkey as the receipt's payer) under `home`. Returns the
/// memory_record id so the caller can re-open the store post-mutation
/// and assert the merged metadata.
///
/// Order matters: the daemon takes ownership of the SQLite memory store
/// on spawn, so all writes go in before the daemon process exists. The
/// identity has to land first so the memory record's owner pubkey
/// matches what the daemon will load as its operator identity — without
/// this the backfill handler's `owner == peer.pubkey` server-side
/// filter would drop the record before the planner even sees it.
async fn seed_legacy_memory_and_receipt(home: &Path) -> Uuid {
    let identity_path = home.join("identity").join("local.key");
    let identity =
        LocalIdentity::load_or_create(&identity_path, "user@local").expect("create identity");
    let operator = identity.agent_id();

    let memory_id = Uuid::new_v4();
    let memory_path = home.join("memory.db");
    let store = SqliteStore::open(&memory_path).expect("open memory store");
    store
        .put(MemoryRecord {
            id: memory_id,
            tier: MemoryTier::Working,
            owner: operator.clone(),
            text: "legacy memory awaiting receipt".into(),
            embedding: Vec::new(),
            metadata: serde_json::json!({"note": "preserved on merge"}),
            created_at: 1,
            parent: None,
        })
        .await
        .expect("put memory record");
    drop(store);

    let receipt = SettlementReceipt {
        id: Uuid::new_v4(),
        payer: operator,
        resource: ResourceKind::Memory,
        memory_record_id: None,
        credits_consumed: 1,
        settled_at: 2,
        chain: None,
        cluster: None,
        batch_id: None,
        merkle_root: None,
        tx_sig: None,
        slot: None,
        confirmed_at: None,
        onchain_sig: None,
    };
    // Mirror the legacy on-wire shape the planner is meant to detect:
    // strip every optional chain field so the row reads as "no
    // memory_record_id" through serde's #[serde(default)] hinge.
    let mut value = serde_json::to_value(&receipt).expect("serialize receipt");
    let obj = value.as_object_mut().expect("receipt is a JSON object");
    for key in [
        "chain",
        "cluster",
        "batch_id",
        "merkle_root",
        "tx_sig",
        "slot",
        "confirmed_at",
        "onchain_sig",
    ] {
        obj.remove(key);
    }
    let line = format!("{}\n", serde_json::to_string(&value).expect("serialize"));
    let receipts_dir = home.join("receipts");
    std::fs::create_dir_all(&receipts_dir).expect("create receipts dir");
    std::fs::write(receipts_dir.join("working.jsonl"), &line).expect("write receipts");

    memory_id
}

async fn read_record_metadata(home: &Path, id: Uuid) -> serde_json::Value {
    let store = SqliteStore::open(&home.join("memory.db")).expect("reopen memory store");
    let record = store
        .get(id)
        .await
        .expect("memory get")
        .expect("record exists");
    record.metadata
}

#[tokio::test]
#[ignore = "live: spawns covenantd + exercises memory record backfill happy path over real IPC"]
async fn live_memory_backfill_happy_path() {
    let home = tempfile::tempdir().expect("tempdir");
    let memory_id = seed_legacy_memory_and_receipt(home.path()).await;
    let mut child = spawn_daemon(home.path()).await;
    let sock = home.path().join("sock");

    let mut stream = auth_as_operator(home.path(), &sock).await;
    grant(&mut stream, "memory.backfill.apply").await;

    let resp = req(
        &mut stream,
        Request::BackfillMemoryRecords {
            dry_run: false,
            scope_pubkey: None,
        },
    )
    .await;
    let (row_count, savepoint_name, dry_run) = match resp {
        Response::MemoryRecordsBackfilled {
            row_count,
            savepoint_name,
            dry_run,
        } => (row_count, savepoint_name, dry_run),
        other => {
            let _ = child.kill().await;
            panic!("unexpected backfill response: {other:?}");
        }
    };
    assert_eq!(row_count, 1, "seeded legacy memory row must be repaired");
    assert!(!dry_run, "apply must report dry_run=false");
    assert_eq!(
        savepoint_name,
        covenant_memory::MEMORY_BACKFILL_SAVEPOINT_NAME,
        "response must carry the canonical SAVEPOINT name so operators can join audit rows to integrity reports",
    );

    let audit = req(
        &mut stream,
        Request::RecentAudit {
            limit: 50,
            since_ms: None,
            prefer_stream: None,
        },
    )
    .await;
    let events = match audit {
        Response::AuditEvents { events } => events,
        other => {
            let _ = child.kill().await;
            panic!("unexpected audit response: {other:?}");
        }
    };
    let row = events
        .iter()
        .find_map(|e| match &e.kind {
            AuditKind::MemoryRecordBackfillApplied {
                row_count,
                savepoint_name,
                dry_run,
            } => Some((*row_count, savepoint_name.clone(), *dry_run)),
            _ => None,
        })
        .expect("MemoryRecordBackfillApplied row must surface on the operator feed");
    assert_eq!(row.0, 1, "audit row_count must match the response");
    assert_eq!(
        row.1.as_deref(),
        Some(savepoint_name.as_str()),
        "audit row must reference the same SAVEPOINT name the operator received",
    );
    assert!(!row.2, "audit dry_run must be false on an apply");

    // Drop the stream and kill the daemon so the SQLite write-ahead is
    // flushed and re-opening the store from the test does not race the
    // daemon's connection.
    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;

    let metadata = read_record_metadata(home.path(), memory_id).await;
    assert!(
        metadata
            .get("receipt_id")
            .and_then(|v| v.as_str())
            .is_some(),
        "apply must merge receipt_id into the record metadata; got {metadata:?}",
    );
    assert_eq!(
        metadata.get("note").and_then(|v| v.as_str()),
        Some("preserved on merge"),
        "apply must preserve pre-existing metadata keys; got {metadata:?}",
    );
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts dry_run capability does not authorize a memory backfill apply"]
async fn live_memory_backfill_unauthorized_scope() {
    let home = tempfile::tempdir().expect("tempdir");
    let memory_id = seed_legacy_memory_and_receipt(home.path()).await;
    let mut child = spawn_daemon(home.path()).await;
    let sock = home.path().join("sock");

    let mut stream = auth_as_operator(home.path(), &sock).await;
    // Grant only the dry_run cap. An apply request must then be rejected
    // with a message naming the missing memory.backfill.apply specifically
    // — a generic auth failure or a missing-grant arm would give false
    // assurance about scope discipline.
    grant(&mut stream, "memory.backfill.dry_run").await;

    let resp = req(
        &mut stream,
        Request::BackfillMemoryRecords {
            dry_run: false,
            scope_pubkey: None,
        },
    )
    .await;
    let message = match resp {
        Response::Error { message } => message,
        other => {
            let _ = child.kill().await;
            panic!("expected Response::Error, got {other:?}");
        }
    };
    assert!(
        message.contains("memory.backfill.apply"),
        "rejection must name the missing apply cap; got {message:?}",
    );
    assert!(
        message.contains("requires capability"),
        "rejection must surface the requires-capability prefix; got {message:?}",
    );

    let audit = req(
        &mut stream,
        Request::RecentAudit {
            limit: 50,
            since_ms: None,
            prefer_stream: None,
        },
    )
    .await;
    let events = match audit {
        Response::AuditEvents { events } => events,
        other => {
            let _ = child.kill().await;
            panic!("unexpected audit response: {other:?}");
        }
    };
    assert!(
        events.iter().all(|e| !matches!(
            e.kind,
            AuditKind::MemoryRecordBackfillApplied { dry_run: false, .. }
        )),
        "a denied apply must not emit a MemoryRecordBackfillApplied apply row; got {events:?}",
    );

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;

    let metadata = read_record_metadata(home.path(), memory_id).await;
    assert!(
        metadata.get("receipt_id").is_none(),
        "a denied apply must not touch the store; got {metadata:?}",
    );
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts the audit chain catches a tampered MemoryRecordBackfillApplied row"]
async fn live_memory_backfill_tamper_audit() {
    let home = tempfile::tempdir().expect("tempdir");
    seed_legacy_memory_and_receipt(home.path()).await;
    let mut child = spawn_daemon(home.path()).await;
    let sock = home.path().join("sock");

    let mut stream = auth_as_operator(home.path(), &sock).await;
    grant(&mut stream, "memory.backfill.apply").await;
    match req(
        &mut stream,
        Request::BackfillMemoryRecords {
            dry_run: false,
            scope_pubkey: None,
        },
    )
    .await
    {
        Response::MemoryRecordsBackfilled { row_count, .. } => {
            assert_eq!(row_count, 1, "one seeded row must be repaired");
        }
        other => {
            let _ = child.kill().await;
            panic!("unexpected backfill response: {other:?}");
        }
    }
    drop(stream);

    // Kill the daemon so the tamper cannot race a concurrent append and
    // so the on-disk events/chain pair reflects exactly what the apply
    // wrote.
    let _ = child.kill().await;
    let _ = child.wait().await;

    let events_path = home.path().join("audit").join("events.jsonl");
    let original_events = std::fs::read_to_string(&events_path).expect("read events");
    // Bump the recorded row_count from 1 to 999. The chain's recorded
    // event_hash_hex was sha256 over the original line; the recomputed
    // hash over the modified line will not match, and verify_integrity
    // must surface the mismatch.
    let tampered = original_events.replacen(
        "\"memory_record_backfill_applied\",\"row_count\":1",
        "\"memory_record_backfill_applied\",\"row_count\":999",
        1,
    );
    assert_ne!(
        tampered, original_events,
        "tamper substitution must find the MemoryRecordBackfillApplied row in {original_events:?}",
    );
    std::fs::write(&events_path, &tampered).expect("write tampered events");

    let log = JsonlAuditLog::open(events_path)
        .await
        .expect("open audit log");
    let report = log.verify_integrity().await.expect("verify_integrity ran");
    assert!(
        !report.valid,
        "tampered audit row must fail integrity check; report: {report:?}",
    );
    // Pin the SPECIFIC failure shape, not just any failure — a regression
    // that swapped the digest-mismatch path for a parse error or a
    // chain-length-mismatch would still produce !valid + non-empty
    // failures while losing the per-row tamper-detection contract the
    // memory backfill audit row depends on.
    assert!(
        report
            .failures
            .iter()
            .any(|f| f.contains("mismatch") && !f.contains("missing")),
        "report must surface a chain-entry digest mismatch; got failures: {:?}",
        report.failures,
    );
}
