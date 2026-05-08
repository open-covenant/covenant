//! Append-only audit log.
//!
//! Every intent dispatch, capability check, capability grant, and
//! capability revocation produces one [`AuditEvent`]. Wire format is
//! JSONL — one event per line, easy to tail or grep — and the
//! [`AuditLog`] trait abstracts over a JSONL-backed implementation
//! suitable for production and an in-memory implementation suitable
//! for tests.

#![deny(unsafe_code)]

use async_trait::async_trait;
use covenant_types::AgentId;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditEvent {
    pub id: Uuid,
    pub timestamp_ms: u64,
    pub issuer: AgentId,
    pub kind: AuditKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuditKind {
    IntentDispatched {
        intent_id: Uuid,
        intent_text: String,
        matched_agent: Option<String>,
        result_hash_hex: String,
        status: String,
    },
    CapabilityCheck {
        agent_id: String,
        required_actions: Vec<String>,
        missing_actions: Vec<String>,
        passed: bool,
    },
    CapabilityGranted {
        subject_display: String,
        action: String,
        granted_by_display: String,
        signature_b58: String,
    },
    IntentIgnored {
        intent_id: Uuid,
        intent_text: String,
        matched_pattern: String,
    },
    /// Logged when `PostA2AResult` is rejected upstream of any
    /// capability check — e.g. the supplied `task_id` was never
    /// dispatched through this daemon. Stronger compromise indicator
    /// than a missing-cap rejection: no honest agent generates a
    /// nonexistent `task_id`.
    A2AResultRejected { task_id: Uuid, reason: String },
    /// Logged on every rejected authentication attempt — bad first
    /// frame on the IPC socket, missing/malformed `Authorization`
    /// header on HTTP, or a token the registry doesn't resolve.
    /// `transport` is `"ipc"` or `"http"`; `reason` is the same
    /// short message the caller saw.
    AuthenticationFailed { transport: String, reason: String },
    /// Logged when `SendA2ATask` is rejected because the supplied
    /// `task.sender` does not match the authenticated peer on the
    /// connection. Closes the spoof attack class flagged in the
    /// Sprint 47 security review.
    A2ASenderMismatch {
        peer_display: String,
        claimed_sender_display: String,
    },
    /// Logged when `RevokeCapability` is rejected because the
    /// authenticated peer is not the subject of the capability they
    /// asked to revoke. Closes the cross-peer-revoke gap flagged in
    /// the Sprint 49 mid-sprint security review.
    CapabilityRevokeRejected {
        signature_b58: String,
        reason: String,
    },
    /// Logged when `dispatch_intent` rejects an intent because the
    /// matched agent's budget bucket is exhausted. `agent_display` is
    /// the synthesized `AgentId.display` for the matched agent (e.g.
    /// `research@agent`); `requested` is the credit cost the daemon
    /// tried to debit; `tokens_remaining` is what the bucket actually
    /// had at the moment of the check (precise `u64`; the wire response
    /// rounds to a coarse bucket per the Sprint 58c L3 closure);
    /// `refill_eta_ms` is the wall time until the bucket can satisfy
    /// `requested` again; `intent_text` carries the rejected intent so
    /// `covenant intents resume <intent-id>` can re-dispatch from this
    /// row alone — the audit log is the resume queue.
    BudgetExhausted {
        agent_display: String,
        intent_id: Uuid,
        intent_text: String,
        requested: u64,
        tokens_remaining: u64,
        refill_eta_ms: u64,
    },
    /// Logged when `dispatch_intent` falls into the NoCapacity fail-open
    /// arm: the manifest opted in to budget enforcement
    /// (`budget_credits_per_hour > 0`) but no bucket was seeded for the
    /// agent — the operator forgot to call `register_agent_budgets`, or
    /// a hot-reload added the manifest without re-seeding. v0 logs and
    /// passes. Distinct from [`AuditKind::BudgetExhausted`] so /audit
    /// consumers can filter operator-misconfig vs. policy-rejection
    /// without special-casing sentinel values.
    BudgetUnseeded {
        agent_display: String,
        intent_id: Uuid,
        requested: u64,
    },
}

#[async_trait]
pub trait AuditLog: Send + Sync {
    async fn record(&self, event: AuditEvent) -> Result<(), AuditError>;
    async fn recent(&self, limit: usize) -> Result<Vec<AuditEvent>, AuditError>;
    /// Drop every event with `timestamp_ms < before_ms`. Returns the
    /// count deleted. Operator-driven retention: with no purge call the
    /// log grows unbounded for the lifetime of the daemon. Mirrors the
    /// `MemoryStore::purge_older_than` shape from Sprint 19.
    async fn purge_older_than(&self, before_ms: u64) -> Result<u64, AuditError>;
}

pub struct JsonlAuditLog {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl JsonlAuditLog {
    pub async fn open(path: PathBuf) -> Result<Self, AuditError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        Ok(Self {
            path,
            lock: Arc::new(Mutex::new(())),
        })
    }
}

#[async_trait]
impl AuditLog for JsonlAuditLog {
    async fn record(&self, event: AuditEvent) -> Result<(), AuditError> {
        let _g = self.lock.lock().await;
        let line = serde_json::to_string(&event)?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        f.write_all(line.as_bytes()).await?;
        f.write_all(b"\n").await?;
        f.flush().await?;
        Ok(())
    }

    async fn recent(&self, limit: usize) -> Result<Vec<AuditEvent>, AuditError> {
        let _g = self.lock.lock().await;
        let f = match fs::File::open(&self.path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut reader = BufReader::new(f);
        let mut all = Vec::new();
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break;
            }
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                continue;
            }
            all.push(serde_json::from_str(trimmed)?);
        }
        let start = all.len().saturating_sub(limit);
        Ok(all.split_off(start))
    }

    async fn purge_older_than(&self, before_ms: u64) -> Result<u64, AuditError> {
        // Read-filter-rewrite under the same lock that record uses, so a
        // concurrent record can't race against the rewrite. Atomicity of
        // the rewrite comes from `tempfile + rename` — readers see either
        // the old log or the new one, never a partial rewrite.
        let _g = self.lock.lock().await;
        let existing: Vec<AuditEvent> = match fs::read_to_string(&self.path).await {
            Ok(s) => s
                .lines()
                .filter(|l| !l.is_empty())
                .map(serde_json::from_str)
                .collect::<Result<Vec<_>, _>>()?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        let kept: Vec<&AuditEvent> = existing
            .iter()
            .filter(|e| e.timestamp_ms >= before_ms)
            .collect();
        let purged = (existing.len() - kept.len()) as u64;
        if purged == 0 {
            return Ok(0);
        }
        let tmp_path = self.path.with_extension("jsonl.tmp");
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)
            .await?;
        for e in &kept {
            let line = serde_json::to_string(e)?;
            f.write_all(line.as_bytes()).await?;
            f.write_all(b"\n").await?;
        }
        f.flush().await?;
        drop(f);
        fs::rename(&tmp_path, &self.path).await?;
        Ok(purged)
    }
}

