//! Live integration tests for the settlement receipt backfill mutation.
//! Each test spawns covenantd against a fresh tempdir HOME and drives the
//! mutation through real IPC, the on-disk receipts store, and the JSONL
//! audit log.
//!
//! Three scenarios:
//! - happy_path: operator with `settlement.backfill.apply` repairs two
//!   legacy rows; the rollback checkpoint sibling of the store carries
//!   the original content, the store is rewritten, and the
//!   `SettlementReceiptBackfillApplied` audit row surfaces on the
//!   operator feed with the matching row_count and rollback_path.
//! - unauthorized_scope: operator holds `settlement.backfill.dry_run`
//!   but requests an apply; the daemon rejects with a permission error
//!   that names the missing `settlement.backfill.apply` specifically,
//!   the store stays untouched, and no rollback file is written. The
//!   dry_run cap is granted first so the failure cannot be confused
//!   with a missing-grant arm.
//! - tamper_rollback: a legitimate apply lands the audit row, the
//!   daemon is killed, and the `SettlementReceiptBackfillApplied` row
//!   in `audit/events.jsonl` is mutated in place. The audit hash chain
//!   was computed over the original line, so `verify_integrity` must
//!   report `valid=false` with at least one failure. The rollback FILE
//!   itself is operational evidence outside the chain — what the chain
//!   actually guards is the durable audit row about the rollback, and
//!   that is the post-hoc target an attacker would alter.
//!
//! Hermetic — no external services. `#[ignore]`'d so they only run
//! under `--ignored live_`. Each test uses its own tempdir to avoid
//! racing under `--test-threads` parallelism.

use covenant_audit::{AuditKind, AuditLog, JsonlAuditLog};
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_types::{AgentId, ResourceKind, SettlementReceipt};
use std::path::{Path, PathBuf};
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

/// Write `count` legacy receipt rows (optional onchain fields removed)
/// into `<home>/receipts/working.jsonl` and return the raw content. The
/// backfill mutator's repair contract is precisely re-introducing those
/// missing keys via serde-decodable defaults, so removing them is what
/// makes a row a backfill candidate.
fn seed_legacy_rows(home: &Path, count: u64) -> String {
    let mut lines = String::new();
    for i in 0..count {
        let receipt = SettlementReceipt {
            id: Uuid::from_u128(0x5e7u128 + i as u128),
            payer: AgentId::new("user@local", [0u8; 32]),
            resource: ResourceKind::Memory,
            memory_record_id: None,
            credits_consumed: 7,
            settled_at: 7,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        };
        let mut value = serde_json::to_value(&receipt).unwrap();
        let obj = value.as_object_mut().unwrap();
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
        lines.push_str(&serde_json::to_string(&value).unwrap());
        lines.push('\n');
    }
    let receipts_dir = home.join("receipts");
    std::fs::create_dir_all(&receipts_dir).unwrap();
    std::fs::write(receipts_dir.join("working.jsonl"), &lines).unwrap();
    lines
}

fn rollback_files(home: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(home.join("receipts")) {
        Ok(entries) => entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().contains(".backfill-rollback-"))
                    .unwrap_or(false)
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + exercises settlement receipt backfill happy path over real IPC"]
async fn live_settlement_backfill_happy_path() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let original = seed_legacy_rows(home.path(), 2);
    let sock = home.path().join("sock");

    let mut stream = auth_as_operator(home.path(), &sock).await;
    grant(&mut stream, "settlement.backfill.apply").await;

    let resp = req(
        &mut stream,
        Request::BackfillSettlementReceipts {
            dry_run: false,
            scope_pubkey: None,
        },
    )
    .await;
    let (row_count, rollback_path, dry_run) = match resp {
        Response::SettlementReceiptsBackfilled {
            row_count,
            rollback_path,
            dry_run,
        } => (row_count, rollback_path, dry_run),
        other => {
            let _ = child.kill().await;
            panic!("unexpected backfill response: {other:?}");
        }
    };
    assert_eq!(row_count, 2, "two seeded legacy rows must be repaired");
    assert!(!dry_run, "apply must report dry_run=false");
    let rollback = rollback_path.expect("apply must return a rollback path");

    let store_path = home.path().join("receipts").join("working.jsonl");
    let after = std::fs::read_to_string(&store_path).expect("read store");
    assert_ne!(after, original, "apply must rewrite the store");
    assert_eq!(
        std::fs::read_to_string(&rollback).expect("read rollback"),
        original,
        "rollback checkpoint must hold the original content",
    );
    let checkpoints = rollback_files(home.path());
    assert_eq!(
        checkpoints.len(),
        1,
        "apply must write exactly one rollback file; got {checkpoints:?}",
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
            AuditKind::SettlementReceiptBackfillApplied {
                row_count,
                rollback_path,
                dry_run,
            } => Some((*row_count, rollback_path.clone(), *dry_run)),
            _ => None,
        })
        .expect("SettlementReceiptBackfillApplied row must surface on the operator feed");
    assert_eq!(row.0, 2, "audit row_count must match the response");
    assert_eq!(
        row.1.as_deref(),
        Some(rollback.as_str()),
        "audit row must reference the same rollback checkpoint the operator received",
    );
    assert!(!row.2, "audit dry_run must be false on an apply");

    let _ = child.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts dry_run capability does not authorize an apply"]
async fn live_settlement_backfill_unauthorized_scope() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let original = seed_legacy_rows(home.path(), 1);
    let sock = home.path().join("sock");

    let mut stream = auth_as_operator(home.path(), &sock).await;
    // Grant only the dry_run cap. An apply request must then be rejected
    // with a message naming the missing settlement.backfill.apply
    // specifically — a generic auth failure or a malformed-grant arm
    // would give false assurance about scope discipline.
    grant(&mut stream, "settlement.backfill.dry_run").await;

    let resp = req(
        &mut stream,
        Request::BackfillSettlementReceipts {
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
        message.contains("settlement.backfill.apply"),
        "rejection must name the missing apply cap; got {message:?}",
    );
    assert!(
        message.contains("requires capability"),
        "rejection must surface the requires-capability prefix; got {message:?}",
    );

    let store_path = home.path().join("receipts").join("working.jsonl");
    assert_eq!(
        std::fs::read_to_string(&store_path).expect("read store"),
        original,
        "a denied apply must not touch the store",
    );
    assert!(
        rollback_files(home.path()).is_empty(),
        "a denied apply must not write a rollback checkpoint",
    );

    let _ = child.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts the audit chain catches a tampered SettlementReceiptBackfillApplied row"]
async fn live_settlement_backfill_tamper_rollback() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    seed_legacy_rows(home.path(), 1);
    let sock = home.path().join("sock");

    let mut stream = auth_as_operator(home.path(), &sock).await;
    grant(&mut stream, "settlement.backfill.apply").await;
    match req(
        &mut stream,
        Request::BackfillSettlementReceipts {
            dry_run: false,
            scope_pubkey: None,
        },
    )
    .await
    {
        Response::SettlementReceiptsBackfilled { row_count, .. } => {
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
        "\"settlement_receipt_backfill_applied\",\"row_count\":1",
        "\"settlement_receipt_backfill_applied\",\"row_count\":999",
        1,
    );
    assert_ne!(
        tampered, original_events,
        "tamper substitution must find the SettlementReceiptBackfillApplied row in {original_events:?}",
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
    assert!(
        !report.failures.is_empty(),
        "report must surface at least one failure; report: {report:?}",
    );
}