#[derive(Default)]
pub struct InMemoryAuditLog {
    events: Mutex<Vec<AuditEvent>>,
}

impl InMemoryAuditLog {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl AuditLog for InMemoryAuditLog {
    async fn record(&self, event: AuditEvent) -> Result<(), AuditError> {
        self.events.lock().await.push(event);
        Ok(())
    }

    async fn recent(&self, limit: usize) -> Result<Vec<AuditEvent>, AuditError> {
        let g = self.events.lock().await;
        let start = g.len().saturating_sub(limit);
        Ok(g[start..].to_vec())
    }

    async fn purge_older_than(&self, before_ms: u64) -> Result<u64, AuditError> {
        let mut g = self.events.lock().await;
        let len_before = g.len();
        g.retain(|e| e.timestamp_ms >= before_ms);
        Ok((len_before - g.len()) as u64)
    }
}

pub fn hash_hex(bytes: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy(kind: AuditKind) -> AuditEvent {
        AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: 0,
            issuer: AgentId::new("user@local", [0u8; 32]),
            kind,
        }
    }

    fn intent_kind(status: &str) -> AuditKind {
        AuditKind::IntentDispatched {
            intent_id: Uuid::new_v4(),
            intent_text: "find x".into(),
            matched_agent: Some("research".into()),
            result_hash_hex: hash_hex(b"some result"),
            status: status.into(),
        }
    }

    #[tokio::test]
    async fn in_memory_record_and_recent() {
        let log = InMemoryAuditLog::new();
        log.record(dummy(intent_kind("ok"))).await.unwrap();
        log.record(dummy(intent_kind("ok"))).await.unwrap();
        log.record(dummy(intent_kind("error"))).await.unwrap();
        let last_two = log.recent(2).await.unwrap();
        assert_eq!(last_two.len(), 2);
        match &last_two[1].kind {
            AuditKind::IntentDispatched { status, .. } => assert_eq!(status, "error"),
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    #[tokio::test]
    async fn jsonl_round_trip_through_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = JsonlAuditLog::open(path.clone()).await.unwrap();
        log.record(dummy(intent_kind("ok"))).await.unwrap();
        log.record(dummy(intent_kind("ok"))).await.unwrap();

        let log2 = JsonlAuditLog::open(path.clone()).await.unwrap();
        let recent = log2.recent(10).await.unwrap();
        assert_eq!(recent.len(), 2);

        let raw = std::fs::read_to_string(&path).unwrap();
        let lines = raw.lines().filter(|l| !l.is_empty()).count();
        assert_eq!(lines, 2);
    }

    #[tokio::test]
    async fn jsonl_recent_on_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = JsonlAuditLog::open(path.clone()).await.unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(log.recent(10).await.unwrap().is_empty());
    }

    #[test]
    fn hash_hex_is_stable_for_same_input() {
        assert_eq!(hash_hex(b"hello"), hash_hex(b"hello"));
        assert_ne!(hash_hex(b"hello"), hash_hex(b"world"));
    }

    #[test]
    fn audit_event_round_trips_through_serde() {
        let e = dummy(intent_kind("ok"));
        let json = serde_json::to_string(&e).unwrap();
        let back: AuditEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    fn dated(ts: u64) -> AuditEvent {
        AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: ts,
            issuer: AgentId::new("user@local", [0u8; 32]),
            kind: intent_kind("ok"),
        }
    }

    #[tokio::test]
    async fn in_memory_purge_drops_old_events_and_keeps_new() {
        let log = InMemoryAuditLog::new();
        log.record(dated(100)).await.unwrap();
        log.record(dated(200)).await.unwrap();
        log.record(dated(300)).await.unwrap();
        let purged = log.purge_older_than(250).await.unwrap();
        assert_eq!(purged, 2);
        let remaining = log.recent(10).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].timestamp_ms, 300);
    }

    #[tokio::test]
    async fn jsonl_purge_rewrites_only_when_something_drops() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = JsonlAuditLog::open(path.clone()).await.unwrap();
        log.record(dated(100)).await.unwrap();
        log.record(dated(200)).await.unwrap();
        log.record(dated(300)).await.unwrap();

        let purged = log.purge_older_than(150).await.unwrap();
        assert_eq!(purged, 1);
        // Re-open to confirm the rewrite landed on disk and the survivors
        // can still be parsed back.
        let log2 = JsonlAuditLog::open(path.clone()).await.unwrap();
        let kept = log2.recent(10).await.unwrap();
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|e| e.timestamp_ms >= 150));
    }

    #[tokio::test]
    async fn jsonl_purge_no_op_when_nothing_old() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = JsonlAuditLog::open(path.clone()).await.unwrap();
        log.record(dated(100)).await.unwrap();
        log.record(dated(200)).await.unwrap();
        let purged = log.purge_older_than(50).await.unwrap();
        assert_eq!(purged, 0);
        // No tempfile.tmp left lying around — atomic-rename path skipped.
        assert!(!path.with_extension("jsonl.tmp").exists());
        let kept = log.recent(10).await.unwrap();
        assert_eq!(kept.len(), 2);
    }

    #[tokio::test]
    async fn jsonl_purge_on_missing_file_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = JsonlAuditLog::open(path.clone()).await.unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(log.purge_older_than(1_000_000).await.unwrap(), 0);
    }
}
