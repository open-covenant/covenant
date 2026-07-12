//! Three-tier memory store for Covenant.
//!
//! [`MemoryRecord`] values live in one of three tiers — working,
//! episodic, or long-term — backed by SQLite for persistence with an
//! in-memory implementation suitable for tests. The [`MemoryStore`]
//! trait covers recent-record reads, embedded-vector cosine search,
//! tier-scoped purge ([`MemoryStore::purge_older_than`]), bounded
//! compaction ([`MemoryStore::compact`]), and scoped repair commands
//! ([`MemoryStore::repair`]).

#![deny(unsafe_code)]

pub mod ignore;
pub use ignore::{IgnorePattern, IgnoreSet, IgnoreVerdict};

use async_trait::async_trait;
pub use covenant_types::{
    MemoryCompactionOutcome, MemoryCompactionPolicy, MemoryCompactionRequest, MemoryRepairAction,
    MemoryRepairCommand, MemoryRepairMode, MemoryRepairOutcome, MemoryRepairRequest,
};
use covenant_types::{MemoryRecord, MemoryTier, ResourceKind, SettlementReceipt};
use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use std::sync::Mutex;
use tokio::task;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("worker: {0}")]
    Worker(String),
    #[error("memory record {0} not found")]
    RecordNotFound(Uuid),
    #[error("parent mismatch for memory {id}: expected {expected:?}, actual {actual:?}")]
    ParentMismatch {
        id: Uuid,
        expected: Option<Uuid>,
        actual: Option<Uuid>,
    },
    #[error("invalid memory repair request: {0}")]
    InvalidRepair(String),
    #[error("invalid memory compaction request: {0}")]
    InvalidCompaction(String),
}

/// One memory-record-to-receipt correlation queued for backfill into the
/// memory store's `metadata.receipt_id` field. Produced by the CLI
/// `memory plan-receipt-backfill` planner and consumed by
/// [`SqliteStore::backfill_receipt_correlation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryReceiptBackfillCorrelation {
    pub memory_record_id: Uuid,
    pub receipt_id: Uuid,
}

/// Outcome of one [`SqliteStore::backfill_receipt_correlation`] call.
/// `row_count` is the number of rows that actually changed (apply mode)
/// or would change (dry-run mode). `savepoint_name` is the SQLite
/// SAVEPOINT identifier the mutator wraps each batch in so a per-row
/// failure rolls back the entire batch atomically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillReceiptCorrelationOutcome {
    pub row_count: u64,
    pub savepoint_name: String,
    pub dry_run: bool,
}

/// SAVEPOINT identifier wrapping every backfill batch. Fixed so the
/// audit row's savepoint_name field is stable across releases.
pub const MEMORY_BACKFILL_SAVEPOINT_NAME: &str = "backfill_receipt_correlation";

/// SAVEPOINT identifier wrapping every SqliteStore compact apply. Mirrors
/// the backfill name so an operator inspecting the SQLite trace sees a
/// stable, audit-grade label for each transactional boundary.
pub const MEMORY_COMPACT_SAVEPOINT_NAME: &str = "compact_apply";

#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn put(&self, record: MemoryRecord) -> Result<(), MemoryError>;
    async fn get(&self, id: Uuid) -> Result<Option<MemoryRecord>, MemoryError>;
    async fn all(&self) -> Result<Vec<MemoryRecord>, MemoryError>;
    async fn recent(
        &self,
        tier: Option<MemoryTier>,
        limit: usize,
    ) -> Result<Vec<MemoryRecord>, MemoryError>;
    async fn delete(&self, id: Uuid) -> Result<bool, MemoryError>;
    /// Score every record's `embedding` against `query_embedding` via cosine
    /// similarity and return the top `limit`, optionally filtered by tier.
    /// Records with empty embeddings get score 0 and are returned last (or
    /// dropped, depending on the impl). `min_relevance` is an optional
    /// threshold in `[0.0, 1.0]`: when set, records whose cosine score is
    /// strictly less than the threshold are dropped before the `limit`
    /// truncation, so a high threshold can yield fewer rows than `limit`
    /// even when the unfiltered set is larger. v0 does an in-process
    /// linear scan; LanceDB / sqlite-vec arrive later.
    async fn search_similar(
        &self,
        query_embedding: Vec<f32>,
        tier: Option<MemoryTier>,
        limit: usize,
        min_relevance: Option<f32>,
    ) -> Result<Vec<MemoryRecord>, MemoryError>;
    /// Purge records whose `created_at` is strictly older than `before_ms`,
    /// optionally restricted to a tier. Returns the count deleted. Closes
    /// the spec §11 working-tier-GC pin (clear at task completion is still
    /// the long-term shape; for v0 the operator drives this on a TTL).
    async fn purge_older_than(
        &self,
        tier: Option<MemoryTier>,
        before_ms: u64,
    ) -> Result<u64, MemoryError>;
    /// Operator-controlled repair for verifier drift findings. Dry-run
    /// returns the exact before/after shape without mutating the store;
    /// apply performs the mutation only after the same checks pass.
    async fn repair(
        &self,
        request: MemoryRepairRequest,
    ) -> Result<MemoryRepairOutcome, MemoryError> {
        validate_repair_request(&request)?;
        let id = request.command.id();
        let before = self.get(id).await?.ok_or(MemoryError::RecordNotFound(id))?;
        let action = request.command.action();
        let after = plan_repair(&before, &request.command)?;
        let would_change = after.as_ref() != Some(&before);

        if request.mode == MemoryRepairMode::Apply && would_change {
            match &after {
                Some(record) => self.put(record.clone()).await?,
                None => {
                    let _ = self.delete(id).await?;
                }
            }
        }

        Ok(MemoryRepairOutcome {
            id,
            action,
            mode: request.mode,
            would_change,
            changed: request.mode == MemoryRepairMode::Apply && would_change,
            before: Some(before),
            after,
        })
    }
    /// Deterministic memory compaction. Dry-run and apply compute the
    /// same plan over a single snapshot: expired working/episodic records
    /// are deleted, old long-term records are marked stale instead of
    /// deleted, and parent references are detached when their target is
    /// absent or deleted by the same plan.
    async fn compact(
        &self,
        request: MemoryCompactionRequest,
    ) -> Result<MemoryCompactionOutcome, MemoryError> {
        validate_compaction_request(&request)?;
        let records = self.all().await?;
        let (outcome, updates) = plan_compaction(&records, &request);

        if request.mode == MemoryRepairMode::Apply && outcome.would_change {
            for id in &outcome.deleted {
                let _ = self.delete(*id).await?;
            }
            for record in updates {
                self.put(record).await?;
            }
        }

        Ok(outcome)
    }

    /// Apply legacy receipt-id correlations to the `metadata.receipt_id`
    /// field of each named memory record. The default impl walks the
    /// trait's [`MemoryStore::get`] / [`MemoryStore::put`] pair and is
    /// non-atomic — a per-row failure mid-batch leaves prior rows
    /// committed. [`SqliteStore`] overrides this with a named
    /// [`MEMORY_BACKFILL_SAVEPOINT_NAME`] SAVEPOINT so a failure rolls
    /// the entire batch back to zero rows changed.
    ///
    /// `dry_run` reports the row_count an apply would change without
    /// writing; the count excludes correlations that would not change
    /// stored metadata (idempotent re-runs).
    async fn backfill_receipt_correlation(
        &self,
        dry_run: bool,
        correlations: Vec<MemoryReceiptBackfillCorrelation>,
    ) -> Result<BackfillReceiptCorrelationOutcome, MemoryError> {
        let mut row_count: u64 = 0;
        for correlation in &correlations {
            let record = self
                .get(correlation.memory_record_id)
                .await?
                .ok_or(MemoryError::RecordNotFound(correlation.memory_record_id))?;
            let current = record.metadata.clone();
            let next = merge_receipt_id(current.clone(), correlation.receipt_id);
            if next == current {
                continue;
            }
            row_count += 1;
            if !dry_run {
                let mut updated = record;
                updated.metadata = next;
                self.put(updated).await?;
            }
        }
        Ok(BackfillReceiptCorrelationOutcome {
            row_count,
            savepoint_name: MEMORY_BACKFILL_SAVEPOINT_NAME.into(),
            dry_run,
        })
    }
}

fn validate_repair_request(request: &MemoryRepairRequest) -> Result<(), MemoryError> {
    if request.reason.trim().is_empty() {
        return Err(MemoryError::InvalidRepair(
            "reason must not be empty".into(),
        ));
    }
    if let MemoryRepairCommand::BackfillProvenance { provenance, .. } = &request.command {
        if provenance.is_null() {
            return Err(MemoryError::InvalidRepair(
                "provenance must not be null".into(),
            ));
        }
    }
    Ok(())
}

fn validate_compaction_request(request: &MemoryCompactionRequest) -> Result<(), MemoryError> {
    if request.reason.trim().is_empty() {
        return Err(MemoryError::InvalidCompaction(
            "reason must not be empty".into(),
        ));
    }
    if request.policy.is_empty() {
        return Err(MemoryError::InvalidCompaction(
            "policy must enable at least one compaction action".into(),
        ));
    }
    Ok(())
}

fn plan_repair(
    record: &MemoryRecord,
    command: &MemoryRepairCommand,
) -> Result<Option<MemoryRecord>, MemoryError> {
    match command {
        MemoryRepairCommand::DetachParent {
            expected_parent, ..
        } => {
            if expected_parent.is_some() && record.parent != *expected_parent {
                return Err(MemoryError::ParentMismatch {
                    id: record.id,
                    expected: *expected_parent,
                    actual: record.parent,
                });
            }
            let mut after = record.clone();
            after.parent = None;
            Ok(Some(after))
        }
        MemoryRepairCommand::DeleteRecord { .. } => Ok(None),
        MemoryRepairCommand::BackfillProvenance { provenance, .. } => {
            let mut after = record.clone();
            let mut metadata = match after.metadata {
                serde_json::Value::Object(map) => map,
                other => {
                    let mut map = serde_json::Map::new();
                    map.insert("previous_metadata".into(), other);
                    map
                }
            };
            metadata.insert("provenance".into(), provenance.clone());
            after.metadata = serde_json::Value::Object(metadata);
            Ok(Some(after))
        }
    }
}

fn plan_compaction(
    records: &[MemoryRecord],
    request: &MemoryCompactionRequest,
) -> (MemoryCompactionOutcome, Vec<MemoryRecord>) {
    let mut deleted: BTreeSet<Uuid> = BTreeSet::new();
    for record in records {
        match record.tier {
            MemoryTier::Working
                if request
                    .policy
                    .delete_working_before_ms
                    .is_some_and(|before| record.created_at < before) =>
            {
                deleted.insert(record.id);
            }
            MemoryTier::Episodic
                if request
                    .policy
                    .delete_episodic_before_ms
                    .is_some_and(|before| record.created_at < before) =>
            {
                deleted.insert(record.id);
            }
            _ => {}
        }
    }

    let retained: BTreeSet<Uuid> = records
        .iter()
        .filter(|record| !deleted.contains(&record.id))
        .map(|record| record.id)
        .collect();

    let mut updates = Vec::new();
    let mut stale_marked = Vec::new();
    let mut parents_detached = Vec::new();
    for record in records {
        if deleted.contains(&record.id) {
            continue;
        }

        let mut after = record.clone();
        let mut changed = false;

        if request.policy.detach_stale_parents {
            if let Some(parent) = after.parent {
                if !retained.contains(&parent) {
                    after.parent = None;
                    parents_detached.push(after.id);
                    changed = true;
                }
            }
        }

        if after.tier == MemoryTier::LongTerm {
            if let Some(before_ms) = request.policy.mark_longterm_stale_before_ms {
                if after.created_at < before_ms {
                    let marked_at_ms = request.policy.marked_at_ms.unwrap_or(before_ms);
                    let stale_context = serde_json::json!({
                        "marked_at_ms": marked_at_ms,
                        "reason": request.reason,
                    });
                    let mut metadata = match after.metadata {
                        serde_json::Value::Object(map) => map,
                        other => {
                            let mut map = serde_json::Map::new();
                            map.insert("previous_metadata".into(), other);
                            map
                        }
                    };
                    if metadata.get("stale_context") != Some(&stale_context) {
                        metadata.insert("stale_context".into(), stale_context);
                        after.metadata = serde_json::Value::Object(metadata);
                        stale_marked.push(after.id);
                        changed = true;
                    } else {
                        after.metadata = serde_json::Value::Object(metadata);
                    }
                }
            }
        }

        if changed {
            updates.push(after);
        }
    }

    let mut deleted: Vec<Uuid> = deleted.into_iter().collect();
    deleted.sort();
    stale_marked.sort();
    parents_detached.sort();
    updates.sort_by_key(|record| record.id);
    let would_change =
        !deleted.is_empty() || !stale_marked.is_empty() || !parents_detached.is_empty();
    (
        MemoryCompactionOutcome {
            mode: request.mode,
            would_change,
            changed: request.mode == MemoryRepairMode::Apply && would_change,
            deleted,
            stale_marked,
            parents_detached,
        },
        updates,
    )
}

/// One pairing produced by the legacy-receipt → memory-record matcher.
/// Borrowed so the JSON and correlation surfaces can render their own
/// projections without re-running the algorithm.
struct ReceiptMemoryMatch<'a> {
    pairs: Vec<(&'a SettlementReceipt, &'a MemoryRecord)>,
    unmatched_legacy_receipts: Vec<&'a SettlementReceipt>,
    unmatched_memory_records: Vec<&'a MemoryRecord>,
}

/// Pair uncorrelated legacy memory-resource receipts (those whose
/// `memory_record_id` is None) with uncorrelated memory records sharing
/// the same payer/owner pubkey. The first eligible memory record (in
/// slice order) wins for each receipt; correlated rows on either side
/// are excluded so a repeat run does not double-bind.
fn match_legacy_receipts_to_memory_records<'a>(
    memories: &'a [MemoryRecord],
    receipts: &'a [SettlementReceipt],
) -> ReceiptMemoryMatch<'a> {
    let memory_receipts: Vec<&SettlementReceipt> = receipts
        .iter()
        .filter(|receipt| receipt.resource == ResourceKind::Memory)
        .collect();
    let correlated: HashSet<Uuid> = memory_receipts
        .iter()
        .filter_map(|receipt| receipt.memory_record_id)
        .collect();
    let legacy: Vec<&SettlementReceipt> = memory_receipts
        .iter()
        .copied()
        .filter(|receipt| receipt.memory_record_id.is_none())
        .collect();

    let mut used_memory: HashSet<Uuid> = HashSet::new();
    let mut pairs: Vec<(&SettlementReceipt, &MemoryRecord)> = Vec::new();
    let mut unmatched_legacy: Vec<&SettlementReceipt> = Vec::new();
    for receipt in &legacy {
        let candidate = memories.iter().find(|memory| {
            memory.owner.pubkey == receipt.payer.pubkey
                && !correlated.contains(&memory.id)
                && !used_memory.contains(&memory.id)
        });
        if let Some(memory) = candidate {
            used_memory.insert(memory.id);
            pairs.push((*receipt, memory));
        } else {
            unmatched_legacy.push(*receipt);
        }
    }
    let unmatched_memory: Vec<&MemoryRecord> = memories
        .iter()
        .filter(|memory| !correlated.contains(&memory.id) && !used_memory.contains(&memory.id))
        .collect();

    ReceiptMemoryMatch {
        pairs,
        unmatched_legacy_receipts: unmatched_legacy,
        unmatched_memory_records: unmatched_memory,
    }
}

fn memory_tier_slug(tier: MemoryTier) -> &'static str {
    match tier {
        MemoryTier::Working => "working",
        MemoryTier::Episodic => "episodic",
        MemoryTier::LongTerm => "longterm",
    }
}

/// Read-only planner envelope for `covenant memory plan-receipt-backfill`.
/// Returns the stable `memory_receipt_backfill_plan` JSON shape: candidate
/// pairings, unmatched legacy receipts, unmatched memory records, and a
/// refusal note carried over from the pre-mutator contract. Pure function
/// over the supplied snapshots; performs no I/O.
pub fn memory_receipt_backfill_plan_json(
    limit: usize,
    memories: &[MemoryRecord],
    receipts: &[SettlementReceipt],
) -> serde_json::Value {
    let matched = match_legacy_receipts_to_memory_records(memories, receipts);
    let records: Vec<serde_json::Value> = matched
        .pairs
        .iter()
        .map(|(receipt, memory)| {
            serde_json::json!({
                "receipt_id": receipt.id,
                "memory_record_id": memory.id,
                "payer_display": receipt.payer.display,
                "payer_pubkey": receipt.payer.pubkey_base58(),
                "memory_owner_display": memory.owner.display,
                "memory_owner_pubkey": memory.owner.pubkey_base58(),
                "credits_consumed": receipt.credits_consumed,
                "status": "candidate",
                "reason": "legacy memory receipt has no memory_record_id and the same owner has an uncorrelated memory record"
            })
        })
        .collect();
    let unmatched_legacy_receipts: Vec<serde_json::Value> = matched
        .unmatched_legacy_receipts
        .iter()
        .map(|receipt| {
            serde_json::json!({
                "receipt_id": receipt.id,
                "payer_display": receipt.payer.display,
                "payer_pubkey": receipt.payer.pubkey_base58(),
                "credits_consumed": receipt.credits_consumed,
                "reason": "no uncorrelated memory record for receipt payer in the requested window"
            })
        })
        .collect();
    let unmatched_memory_records: Vec<serde_json::Value> = matched
        .unmatched_memory_records
        .iter()
        .map(|memory| {
            serde_json::json!({
                "memory_record_id": memory.id,
                "owner_display": memory.owner.display,
                "owner_pubkey": memory.owner.pubkey_base58(),
                "tier": memory_tier_slug(memory.tier),
                "reason": "no legacy receipt candidate for memory owner in the requested window"
            })
        })
        .collect();

    serde_json::json!({
        "kind": "memory_receipt_backfill_plan",
        "mode": "dry_run",
        "limit": limit,
        "mutation_supported": false,
        "records": records,
        "unmatched_legacy_receipts": unmatched_legacy_receipts,
        "unmatched_memory_records": unmatched_memory_records,
        "refusal": {
            "apply_supported": false,
            "reason": "receipt backfill mutation is not implemented; review this plan before adding an explicit mutation path with audit evidence"
        }
    })
}

/// Typed correlation list consumed by
/// [`SqliteStore::backfill_receipt_correlation`]. Pairs match
/// [`memory_receipt_backfill_plan_json`] one-for-one so the planner's
/// dry-run preview and an apply over the same snapshots cannot diverge.
pub fn memory_receipt_backfill_correlations(
    memories: &[MemoryRecord],
    receipts: &[SettlementReceipt],
) -> Vec<MemoryReceiptBackfillCorrelation> {
    match_legacy_receipts_to_memory_records(memories, receipts)
        .pairs
        .into_iter()
        .map(|(receipt, memory)| MemoryReceiptBackfillCorrelation {
            memory_record_id: memory.id,
            receipt_id: receipt.id,
        })
        .collect()
}

/// Cosine similarity over two equal-length vectors. Returns 0.0 for any
/// degenerate input (mismatched length, zero norm, empty).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0_f32;
    let mut na = 0.0_f32;
    let mut nb = 0.0_f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// In-memory backend (tests, defaults when persistence is disabled).
#[derive(Default)]
pub struct InMemoryStore {
    records: Mutex<Vec<MemoryRecord>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl MemoryStore for InMemoryStore {
    async fn put(&self, record: MemoryRecord) -> Result<(), MemoryError> {
        let mut g = self
            .records
            .lock()
            .map_err(|e| MemoryError::Worker(e.to_string()))?;
        g.retain(|r| r.id != record.id);
        g.push(record);
        Ok(())
    }

    async fn get(&self, id: Uuid) -> Result<Option<MemoryRecord>, MemoryError> {
        let g = self
            .records
            .lock()
            .map_err(|e| MemoryError::Worker(e.to_string()))?;
        Ok(g.iter().find(|r| r.id == id).cloned())
    }

    async fn all(&self) -> Result<Vec<MemoryRecord>, MemoryError> {
        let g = self
            .records
            .lock()
            .map_err(|e| MemoryError::Worker(e.to_string()))?;
        Ok(g.clone())
    }

    async fn recent(
        &self,
        tier: Option<MemoryTier>,
        limit: usize,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        let g = self
            .records
            .lock()
            .map_err(|e| MemoryError::Worker(e.to_string()))?;
        let mut out: Vec<MemoryRecord> = g
            .iter()
            .filter(|r| tier.is_none_or(|t| r.tier == t))
            .cloned()
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        out.truncate(limit);
        Ok(out)
    }

    async fn delete(&self, id: Uuid) -> Result<bool, MemoryError> {
        let mut g = self
            .records
            .lock()
            .map_err(|e| MemoryError::Worker(e.to_string()))?;
        let len_before = g.len();
        g.retain(|r| r.id != id);
        Ok(g.len() != len_before)
    }

    async fn search_similar(
        &self,
        query_embedding: Vec<f32>,
        tier: Option<MemoryTier>,
        limit: usize,
        min_relevance: Option<f32>,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        let g = self
            .records
            .lock()
            .map_err(|e| MemoryError::Worker(e.to_string()))?;
        let floor = min_relevance.unwrap_or(0.0).max(0.0);
        let mut scored: Vec<(f32, MemoryRecord)> = g
            .iter()
            .filter(|r| tier.is_none_or(|t| r.tier == t))
            .map(|r| (cosine(&query_embedding, &r.embedding), r.clone()))
            .filter(|(s, _)| *s > 0.0 && *s >= floor)
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored.into_iter().take(limit).map(|(_, r)| r).collect())
    }

    async fn purge_older_than(
        &self,
        tier: Option<MemoryTier>,
        before_ms: u64,
    ) -> Result<u64, MemoryError> {
        let mut g = self
            .records
            .lock()
            .map_err(|e| MemoryError::Worker(e.to_string()))?;
        let len_before = g.len();
        g.retain(|r| !(r.created_at < before_ms && tier.is_none_or(|t| r.tier == t)));
        Ok((len_before - g.len()) as u64)
    }
}

/// SQLite backend. Connection is wrapped in a `Mutex` and operations run on
/// a `spawn_blocking` worker since rusqlite is sync.
pub struct SqliteStore {
    conn: std::sync::Arc<Mutex<rusqlite::Connection>>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS memories (
    id            TEXT PRIMARY KEY,
    tier          TEXT NOT NULL,
    owner_display TEXT NOT NULL,
    owner_pubkey  TEXT NOT NULL,
    text          TEXT NOT NULL,
    embedding     BLOB NOT NULL,
    metadata      TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    parent        TEXT
);
CREATE INDEX IF NOT EXISTS memories_tier_created_idx
    ON memories (tier, created_at DESC);
CREATE INDEX IF NOT EXISTS memories_created_idx
    ON memories (created_at DESC);
"#;

impl SqliteStore {
    pub fn open(path: &Path) -> Result<Self, MemoryError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = rusqlite::Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: std::sync::Arc::new(Mutex::new(conn)),
        })
    }

    pub fn open_in_memory() -> Result<Self, MemoryError> {
        let conn = rusqlite::Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: std::sync::Arc::new(Mutex::new(conn)),
        })
    }

    fn tier_str(t: MemoryTier) -> &'static str {
        match t {
            MemoryTier::Working => "working",
            MemoryTier::Episodic => "episodic",
            MemoryTier::LongTerm => "longterm",
        }
    }

    fn parse_tier(s: &str) -> MemoryTier {
        match s {
            "working" => MemoryTier::Working,
            "episodic" => MemoryTier::Episodic,
            _ => MemoryTier::LongTerm,
        }
    }

    fn embedding_to_bytes(v: &[f32]) -> Vec<u8> {
        let mut out = Vec::with_capacity(v.len() * 4);
        for f in v {
            out.extend_from_slice(&f.to_le_bytes());
        }
        out
    }

    fn embedding_from_bytes(b: &[u8]) -> Vec<f32> {
        b.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryRecord> {
        let id_s: String = row.get(0)?;
        let tier_s: String = row.get(1)?;
        let owner_display: String = row.get(2)?;
        let owner_pubkey_s: String = row.get(3)?;
        let text: String = row.get(4)?;
        let embedding_bytes: Vec<u8> = row.get(5)?;
        let metadata_s: String = row.get(6)?;
        let created_at: i64 = row.get(7)?;
        let parent_s: Option<String> = row.get(8)?;

        let id = Uuid::parse_str(&id_s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?;
        let pubkey_vec = bs58::decode(&owner_pubkey_s).into_vec().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
        })?;
        if pubkey_vec.len() != 32 {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                format!(
                    "owner_pubkey decoded to {} bytes, expected 32",
                    pubkey_vec.len()
                )
                .into(),
            ));
        }
        let mut pubkey = [0u8; 32];
        pubkey.copy_from_slice(&pubkey_vec);
        let metadata: serde_json::Value = serde_json::from_str(&metadata_s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(e))
        })?;
        let parent = parent_s
            .as_deref()
            .map(Uuid::parse_str)
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    8,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;

        Ok(MemoryRecord {
            id,
            tier: Self::parse_tier(&tier_s),
            owner: covenant_types::AgentId {
                display: owner_display,
                pubkey,
            },
            text,
            embedding: Self::embedding_from_bytes(&embedding_bytes),
            metadata,
            created_at: created_at as u64,
            parent,
        })
    }
}

#[async_trait]
impl MemoryStore for SqliteStore {
    async fn put(&self, record: MemoryRecord) -> Result<(), MemoryError> {
        let conn = self.conn.clone();
        task::spawn_blocking(move || -> Result<(), MemoryError> {
            let g = conn.lock().map_err(|e| MemoryError::Worker(e.to_string()))?;
            g.execute(
                "INSERT OR REPLACE INTO memories
                 (id, tier, owner_display, owner_pubkey, text, embedding, metadata, created_at, parent)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    record.id.to_string(),
                    SqliteStore::tier_str(record.tier),
                    record.owner.display,
                    bs58::encode(record.owner.pubkey).into_string(),
                    record.text,
                    SqliteStore::embedding_to_bytes(&record.embedding),
                    serde_json::to_string(&record.metadata)?,
                    record.created_at as i64,
                    record.parent.as_ref().map(|u| u.to_string()),
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| MemoryError::Worker(e.to_string()))??;
        Ok(())
    }

    async fn get(&self, id: Uuid) -> Result<Option<MemoryRecord>, MemoryError> {
        let conn = self.conn.clone();
        task::spawn_blocking(move || -> Result<Option<MemoryRecord>, MemoryError> {
            let g = conn.lock().map_err(|e| MemoryError::Worker(e.to_string()))?;
            let mut stmt = g.prepare(
                "SELECT id, tier, owner_display, owner_pubkey, text, embedding, metadata, created_at, parent
                 FROM memories WHERE id = ?1",
            )?;
            let mut rows = stmt.query(rusqlite::params![id.to_string()])?;
            if let Some(row) = rows.next()? {
                Ok(Some(SqliteStore::row_to_record(row)?))
            } else {
                Ok(None)
            }
        })
        .await
        .map_err(|e| MemoryError::Worker(e.to_string()))?
    }

    async fn all(&self) -> Result<Vec<MemoryRecord>, MemoryError> {
        let conn = self.conn.clone();
        task::spawn_blocking(move || -> Result<Vec<MemoryRecord>, MemoryError> {
            let g = conn.lock().map_err(|e| MemoryError::Worker(e.to_string()))?;
            let mut stmt = g.prepare(
                "SELECT id, tier, owner_display, owner_pubkey, text, embedding, metadata, created_at, parent
                 FROM memories ORDER BY created_at DESC",
            )?;
            let rows = stmt.query_map([], SqliteStore::row_to_record)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
        .await
        .map_err(|e| MemoryError::Worker(e.to_string()))?
    }

    async fn recent(
        &self,
        tier: Option<MemoryTier>,
        limit: usize,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        let conn = self.conn.clone();
        task::spawn_blocking(move || -> Result<Vec<MemoryRecord>, MemoryError> {
            let g = conn.lock().map_err(|e| MemoryError::Worker(e.to_string()))?;
            let (sql, params): (&str, Vec<rusqlite::types::Value>) = match tier {
                Some(t) => (
                    "SELECT id, tier, owner_display, owner_pubkey, text, embedding, metadata, created_at, parent
                     FROM memories WHERE tier = ?1 ORDER BY created_at DESC LIMIT ?2",
                    vec![SqliteStore::tier_str(t).to_string().into(), (limit as i64).into()],
                ),
                None => (
                    "SELECT id, tier, owner_display, owner_pubkey, text, embedding, metadata, created_at, parent
                     FROM memories ORDER BY created_at DESC LIMIT ?1",
                    vec![(limit as i64).into()],
                ),
            };
            let mut stmt = g.prepare(sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(params), SqliteStore::row_to_record)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
        .await
        .map_err(|e| MemoryError::Worker(e.to_string()))?
    }

    async fn delete(&self, id: Uuid) -> Result<bool, MemoryError> {
        let conn = self.conn.clone();
        task::spawn_blocking(move || -> Result<bool, MemoryError> {
            let g = conn
                .lock()
                .map_err(|e| MemoryError::Worker(e.to_string()))?;
            let n = g.execute(
                "DELETE FROM memories WHERE id = ?1",
                rusqlite::params![id.to_string()],
            )?;
            Ok(n > 0)
        })
        .await
        .map_err(|e| MemoryError::Worker(e.to_string()))?
    }

    async fn search_similar(
        &self,
        query_embedding: Vec<f32>,
        tier: Option<MemoryTier>,
        limit: usize,
        min_relevance: Option<f32>,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        let conn = self.conn.clone();
        let floor = min_relevance.unwrap_or(0.0).max(0.0);
        task::spawn_blocking(move || -> Result<Vec<MemoryRecord>, MemoryError> {
            let g = conn
                .lock()
                .map_err(|e| MemoryError::Worker(e.to_string()))?;
            let (sql, params): (&str, Vec<rusqlite::types::Value>) = match tier {
                Some(t) => (
                    "SELECT id, tier, owner_display, owner_pubkey, text, embedding, metadata, created_at, parent
                     FROM memories WHERE tier = ?1",
                    vec![SqliteStore::tier_str(t).to_string().into()],
                ),
                None => (
                    "SELECT id, tier, owner_display, owner_pubkey, text, embedding, metadata, created_at, parent
                     FROM memories",
                    vec![],
                ),
            };
            let mut stmt = g.prepare(sql)?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(params), SqliteStore::row_to_record)?;
            let mut scored: Vec<(f32, MemoryRecord)> = Vec::new();
            for r in rows {
                let r = r?;
                let s = cosine(&query_embedding, &r.embedding);
                if s > 0.0 && s >= floor {
                    scored.push((s, r));
                }
            }
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            Ok(scored.into_iter().take(limit).map(|(_, r)| r).collect())
        })
        .await
        .map_err(|e| MemoryError::Worker(e.to_string()))?
    }

    async fn purge_older_than(
        &self,
        tier: Option<MemoryTier>,
        before_ms: u64,
    ) -> Result<u64, MemoryError> {
        let conn = self.conn.clone();
        task::spawn_blocking(move || -> Result<u64, MemoryError> {
            let g = conn
                .lock()
                .map_err(|e| MemoryError::Worker(e.to_string()))?;
            let n = match tier {
                Some(t) => g.execute(
                    "DELETE FROM memories WHERE tier = ?1 AND created_at < ?2",
                    rusqlite::params![SqliteStore::tier_str(t), before_ms as i64],
                )?,
                None => g.execute(
                    "DELETE FROM memories WHERE created_at < ?1",
                    rusqlite::params![before_ms as i64],
                )?,
            };
            Ok(n as u64)
        })
        .await
        .map_err(|e| MemoryError::Worker(e.to_string()))?
    }

    /// Apply receipt-id correlations onto memory records inside a single
    /// SAVEPOINT-wrapped transaction. Overrides the trait default with a
    /// SQLite-native SAVEPOINT so a per-row failure rolls the entire
    /// batch back to zero rows changed. The planner that produces the
    /// correlation list lives next to this mutator in
    /// [`memory_receipt_backfill_correlations`].
    ///
    /// Per correlation the existing record's metadata is read, the
    /// `receipt_id` key is merged in (other keys are preserved; a
    /// non-object metadata is wrapped under `previous_metadata` to match
    /// the [`MemoryRepairCommand::BackfillProvenance`] convention), and
    /// the row is rewritten. The mutator opens an IMMEDIATE-mode
    /// transaction and a named [`MEMORY_BACKFILL_SAVEPOINT_NAME`]
    /// SAVEPOINT before any UPDATE; if any per-row step fails the
    /// SAVEPOINT is rolled back, the transaction is rolled back, and
    /// the original error surfaces to the caller. Zero rows change on
    /// failure.
    ///
    /// `dry_run` skips every write and returns the row_count the apply
    /// path would have produced. `row_count` excludes correlations that
    /// would not change the stored metadata (e.g., the same receipt_id
    /// is already present), which keeps repeated invocations idempotent.
    ///
    /// A correlation that references a missing memory_record_id returns
    /// [`MemoryError::RecordNotFound`] and aborts the batch so the
    /// caller does not silently report success against stale planner
    /// data.
    async fn backfill_receipt_correlation(
        &self,
        dry_run: bool,
        correlations: Vec<MemoryReceiptBackfillCorrelation>,
    ) -> Result<BackfillReceiptCorrelationOutcome, MemoryError> {
        let conn = self.conn.clone();
        task::spawn_blocking(
            move || -> Result<BackfillReceiptCorrelationOutcome, MemoryError> {
                let g = conn
                    .lock()
                    .map_err(|e| MemoryError::Worker(e.to_string()))?;

                if dry_run {
                    let mut row_count: u64 = 0;
                    for correlation in &correlations {
                        let current = read_record_metadata(&g, correlation.memory_record_id)?;
                        let next = merge_receipt_id(current.clone(), correlation.receipt_id);
                        if next != current {
                            row_count += 1;
                        }
                    }
                    return Ok(BackfillReceiptCorrelationOutcome {
                        row_count,
                        savepoint_name: MEMORY_BACKFILL_SAVEPOINT_NAME.into(),
                        dry_run: true,
                    });
                }

                g.execute_batch("BEGIN IMMEDIATE")?;
                g.execute_batch(&format!("SAVEPOINT {MEMORY_BACKFILL_SAVEPOINT_NAME}"))?;

                let result: Result<u64, MemoryError> = (|| {
                    let mut count: u64 = 0;
                    for correlation in &correlations {
                        let current = read_record_metadata(&g, correlation.memory_record_id)?;
                        let next = merge_receipt_id(current.clone(), correlation.receipt_id);
                        if next == current {
                            continue;
                        }
                        let metadata_str = serde_json::to_string(&next)?;
                        g.execute(
                            "UPDATE memories SET metadata = ?1 WHERE id = ?2",
                            rusqlite::params![
                                metadata_str,
                                correlation.memory_record_id.to_string()
                            ],
                        )?;
                        count += 1;
                    }
                    Ok(count)
                })();

                match result {
                    Ok(row_count) => {
                        g.execute_batch(&format!(
                            "RELEASE SAVEPOINT {MEMORY_BACKFILL_SAVEPOINT_NAME}"
                        ))?;
                        g.execute_batch("COMMIT")?;
                        Ok(BackfillReceiptCorrelationOutcome {
                            row_count,
                            savepoint_name: MEMORY_BACKFILL_SAVEPOINT_NAME.into(),
                            dry_run: false,
                        })
                    }
                    Err(e) => {
                        let _ = g.execute_batch(&format!(
                            "ROLLBACK TO SAVEPOINT {MEMORY_BACKFILL_SAVEPOINT_NAME}"
                        ));
                        let _ = g.execute_batch(&format!(
                            "RELEASE SAVEPOINT {MEMORY_BACKFILL_SAVEPOINT_NAME}"
                        ));
                        let _ = g.execute_batch("ROLLBACK");
                        Err(e)
                    }
                }
            },
        )
        .await
        .map_err(|e| MemoryError::Worker(e.to_string()))?
    }

    /// SQLite-native [`MemoryStore::compact`] override. The trait default
    /// runs N delete() + M put() calls as separate spawn_blocking ops with
    /// no transaction, so a mid-apply failure (full disk, SQLITE_CORRUPT, a
    /// trigger ABORT) leaves the store half-compacted: some records
    /// deleted, some parent refs detached, some stale-context writes
    /// missing — an audit-invisible inconsistency that contaminates
    /// downstream tier-lifecycle and drift reports. This override mirrors
    /// the backfill pattern: BEGIN IMMEDIATE + a named SAVEPOINT
    /// ([`MEMORY_COMPACT_SAVEPOINT_NAME`]) before any mutation, RELEASE +
    /// COMMIT on full success, ROLLBACK on any error so zero rows change
    /// on failure.
    ///
    /// The plan read uses a projection — id, tier, created_at, parent,
    /// metadata — instead of `all()`, which fetches every embedding BLOB
    /// `plan_compaction` never inspects. Saves a per-record allocation
    /// proportional to the embedding dimension on every compact pass.
    ///
    /// Apply is two surgical SQL statements per affected row: DELETE for
    /// the deleted set, UPDATE metadata + parent for the updates set.
    /// Neither path rewrites embedding bytes or text, which keeps the
    /// transaction small and preserves the costly columns verbatim.
    async fn compact(
        &self,
        request: MemoryCompactionRequest,
    ) -> Result<MemoryCompactionOutcome, MemoryError> {
        validate_compaction_request(&request)?;
        let conn = self.conn.clone();
        task::spawn_blocking(move || -> Result<MemoryCompactionOutcome, MemoryError> {
            let g = conn
                .lock()
                .map_err(|e| MemoryError::Worker(e.to_string()))?;

            // Projection read: only the columns plan_compaction reads.
            // Other MemoryRecord fields (owner, text, embedding) are
            // filled with cheap placeholders since plan_compaction
            // ignores them; the apply path never writes back through
            // those fields, so the placeholders never reach disk.
            let mut stmt = g.prepare(
                "SELECT id, tier, created_at, parent, metadata
                     FROM memories ORDER BY created_at DESC",
            )?;
            let rows = stmt.query_map([], |row| {
                let id_s: String = row.get(0)?;
                let tier_s: String = row.get(1)?;
                let created_at: i64 = row.get(2)?;
                let parent_s: Option<String> = row.get(3)?;
                let metadata_s: String = row.get(4)?;
                Ok((id_s, tier_s, created_at, parent_s, metadata_s))
            })?;
            let mut records = Vec::new();
            for r in rows {
                let (id_s, tier_s, created_at, parent_s, metadata_s) = r?;
                let id = Uuid::parse_str(&id_s).map_err(|e| MemoryError::Worker(e.to_string()))?;
                let tier = SqliteStore::parse_tier(&tier_s);
                let parent = parent_s
                    .map(|s| Uuid::parse_str(&s).map_err(|e| MemoryError::Worker(e.to_string())))
                    .transpose()?;
                let metadata = serde_json::from_str(&metadata_s)?;
                records.push(MemoryRecord {
                    id,
                    tier,
                    owner: covenant_types::AgentId::new("", [0u8; 32]),
                    text: String::new(),
                    embedding: Vec::new(),
                    metadata,
                    created_at: created_at as u64,
                    parent,
                });
            }
            drop(stmt);

            let (outcome, updates) = plan_compaction(&records, &request);

            if request.mode != MemoryRepairMode::Apply || !outcome.would_change {
                return Ok(outcome);
            }

            g.execute_batch("BEGIN IMMEDIATE")?;
            g.execute_batch(&format!("SAVEPOINT {MEMORY_COMPACT_SAVEPOINT_NAME}"))?;

            let apply_result: Result<(), MemoryError> = (|| {
                for id in &outcome.deleted {
                    g.execute(
                        "DELETE FROM memories WHERE id = ?1",
                        rusqlite::params![id.to_string()],
                    )?;
                }
                for update in &updates {
                    let metadata_str = serde_json::to_string(&update.metadata)?;
                    g.execute(
                        "UPDATE memories SET metadata = ?1, parent = ?2 WHERE id = ?3",
                        rusqlite::params![
                            metadata_str,
                            update.parent.as_ref().map(|u| u.to_string()),
                            update.id.to_string(),
                        ],
                    )?;
                }
                Ok(())
            })();

            match apply_result {
                Ok(()) => {
                    g.execute_batch(&format!(
                        "RELEASE SAVEPOINT {MEMORY_COMPACT_SAVEPOINT_NAME}"
                    ))?;
                    g.execute_batch("COMMIT")?;
                    Ok(outcome)
                }
                Err(e) => {
                    let _ = g.execute_batch(&format!(
                        "ROLLBACK TO SAVEPOINT {MEMORY_COMPACT_SAVEPOINT_NAME}"
                    ));
                    let _ = g.execute_batch(&format!(
                        "RELEASE SAVEPOINT {MEMORY_COMPACT_SAVEPOINT_NAME}"
                    ));
                    let _ = g.execute_batch("ROLLBACK");
                    Err(e)
                }
            }
        })
        .await
        .map_err(|e| MemoryError::Worker(e.to_string()))?
    }
}

fn read_record_metadata(
    conn: &rusqlite::Connection,
    id: Uuid,
) -> Result<serde_json::Value, MemoryError> {
    let mut stmt = conn.prepare("SELECT metadata FROM memories WHERE id = ?1")?;
    let mut rows = stmt.query(rusqlite::params![id.to_string()])?;
    let row = rows.next()?.ok_or(MemoryError::RecordNotFound(id))?;
    let metadata_s: String = row.get(0)?;
    Ok(serde_json::from_str(&metadata_s)?)
}

fn merge_receipt_id(metadata: serde_json::Value, receipt_id: Uuid) -> serde_json::Value {
    let mut map = match metadata {
        serde_json::Value::Object(m) => m,
        other => {
            let mut m = serde_json::Map::new();
            m.insert("previous_metadata".into(), other);
            m
        }
    };
    map.insert(
        "receipt_id".into(),
        serde_json::Value::String(receipt_id.to_string()),
    );
    serde_json::Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_types::{AgentId, MemoryTier};

    fn record(id: Uuid, tier: MemoryTier, text: &str, created_at: u64) -> MemoryRecord {
        MemoryRecord {
            id,
            tier,
            owner: AgentId::new("user@local", [0u8; 32]),
            text: text.into(),
            embedding: Vec::new(),
            metadata: serde_json::json!({}),
            created_at,
            parent: None,
        }
    }

    #[tokio::test]
    async fn in_memory_store_put_replaces_record_with_same_id() {
        // covenant_memory::InMemoryStore::put runs
        // g.retain(|r| r.id != record.id) before g.push(record), making
        // put behave as an upsert keyed by record.id — a second put
        // with the same id REPLACES the previous record rather than
        // appending a duplicate. Downstream callers (memory.update,
        // memory.repair, the compaction planner) rely on id being a
        // primary key.
        //
        // in_memory_put_get_recent_delete (next test) puts two
        // DIFFERENT ids and does not exercise the upsert path. A
        // refactor that dropped the retain line during a 'simplify by
        // removing the duplicate-check' pass would silently let two
        // records with the same id coexist; get() would return one
        // non-deterministically; recent() and all() would surface
        // both copies; the memory.repair and compaction planners
        // would see phantom duplicates.
        let s = InMemoryStore::new();
        let id = Uuid::new_v4();

        s.put(record(id, MemoryTier::Working, "first", 1))
            .await
            .unwrap();
        s.put(record(id, MemoryTier::Working, "second", 2))
            .await
            .unwrap();

        let got = s
            .get(id)
            .await
            .unwrap()
            .expect("upserted record must be retrievable by id");
        assert_eq!(
            got.text, "second",
            "InMemoryStore::put must REPLACE the previous record when called \
             with the same id — a refactor that dropped the retain line under \
             the rationale that 'callers handle deduplication explicitly' \
             would silently let the first record survive and get(id) would \
             return one of the two non-deterministically; pinning the \
             second-write-wins contract anchors the v0 upsert semantics",
        );
        assert_eq!(
            got.created_at, 2,
            "the replacement record's fields must overwrite the previous record \
             verbatim — a refactor that merged fields from the previous record \
             would silently surface partial state on every memory.update flow",
        );

        let all = s.all().await.unwrap();
        assert_eq!(
            all.len(),
            1,
            "InMemoryStore::all() must return exactly one record after two puts \
             with the same id — a refactor that dropped the retain line would \
             surface BOTH records here, breaking the primary-key contract that \
             every MemoryStore caller depends on",
        );
        assert_eq!(
            all[0].text, "second",
            "the single surviving record in all() must be the second (most \
             recent) write — pinning second-write-wins identically to get()",
        );

        let recent = s.recent(None, 10).await.unwrap();
        assert_eq!(
            recent.len(),
            1,
            "InMemoryStore::recent(None, 10) must return exactly one record \
             — the upsert contract must propagate to recent() identically to \
             all(); a refactor that diverged the two views would silently \
             surface duplicate rows on operator dashboards while keeping \
             get() consistent",
        );
        assert_eq!(recent[0].text, "second");
    }

    #[tokio::test]
    async fn in_memory_put_get_recent_delete() {
        let s = InMemoryStore::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        s.put(record(id1, MemoryTier::Working, "first", 1))
            .await
            .unwrap();
        s.put(record(id2, MemoryTier::Working, "second", 2))
            .await
            .unwrap();
        assert_eq!(s.get(id1).await.unwrap().unwrap().text, "first");
        let recent = s.recent(None, 10).await.unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].text, "second"); // most recent first
        assert!(s.delete(id1).await.unwrap());
        assert!(s.get(id1).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn in_memory_recent_filters_by_tier() {
        let s = InMemoryStore::new();
        s.put(record(Uuid::new_v4(), MemoryTier::Working, "w", 1))
            .await
            .unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::Episodic, "e", 2))
            .await
            .unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::LongTerm, "l", 3))
            .await
            .unwrap();
        let only_episodic = s.recent(Some(MemoryTier::Episodic), 10).await.unwrap();
        assert_eq!(only_episodic.len(), 1);
        assert_eq!(only_episodic[0].text, "e");
    }

    #[tokio::test]
    async fn sqlite_roundtrip() {
        let s = SqliteStore::open_in_memory().unwrap();
        let id = Uuid::new_v4();
        let mut r = record(id, MemoryTier::Episodic, "hello", 100);
        r.embedding = vec![0.1, -0.2, 0.3];
        r.metadata = serde_json::json!({"source": "test"});
        s.put(r.clone()).await.unwrap();
        let got = s.get(id).await.unwrap().unwrap();
        assert_eq!(got.text, "hello");
        assert_eq!(got.tier, MemoryTier::Episodic);
        assert_eq!(got.embedding, vec![0.1, -0.2, 0.3]);
        assert_eq!(got.metadata, serde_json::json!({"source": "test"}));
    }

    #[tokio::test]
    async fn sqlite_recent_orders_by_created_at_desc() {
        let s = SqliteStore::open_in_memory().unwrap();
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let id_c = Uuid::new_v4();
        s.put(record(id_a, MemoryTier::Working, "a", 100))
            .await
            .unwrap();
        s.put(record(id_b, MemoryTier::Working, "b", 300))
            .await
            .unwrap();
        s.put(record(id_c, MemoryTier::Working, "c", 200))
            .await
            .unwrap();
        let recent = s.recent(Some(MemoryTier::Working), 2).await.unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].text, "b");
        assert_eq!(recent[1].text, "c");
    }

    #[tokio::test]
    async fn sqlite_recent_filters_by_tier() {
        // SqliteStore is the production backend, and recent()'s `WHERE tier = ?1`
        // arm is what enforces memory.read.<tier> isolation there. The sibling
        // sqlite_recent_orders_by_created_at_desc seeds Working-only rows, so
        // dropping that clause would still pass it; mixed tiers make the filter
        // load-bearing — recent(Some(Episodic)) must surface only the Episodic row.
        let s = SqliteStore::open_in_memory().unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::Working, "w", 1))
            .await
            .unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::Episodic, "e", 2))
            .await
            .unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::LongTerm, "l", 3))
            .await
            .unwrap();
        let only_episodic = s.recent(Some(MemoryTier::Episodic), 10).await.unwrap();
        assert_eq!(only_episodic.len(), 1);
        assert_eq!(only_episodic[0].text, "e");
    }

    /// `recent` must apply the `memory.read.<tier>` filter *before* the limit:
    /// the returned page is the newest `limit` rows among the tier-matching
    /// rows, not the tier-matching subset of the newest `limit` rows.
    /// `in_memory_recent_filters_by_tier` keeps `limit` slack (10) so truncate
    /// is a no-op, and `sqlite_recent_orders_by_created_at_desc` binds the
    /// limit but seeds one tier, so neither pins the ordering. Here three
    /// Episodic rows sit behind two newer Working rows under a cap of two:
    /// filter-then-truncate returns the two newest Episodic rows; a
    /// `sort -> truncate -> filter` reorder takes the newer Working rows first
    /// and filters them away, collapsing to an empty page on the live
    /// `memory recent --tier Episodic --limit 2` path.
    #[tokio::test]
    async fn in_memory_recent_tier_filter_with_binding_limit_keeps_matching_rows_beyond_cap() {
        let s = InMemoryStore::new();
        s.put(record(Uuid::new_v4(), MemoryTier::Episodic, "e1", 1))
            .await
            .unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::Episodic, "e2", 2))
            .await
            .unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::Episodic, "e3", 3))
            .await
            .unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::Working, "w4", 4))
            .await
            .unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::Working, "w5", 5))
            .await
            .unwrap();

        let page = s.recent(Some(MemoryTier::Episodic), 2).await.unwrap();
        assert_eq!(page.len(), 2, "limit binds among the Episodic rows");
        assert!(
            page.iter().all(|r| r.tier == MemoryTier::Episodic),
            "newer Working rows must not displace tier-matching rows"
        );
        let texts: Vec<&str> = page.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["e3", "e2"],
            "newest two Episodic rows, newest first; a third (e1) exists beyond the cap"
        );
    }

    /// SQLite mirror: the production backend pages the tier+limit interaction
    /// through `WHERE tier = ?1 ORDER BY created_at DESC LIMIT ?2`, so the
    /// filter binds before the limit in SQL. Pin it so moving the tier
    /// predicate to a post-`LIMIT` Rust take regresses identically to the
    /// in-memory reorder.
    #[tokio::test]
    async fn sqlite_recent_tier_filter_with_binding_limit_keeps_matching_rows_beyond_cap() {
        let s = SqliteStore::open_in_memory().unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::Episodic, "e1", 1))
            .await
            .unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::Episodic, "e2", 2))
            .await
            .unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::Episodic, "e3", 3))
            .await
            .unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::Working, "w4", 4))
            .await
            .unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::Working, "w5", 5))
            .await
            .unwrap();

        let page = s.recent(Some(MemoryTier::Episodic), 2).await.unwrap();
        assert_eq!(page.len(), 2, "limit binds among the Episodic rows");
        assert!(
            page.iter().all(|r| r.tier == MemoryTier::Episodic),
            "newer Working rows must not displace tier-matching rows"
        );
        let texts: Vec<&str> = page.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["e3", "e2"],
            "newest two Episodic rows, newest first; a third (e1) exists beyond the cap"
        );
    }

    #[test]
    fn parse_tier_pins_canonical_tier_mapping_and_silent_catch_all_to_long_term() {
        // SqliteStore::parse_tier is the reverse of SqliteStore::tier_str
        // and is called from row_to_record on every memory record read.
        // The forward mapping (tier_str) is exercised indirectly through
        // every list_records/get_record round-trip test (sqlite_roundtrip,
        // sqlite_recent_orders_by_created_at_desc), but parse_tier has no
        // direct test pinning the three arms ('working' -> Working,
        // 'episodic' -> Episodic, _ catch-all -> LongTerm). Pin each arm
        // so a slug rename in tier_str-without-parse_tier (or vice versa)
        // or a tightening of the catch-all to a strict match is caught
        // at the parse_tier call site, distinct from the helper-internal
        // round-trip tests.
        assert_eq!(
            SqliteStore::parse_tier("working"),
            MemoryTier::Working,
            "working arm: SqliteStore::tier_str(Working) emits 'working' \
             so parse_tier must invert it; a slug rename in either \
             direction without a matching update would silently route \
             every Working record through the catch-all to LongTerm \
             at SQLite read time"
        );
        assert_eq!(
            SqliteStore::parse_tier("episodic"),
            MemoryTier::Episodic,
            "episodic arm: SqliteStore::tier_str(Episodic) emits \
             'episodic' so parse_tier must invert it; a slug rename in \
             either direction without a matching update would silently \
             route every Episodic record through the catch-all to \
             LongTerm at SQLite read time, breaking episodic-tier \
             scoped reads and dashboards"
        );
        assert_eq!(
            SqliteStore::parse_tier("longterm"),
            MemoryTier::LongTerm,
            "catch-all arm via canonical sibling: SqliteStore::tier_str \
             (LongTerm) emits 'longterm' which is intentionally NOT an \
             explicit match arm in parse_tier and routes through the \
             underscore catch-all to LongTerm; a refactor that promoted \
             'longterm' to an explicit arm would change the contract \
             that documents the catch-all as the LongTerm route — \
             future migrations relying on the catch-all to coerce \
             unknown labels would silently regress"
        );
        assert_eq!(
            SqliteStore::parse_tier("unknown-tier-from-future-migration"),
            MemoryTier::LongTerm,
            "catch-all arm via non-canonical value: any tier_s value not \
             matching 'working' or 'episodic' must coerce to LongTerm \
             so a manually-edited SQLite row, a mid-migration state, or \
             a daemon downgrade reading a future tier label gracefully \
             degrades to LongTerm; a refactor that tightened the \
             catch-all to a panic or error would turn a graceful read \
             path into a hard failure that takes down list_records and \
             get_record for the entire SqliteStore"
        );
    }

    #[test]
    fn memory_tier_slug_pins_canonical_forward_mapping_for_the_backfill_plan() {
        // The forward map feeding the `tier` field of every candidate in
        // memory_receipt_backfill_plan_json. The plan tests pin that schema's
        // keys but never the value, so the per-variant slug is only pinned here;
        // it must stay in lock-step with parse_tier's reverse map above.
        assert_eq!(memory_tier_slug(MemoryTier::Working), "working");
        assert_eq!(memory_tier_slug(MemoryTier::Episodic), "episodic");
        assert_eq!(memory_tier_slug(MemoryTier::LongTerm), "longterm");
    }

    #[test]
    fn validate_repair_request_pins_reason_and_backfill_provenance_arms() {
        let id = Uuid::new_v4();
        let detach = || MemoryRepairCommand::DetachParent {
            id,
            expected_parent: None,
        };
        let delete = || MemoryRepairCommand::DeleteRecord { id };
        let backfill = |provenance: serde_json::Value| MemoryRepairCommand::BackfillProvenance {
            id,
            provenance,
        };

        // DetachParent with non-empty reason validates Ok.
        assert!(validate_repair_request(&MemoryRepairRequest {
            mode: MemoryRepairMode::DryRun,
            command: detach(),
            reason: "operator-initiated".into(),
        })
        .is_ok());

        // Empty/whitespace reason rejects for every command variant so
        // the reason field cannot silently widen across the three arms.
        for reason in ["", "   "] {
            for command in [
                detach(),
                delete(),
                backfill(serde_json::json!({"source": "manual"})),
            ] {
                let err = validate_repair_request(&MemoryRepairRequest {
                    mode: MemoryRepairMode::DryRun,
                    command,
                    reason: reason.into(),
                })
                .unwrap_err();
                match err {
                    MemoryError::InvalidRepair(message) => assert!(
                        message.contains("reason must not be empty"),
                        "unexpected InvalidRepair payload: {message:?}",
                    ),
                    other => panic!("expected InvalidRepair, got {other:?}"),
                }
            }
        }

        // BackfillProvenance with null provenance rejects even when
        // reason is non-empty. The provenance check only fires for
        // BackfillProvenance; DetachParent and DeleteRecord do not
        // carry a provenance field.
        let err = validate_repair_request(&MemoryRepairRequest {
            mode: MemoryRepairMode::DryRun,
            command: backfill(serde_json::Value::Null),
            reason: "operator-initiated".into(),
        })
        .unwrap_err();
        match err {
            MemoryError::InvalidRepair(message) => assert!(
                message.contains("provenance must not be null"),
                "unexpected InvalidRepair payload: {message:?}",
            ),
            other => panic!("expected InvalidRepair, got {other:?}"),
        }

        // BackfillProvenance with a non-null provenance object validates
        // Ok so legitimate backfills still flow through.
        assert!(validate_repair_request(&MemoryRepairRequest {
            mode: MemoryRepairMode::DryRun,
            command: backfill(serde_json::json!({"source": "manual"})),
            reason: "operator-initiated".into(),
        })
        .is_ok());

        // DetachParent and DeleteRecord with non-empty reason validate
        // Ok; the provenance branch must not widen onto these arms.
        for command in [detach(), delete()] {
            assert!(validate_repair_request(&MemoryRepairRequest {
                mode: MemoryRepairMode::DryRun,
                command,
                reason: "operator-initiated".into(),
            })
            .is_ok());
        }
    }

    #[test]
    fn validate_compaction_request_pins_reason_and_empty_policy_arms() {
        let request = |policy: MemoryCompactionPolicy, reason: &str| MemoryCompactionRequest {
            mode: MemoryRepairMode::DryRun,
            policy,
            reason: reason.into(),
        };

        let non_empty_policy = MemoryCompactionPolicy {
            delete_working_before_ms: Some(123),
            ..MemoryCompactionPolicy::default()
        };

        // Non-empty reason and non-empty policy validate Ok.
        assert!(validate_compaction_request(&request(
            non_empty_policy.clone(),
            "operator-initiated"
        ))
        .is_ok());

        // Empty/whitespace reason rejects regardless of policy so a
        // policy-only check cannot mask a blank reason.
        for reason in ["", "   "] {
            let err = validate_compaction_request(&request(non_empty_policy.clone(), reason))
                .unwrap_err();
            match err {
                MemoryError::InvalidCompaction(message) => assert!(
                    message.contains("reason must not be empty"),
                    "unexpected InvalidCompaction payload: {message:?}",
                ),
                other => panic!("expected InvalidCompaction, got {other:?}"),
            }
        }

        // All-default policy rejects with the documented message even
        // when reason is non-empty; this is the no-op compaction guard.
        let err =
            validate_compaction_request(&request(MemoryCompactionPolicy::default(), "operator"))
                .unwrap_err();
        match err {
            MemoryError::InvalidCompaction(message) => assert!(
                message.contains("policy must enable at least one compaction action"),
                "unexpected InvalidCompaction payload: {message:?}",
            ),
            other => panic!("expected InvalidCompaction, got {other:?}"),
        }

        // detach_stale_parents=true alone validates Ok; the boolean
        // must NOT be treated as inert by is_empty(), or the legitimate
        // detach-only compaction shape would be silently rejected.
        assert!(validate_compaction_request(&request(
            MemoryCompactionPolicy {
                detach_stale_parents: true,
                ..MemoryCompactionPolicy::default()
            },
            "operator-initiated",
        ))
        .is_ok());

        // Each individual before_ms cutoff alone validates Ok so the
        // is_empty() short-circuit cannot regress to require multiple
        // policy fields to be set.
        for policy in [
            MemoryCompactionPolicy {
                delete_working_before_ms: Some(1),
                ..MemoryCompactionPolicy::default()
            },
            MemoryCompactionPolicy {
                delete_episodic_before_ms: Some(1),
                ..MemoryCompactionPolicy::default()
            },
            MemoryCompactionPolicy {
                mark_longterm_stale_before_ms: Some(1),
                ..MemoryCompactionPolicy::default()
            },
        ] {
            assert!(validate_compaction_request(&request(policy, "operator")).is_ok());
        }
    }

    #[test]
    fn cosine_basics() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        let c = vec![0.0, 1.0, 0.0];
        assert!((cosine(&a, &b) - 1.0).abs() < 1e-6);
        assert!(cosine(&a, &c).abs() < 1e-6);
        assert_eq!(cosine(&[], &[]), 0.0);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
    }

    #[test]
    fn cosine_zero_norm_returns_zero_and_anti_parallel_returns_minus_one() {
        // The docstring promises 0.0 for any degenerate input (mismatched
        // length, zero norm, empty). cosine_basics covers length-mismatch
        // and empty; this pin closes the zero-norm arm in both
        // orientations and asserts the result is a finite 0.0, not NaN —
        // a regression that dropped the `na == 0.0 || nb == 0.0` guard
        // would silently NaN every search rank that compares against an
        // all-zero embedding.
        let both_zero = cosine(&[0.0, 0.0, 0.0], &[0.0, 0.0, 0.0]);
        assert_eq!(both_zero, 0.0);
        assert!(!both_zero.is_nan());

        let lhs_zero = cosine(&[0.0, 0.0, 0.0], &[1.0, 0.0, 0.0]);
        assert_eq!(lhs_zero, 0.0);
        assert!(!lhs_zero.is_nan());

        let rhs_zero = cosine(&[1.0, 0.0, 0.0], &[0.0, 0.0, 0.0]);
        assert_eq!(rhs_zero, 0.0);
        assert!(!rhs_zero.is_nan());

        // Anti-parallel unit vectors must score -1.0, not +1.0. A vectorised
        // rewrite that mishandles negative components (e.g. taking |x*y| in
        // the dot product) would silently invert the similarity ordering of
        // every embedding with a sign-bearing dimension.
        let anti_parallel = cosine(&[1.0, 0.0], &[-1.0, 0.0]);
        assert!(
            (anti_parallel - (-1.0)).abs() < 1e-6,
            "anti-parallel must be ~-1.0, got {anti_parallel}",
        );
    }

    #[tokio::test]
    async fn in_memory_search_returns_closest_first() {
        let s = InMemoryStore::new();
        let mut a = record(Uuid::new_v4(), MemoryTier::Working, "alpha", 1);
        a.embedding = vec![1.0, 0.0, 0.0];
        let mut b = record(Uuid::new_v4(), MemoryTier::Working, "beta", 2);
        b.embedding = vec![0.0, 1.0, 0.0];
        let mut c = record(Uuid::new_v4(), MemoryTier::Working, "gamma", 3);
        c.embedding = vec![0.9, 0.1, 0.0];
        s.put(a).await.unwrap();
        s.put(b).await.unwrap();
        s.put(c).await.unwrap();
        let hits = s
            .search_similar(vec![1.0, 0.0, 0.0], None, 2, None)
            .await
            .unwrap();
        assert_eq!(hits[0].text, "alpha");
        assert_eq!(hits[1].text, "gamma");
    }

    #[tokio::test]
    async fn in_memory_search_min_relevance_drops_below_threshold() {
        let s = InMemoryStore::new();
        let mut close = record(Uuid::new_v4(), MemoryTier::Working, "close", 1);
        close.embedding = vec![1.0, 0.0, 0.0];
        let mut far = record(Uuid::new_v4(), MemoryTier::Working, "far", 2);
        far.embedding = vec![0.1, 1.0, 0.0];
        s.put(close).await.unwrap();
        s.put(far).await.unwrap();
        let hits = s
            .search_similar(vec![1.0, 0.0, 0.0], None, 10, Some(0.5))
            .await
            .unwrap();
        assert_eq!(hits.len(), 1, "min_relevance 0.5 must drop far record");
        assert_eq!(hits[0].text, "close");
    }

    #[tokio::test]
    async fn sqlite_search_min_relevance_drops_below_threshold() {
        let s = SqliteStore::open_in_memory().unwrap();
        let mut close = record(Uuid::new_v4(), MemoryTier::Working, "close", 1);
        close.embedding = vec![1.0, 0.0, 0.0];
        let mut far = record(Uuid::new_v4(), MemoryTier::Working, "far", 2);
        far.embedding = vec![0.1, 1.0, 0.0];
        s.put(close).await.unwrap();
        s.put(far).await.unwrap();
        let hits = s
            .search_similar(vec![1.0, 0.0, 0.0], None, 10, Some(0.5))
            .await
            .unwrap();
        assert_eq!(hits.len(), 1, "min_relevance 0.5 must drop far record");
        assert_eq!(hits[0].text, "close");
    }

    #[tokio::test]
    async fn in_memory_search_similar_pins_strict_positive_filter_and_inclusive_floor_equal_boundary(
    ) {
        // InMemoryStore::search_similar applies the paired
        // predicate
        //
        //     .filter(|(s, _)| *s > 0.0 && *s >= floor)
        //
        // The strict-positive `s > 0.0` arm drops degenerate
        // zero-cosine records (zero-norm or mismatched-length embeddings;
        // see cosine()) and anchors the documented
        // 'Records with empty embeddings get score 0 and are... dropped,
        // depending on the impl' contract from the MemoryStore trait
        // doc. The `s >= floor` arm pins the inclusive
        // min_relevance semantic documented on the trait method ('records
        // whose cosine score is strictly less than the threshold are
        // dropped' — i.e., equality passes).
        //
        // The existing in_memory_search_min_relevance_drops_below_threshold
        // (above) probes min_relevance=Some(0.5) against cosine ~0.099
        // — both `>` and `>=` agree on this case, neither arm of the
        // dual predicate is uniquely exercised. The existing
        // in_memory_search_returns_closest_first uses
        // limit=2 against 3 records so a zero-cosine record is silently
        // dropped by limit-truncation rather than by the strict-positive
        // filter. A refactor that changed `s > 0.0` to `s >= 0.0` under
        // a 'simplify the dual predicate' rationale, or that changed
        // `s >= floor` to `s > floor` under a 'use exclusive thresholds
        // for consistency with the doc phrase strictly less than'
        // rationale, would not surface in either existing test.

        let s = InMemoryStore::new();
        let mut exact = record(Uuid::new_v4(), MemoryTier::Working, "exact", 1);
        exact.embedding = vec![1.0, 0.0, 0.0];
        let mut orthogonal = record(Uuid::new_v4(), MemoryTier::Working, "orthogonal", 2);
        orthogonal.embedding = vec![0.0, 1.0, 0.0];
        let mut anti = record(Uuid::new_v4(), MemoryTier::Working, "anti", 3);
        anti.embedding = vec![-1.0, 0.0, 0.0];
        s.put(exact).await.unwrap();
        s.put(orthogonal).await.unwrap();
        s.put(anti).await.unwrap();

        // (1) Strict-positive arm: limit much larger than the non-zero
        // match count, no min_relevance set. The orthogonal record's
        // cosine is exactly 0.0 — under `s > 0.0` it must be filtered;
        // under a regressed `s >= 0.0` it would surface. The
        // anti-parallel record's cosine is -1.0 — filtered by either
        // operator on the strict-positive arm. Only 'exact' surfaces.
        let hits = s
            .search_similar(vec![1.0, 0.0, 0.0], None, 10, None)
            .await
            .unwrap();
        assert_eq!(
            hits.len(),
            1,
            "strict-positive filter `s > 0.0` must drop the orthogonal \
             cosine-zero record even with abundant limit and no \
             min_relevance set — a refactor to `s >= 0.0` would let \
             zero-norm or mismatched-length embeddings (which v0's mock \
             embedder and Ollama embedder cannot produce, but a future \
             external embedder or corrupted SQLite blob could) pollute \
             every search result silently",
        );
        assert_eq!(
            hits[0].text, "exact",
            "the only surviving record must be the cosine=1.0 match",
        );

        // (2) Inclusive-floor-equality arm: min_relevance = Some(1.0).
        // The 'exact' record's cosine is exactly 1.0 (f32 arithmetic
        // 1.0/(1.0*1.0) = 1.0) — under `s >= floor` it passes; under a
        // regressed `s > floor` it would be rejected. Orthogonal is
        // filtered by strict-positive; anti by strict-positive.
        let hits = s
            .search_similar(vec![1.0, 0.0, 0.0], None, 10, Some(1.0))
            .await
            .unwrap();
        assert_eq!(
            hits.len(),
            1,
            "inclusive-floor `s >= floor` must accept a record whose \
             cosine is exactly the min_relevance threshold — a refactor \
             to `s > floor` would silently shift the documented \
             inclusive semantic so operators tuning min_relevance to \
             the cosine of their best-match record (a common \
             calibration step after embedding upgrades) would see zero \
             results returned with no signal that the comparison \
             operator drifted",
        );
        assert_eq!(
            hits[0].text, "exact",
            "the exact-cosine record must surface at the equality boundary",
        );
    }

    #[tokio::test]
    async fn sqlite_search_similar_pins_strict_positive_filter_and_inclusive_floor_equal_boundary()
    {
        // SqliteStore::search_similar applies the same paired
        // predicate as InMemoryStore::search_similar:
        //
        //     if s > 0.0 && s >= floor {
        //
        // The two implementations must agree on the dual predicate so
        // storage-tier choice (in-memory for tests/defaults vs SQLite
        // for persistent daemon runs) doesn't change the search result
        // set semantic. The parallel pin (in_memory_search_similar_pins_*
        // above) anchors the contract on the InMemoryStore side; this
        // pin anchors it on the SQLite side. A refactor that diverged
        // the two implementations — e.g., dropped the strict-positive
        // guard on SQLite under a 'rows controlled by put() can never
        // have empty embeddings' rationale — would silently shift
        // result sets when operators migrate from in-memory to SQLite
        // with no audit signal.

        let s = SqliteStore::open_in_memory().unwrap();
        let mut exact = record(Uuid::new_v4(), MemoryTier::Working, "exact", 1);
        exact.embedding = vec![1.0, 0.0, 0.0];
        let mut orthogonal = record(Uuid::new_v4(), MemoryTier::Working, "orthogonal", 2);
        orthogonal.embedding = vec![0.0, 1.0, 0.0];
        let mut anti = record(Uuid::new_v4(), MemoryTier::Working, "anti", 3);
        anti.embedding = vec![-1.0, 0.0, 0.0];
        s.put(exact).await.unwrap();
        s.put(orthogonal).await.unwrap();
        s.put(anti).await.unwrap();

        // (1) Strict-positive arm — same shape as the in-memory pin.
        let hits = s
            .search_similar(vec![1.0, 0.0, 0.0], None, 10, None)
            .await
            .unwrap();
        assert_eq!(
            hits.len(),
            1,
            "SqliteStore strict-positive filter must drop the \
             orthogonal cosine-zero record — cross-binds the \
             in_memory_search_similar_pins_strict_positive_filter_and_inclusive_floor_equal_boundary \
             arm so the two implementations cannot diverge silently",
        );
        assert_eq!(hits[0].text, "exact");

        // (2) Inclusive-floor-equality arm — same shape as the in-memory
        // pin.
        let hits = s
            .search_similar(vec![1.0, 0.0, 0.0], None, 10, Some(1.0))
            .await
            .unwrap();
        assert_eq!(
            hits.len(),
            1,
            "SqliteStore inclusive-floor must accept a record at \
             cosine exactly equal to the min_relevance threshold — \
             cross-binds the in_memory pin's inclusive-equality arm",
        );
        assert_eq!(hits[0].text, "exact");
    }

    #[test]
    fn sqlite_embedding_bytes_pins_round_trip_little_endian_and_trailing_partial_chunk_drop() {
        // SqliteStore::embedding_to_bytes and
        // ::embedding_from_bytes are the SQLite BLOB
        // serializer/deserializer pair the persistent memory store
        // uses to round-trip f32 embedding vectors through the
        // embedding column. The pair encodes f32 as little-endian
        // 4-byte chunks with no length prefix, no version byte, no
        // padding — embedding_to_bytes writes f32::to_le_bytes()
        // back-to-back; embedding_from_bytes reads via
        // chunks_exact(4) which silently drops any trailing partial
        // chunk.
        //
        // sqlite_roundtrip and sqlite_recent_orders_by_created_at_desc
        // round-trip records but never directly assert byte-level
        // encoding shape, byte order, or trailing-chunk drop
        // semantics. A refactor that swapped little-endian for big-
        // endian would silently corrupt every persisted embedding
        // across mixed-endian deployments. A refactor that switched
        // chunks_exact to chunks would silently panic on the first
        // truncated row. A refactor that added a length prefix would
        // silently scramble cosine similarity for every existing row.

        // (1) Empty round-trip: no allocation, no panic.
        assert_eq!(
            SqliteStore::embedding_to_bytes(&[]),
            Vec::<u8>::new(),
            "empty embedding must serialize to empty bytes",
        );
        assert_eq!(
            SqliteStore::embedding_from_bytes(&[]),
            Vec::<f32>::new(),
            "empty bytes must deserialize to empty embedding",
        );

        // (2) Single-element round-trip.
        let v = vec![1.5_f32];
        assert_eq!(
            SqliteStore::embedding_from_bytes(&SqliteStore::embedding_to_bytes(&v)),
            v,
            "single-element vector must round-trip exactly",
        );

        // (3) Byte order: cross-bind to f32::to_le_bytes so any
        // switch to big-endian (e.g., 'match network byte order')
        // surfaces immediately.
        assert_eq!(
            SqliteStore::embedding_to_bytes(&[1.0_f32]),
            1.0_f32.to_le_bytes().to_vec(),
            "embedding_to_bytes must emit little-endian f32 — a \
             refactor that swapped to to_be_bytes under a 'match \
             network byte order' rationale would silently corrupt \
             every persisted embedding across mixed-endian \
             deployments (and an in-process round-trip would still \
             pass because both writer and reader would agree on the \
             new order)",
        );

        // (4) Ordering preservation: first f32 → first 4 bytes.
        let bytes = SqliteStore::embedding_to_bytes(&[1.0_f32, 2.0, 3.0]);
        assert_eq!(bytes.len(), 12, "three f32 must produce 12 bytes");
        assert_eq!(
            f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            1.0_f32,
            "first f32 must occupy bytes [0..4]",
        );
        assert_eq!(
            f32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            2.0_f32,
            "second f32 must occupy bytes [4..8]",
        );
        assert_eq!(
            f32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            3.0_f32,
            "third f32 must occupy bytes [8..12]",
        );

        // (5) Trailing partial chunk drop: chunks_exact(4) silently
        // drops the trailing 1-3 bytes. A refactor that switched to
        // chunks(4) and indexed c[0..4] would panic on the partial
        // slice; the silent-drop contract is what protects the
        // recall path against partial writes.
        assert_eq!(
            SqliteStore::embedding_from_bytes(&[0u8, 0, 0, 0, 1, 2, 3]),
            vec![0.0_f32],
            "embedding_from_bytes must yield exactly 1 f32 from 7 \
             bytes — chunks_exact(4) drops the trailing 3 bytes \
             silently, which is what protects the recall path from \
             panicking on partially-written rows after a write-time \
             crash",
        );

        // (6) Length scaling: 100 f32 → 400 bytes (no prefix, no
        // padding, no overhead). A length prefix or version byte
        // would shift this to 404+.
        assert_eq!(
            SqliteStore::embedding_to_bytes(&[0.0_f32; 100]).len(),
            400,
            "100 f32 must produce exactly 400 bytes (4 bytes per f32, \
             no overhead) — a refactor that added a length prefix \
             ('4-byte u32 length || 4N bytes') under an 'evolve the \
             embedding format' rationale without a coordinated \
             migration would shift this to 404 and silently scramble \
             cosine similarity for every existing SQLite row",
        );
    }

    #[tokio::test]
    async fn in_memory_search_respects_tier_filter() {
        // InMemoryStore is the default backend when persistence is off, so
        // its inline `tier.is_none_or(|t| r.tier == t)` predicate is what
        // enforces memory.read.<tier> isolation in that configuration. Two
        // equal-similarity records in different tiers pin it: a search scoped
        // to Episodic must drop the higher-scoring-eligible Working record.
        let s = InMemoryStore::new();
        let mut w = record(Uuid::new_v4(), MemoryTier::Working, "w-alpha", 1);
        w.embedding = vec![1.0, 0.0, 0.0];
        let mut e = record(Uuid::new_v4(), MemoryTier::Episodic, "e-alpha", 2);
        e.embedding = vec![1.0, 0.0, 0.0];
        s.put(w).await.unwrap();
        s.put(e).await.unwrap();
        let hits = s
            .search_similar(vec![1.0, 0.0, 0.0], Some(MemoryTier::Episodic), 5, None)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].text, "e-alpha");
    }

    #[tokio::test]
    async fn sqlite_search_respects_tier_filter() {
        let s = SqliteStore::open_in_memory().unwrap();
        let mut w = record(Uuid::new_v4(), MemoryTier::Working, "w-alpha", 1);
        w.embedding = vec![1.0, 0.0, 0.0];
        let mut e = record(Uuid::new_v4(), MemoryTier::Episodic, "e-alpha", 2);
        e.embedding = vec![1.0, 0.0, 0.0];
        s.put(w).await.unwrap();
        s.put(e).await.unwrap();
        let hits = s
            .search_similar(vec![1.0, 0.0, 0.0], Some(MemoryTier::Episodic), 5, None)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].text, "e-alpha");
    }

    /// `search_similar` must apply the `memory.read.<tier>` filter *before* the
    /// limit: the page is the top-`limit` rows among the tier-matching rows,
    /// not the tier-matching subset of the top-`limit` rows overall. The
    /// `*_search_respects_tier_filter` tests seed equal-similarity rows under a
    /// slack limit, so neither a binding limit nor a higher-similarity off-tier
    /// row is exercised. Here a Working row outscores two Episodic matches under
    /// a cap of one: filter-then-take returns the top Episodic row; a
    /// `take -> filter` reorder takes the Working row first and filters it away,
    /// collapsing to an empty page on the live `memory search --tier --limit`
    /// path.
    #[tokio::test]
    async fn in_memory_search_tier_filter_with_binding_limit_keeps_matching_rows_beyond_cap() {
        let s = InMemoryStore::new();
        let mut w = record(Uuid::new_v4(), MemoryTier::Working, "w1", 1);
        w.embedding = vec![1.0, 0.0, 0.0];
        let mut e1 = record(Uuid::new_v4(), MemoryTier::Episodic, "e1", 2);
        e1.embedding = vec![0.8, 0.6, 0.0];
        let mut e2 = record(Uuid::new_v4(), MemoryTier::Episodic, "e2", 3);
        e2.embedding = vec![0.6, 0.8, 0.0];
        s.put(w).await.unwrap();
        s.put(e1).await.unwrap();
        s.put(e2).await.unwrap();

        let hits = s
            .search_similar(vec![1.0, 0.0, 0.0], Some(MemoryTier::Episodic), 1, None)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1, "limit binds among the Episodic rows");
        assert_eq!(
            hits[0].tier,
            MemoryTier::Episodic,
            "the higher-similarity Working row must not consume the slot"
        );
        assert_eq!(
            hits[0].text, "e1",
            "the top-similarity Episodic row, with a second Episodic match beyond the cap"
        );
    }

    /// SQLite mirror: the production backend filters with `WHERE tier = ?1`
    /// before scoring and taking the limit in Rust, so the tier predicate binds
    /// before the cap. Pin it so moving the tier filter to a post-`take` Rust
    /// step regresses identically to the in-memory reorder.
    #[tokio::test]
    async fn sqlite_search_tier_filter_with_binding_limit_keeps_matching_rows_beyond_cap() {
        let s = SqliteStore::open_in_memory().unwrap();
        let mut w = record(Uuid::new_v4(), MemoryTier::Working, "w1", 1);
        w.embedding = vec![1.0, 0.0, 0.0];
        let mut e1 = record(Uuid::new_v4(), MemoryTier::Episodic, "e1", 2);
        e1.embedding = vec![0.8, 0.6, 0.0];
        let mut e2 = record(Uuid::new_v4(), MemoryTier::Episodic, "e2", 3);
        e2.embedding = vec![0.6, 0.8, 0.0];
        s.put(w).await.unwrap();
        s.put(e1).await.unwrap();
        s.put(e2).await.unwrap();

        let hits = s
            .search_similar(vec![1.0, 0.0, 0.0], Some(MemoryTier::Episodic), 1, None)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1, "limit binds among the Episodic rows");
        assert_eq!(
            hits[0].tier,
            MemoryTier::Episodic,
            "the higher-similarity Working row must not consume the slot"
        );
        assert_eq!(
            hits[0].text, "e1",
            "the top-similarity Episodic row, with a second Episodic match beyond the cap"
        );
    }

    #[tokio::test]
    async fn in_memory_purge_older_than_drops_old_records() {
        let s = InMemoryStore::new();
        s.put(record(Uuid::new_v4(), MemoryTier::Working, "old", 100))
            .await
            .unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::Working, "newer", 500))
            .await
            .unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::Episodic, "old-ep", 100))
            .await
            .unwrap();
        // Purge only the working tier records older than 200.
        let n = s
            .purge_older_than(Some(MemoryTier::Working), 200)
            .await
            .unwrap();
        assert_eq!(n, 1);
        let remaining = s.recent(None, 10).await.unwrap();
        assert_eq!(remaining.len(), 2);
    }

    #[tokio::test]
    async fn sqlite_purge_older_than_clears_only_matching_rows() {
        let s = SqliteStore::open_in_memory().unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::Working, "old", 100))
            .await
            .unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::Working, "newer", 500))
            .await
            .unwrap();
        // A same-age record in another tier proves the delete is tier-scoped,
        // not just age-scoped. purge_older_than is destructive, so a dropped
        // `tier = ?1` clause is cross-tier data loss, not a read leak: this
        // Episodic row is older than the cutoff and must survive a
        // Working-scoped purge.
        s.put(record(Uuid::new_v4(), MemoryTier::Episodic, "old-ep", 100))
            .await
            .unwrap();
        let n = s
            .purge_older_than(Some(MemoryTier::Working), 200)
            .await
            .unwrap();
        assert_eq!(n, 1);
        let recent = s.recent(Some(MemoryTier::Working), 10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].text, "newer");
        let episodic = s.recent(Some(MemoryTier::Episodic), 10).await.unwrap();
        assert_eq!(
            episodic.len(),
            1,
            "a Working-scoped purge must spare the older Episodic record"
        );
        assert_eq!(episodic[0].text, "old-ep");
    }

    #[tokio::test]
    async fn in_memory_purge_older_than_keeps_record_stamped_exactly_at_cutoff() {
        // InMemoryStore::purge_older_than retains a record unless it is
        // STRICTLY older than the cutoff: `!(r.created_at < before_ms && ..)`.
        // The trait contract (the `purge_older_than` doc) says "strictly older
        // than before_ms", so a record stamped EXACTLY at before_ms survives.
        // in_memory_purge_older_than_drops_old_records stamps 100/500 with
        // cutoff 200, so no record ever lands on the cutoff and the equality
        // arm is exercised by zero tests. A `<` -> `<=` flip (a "purge older OR
        // EQUAL TO before_ms" misreading) would silently purge every record
        // stamped on the cutoff tick — an operator running a TTL purge with a
        // cutoff aligned to a record timestamp loses that record every cycle,
        // and the strict-less-than test still passes. Pin both the equality
        // keep arm and the strict-less-than drop arm so a coordinated swap
        // cannot land silently.
        let s = InMemoryStore::new();
        s.put(record(Uuid::new_v4(), MemoryTier::Working, "below", 100))
            .await
            .unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::Working, "boundary", 200))
            .await
            .unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::Working, "above", 300))
            .await
            .unwrap();

        // cutoff=200 lands the boundary on the middle record: 100 is strictly
        // less (purged), 200 sits on the cutoff (kept), 300 is greater (kept).
        let purged = s
            .purge_older_than(Some(MemoryTier::Working), 200)
            .await
            .unwrap();
        assert_eq!(
            purged, 1,
            "cutoff=200 must purge only the 100-stamped record; the \
             200-stamped record sits on the cutoff and `<` keeps it. A flip \
             to `<=` would purge both and return 2, silently losing every \
             cutoff-equal record. got: {purged}",
        );
        let mut stamps: Vec<u64> = s
            .recent(Some(MemoryTier::Working), 10)
            .await
            .unwrap()
            .iter()
            .map(|r| r.created_at)
            .collect();
        stamps.sort();
        assert_eq!(
            stamps,
            vec![200, 300],
            "survivors must be the cutoff-equal record (200) and the \
             strictly-greater one (300); purging cutoff-equal records would \
             leave [300] and inverting the predicate would leave [100]",
        );

        // Re-purge with the boundary moved to 300 so the equality arm is
        // pinned on a second cutoff value, not a phase-1 coincidence.
        let purged = s
            .purge_older_than(Some(MemoryTier::Working), 300)
            .await
            .unwrap();
        assert_eq!(
            purged, 1,
            "re-purge at cutoff=300 drops the now-strictly-less 200 while \
             keeping the cutoff-equal 300; pins the equality arm is invariant \
             to the cutoff value. got: {purged}",
        );
        let remaining = s.recent(Some(MemoryTier::Working), 10).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(
            remaining[0].created_at, 300,
            "the lone survivor must be the cutoff-equal 300-stamped record",
        );
    }

    #[tokio::test]
    async fn sqlite_purge_older_than_keeps_record_stamped_exactly_at_cutoff() {
        // SqliteStore::purge_older_than deletes via the same strict cutoff as
        // the in-memory backend: `DELETE FROM memories WHERE .. created_at < ?2`,
        // so a row stamped EXACTLY at before_ms survives. sqlite_purge_older_
        // than_clears_only_matching_rows stamps 100/500 with cutoff 200 and
        // pins tier scoping, never the cutoff-equality arm. A `<` -> `<=` flip
        // in the SQL predicate would DELETE the boundary row on every purge and
        // diverge from the in-memory backend and the trait contract. Mirror the
        // in-memory equality pin so the two backends stay aligned.
        let s = SqliteStore::open_in_memory().unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::Working, "below", 100))
            .await
            .unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::Working, "boundary", 200))
            .await
            .unwrap();
        s.put(record(Uuid::new_v4(), MemoryTier::Working, "above", 300))
            .await
            .unwrap();

        let purged = s
            .purge_older_than(Some(MemoryTier::Working), 200)
            .await
            .unwrap();
        assert_eq!(
            purged, 1,
            "cutoff=200 must delete only the 100-stamped row; the 200-stamped \
             row sits on the cutoff and `created_at < ?2` keeps it. A flip to \
             `<=` would delete both and return 2. got: {purged}",
        );
        let mut stamps: Vec<u64> = s
            .recent(Some(MemoryTier::Working), 10)
            .await
            .unwrap()
            .iter()
            .map(|r| r.created_at)
            .collect();
        stamps.sort();
        assert_eq!(
            stamps,
            vec![200, 300],
            "survivors must be the cutoff-equal row (200) and the \
             strictly-greater one (300)",
        );

        let purged = s
            .purge_older_than(Some(MemoryTier::Working), 300)
            .await
            .unwrap();
        assert_eq!(
            purged, 1,
            "re-purge at cutoff=300 drops the now-strictly-less 200 while \
             keeping the cutoff-equal 300. got: {purged}",
        );
        let remaining = s.recent(Some(MemoryTier::Working), 10).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].created_at, 300);
    }

    #[tokio::test]
    async fn sqlite_persistence_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let id = Uuid::new_v4();
        {
            let s = SqliteStore::open(&path).unwrap();
            s.put(record(id, MemoryTier::LongTerm, "persist me", 42))
                .await
                .unwrap();
        }
        let s2 = SqliteStore::open(&path).unwrap();
        let got = s2.get(id).await.unwrap().unwrap();
        assert_eq!(got.text, "persist me");
    }

    #[tokio::test]
    async fn repair_dry_run_detach_parent_does_not_mutate() {
        let s = InMemoryStore::new();
        let id = Uuid::new_v4();
        let parent = Uuid::new_v4();
        let mut r = record(id, MemoryTier::Episodic, "child", 10);
        r.parent = Some(parent);
        s.put(r).await.unwrap();

        let outcome = s
            .repair(MemoryRepairRequest {
                mode: MemoryRepairMode::DryRun,
                command: MemoryRepairCommand::DetachParent {
                    id,
                    expected_parent: Some(parent),
                },
                reason: "parent was deleted".into(),
            })
            .await
            .unwrap();

        assert!(outcome.would_change);
        assert!(!outcome.changed);
        assert_eq!(outcome.after.unwrap().parent, None);
        assert_eq!(s.get(id).await.unwrap().unwrap().parent, Some(parent));
    }

    #[tokio::test]
    async fn repair_apply_detaches_parent() {
        let s = InMemoryStore::new();
        let id = Uuid::new_v4();
        let parent = Uuid::new_v4();
        let mut r = record(id, MemoryTier::Episodic, "child", 10);
        r.parent = Some(parent);
        s.put(r).await.unwrap();

        let outcome = s
            .repair(MemoryRepairRequest {
                mode: MemoryRepairMode::Apply,
                command: MemoryRepairCommand::DetachParent {
                    id,
                    expected_parent: Some(parent),
                },
                reason: "parent was deleted".into(),
            })
            .await
            .unwrap();

        assert!(outcome.changed);
        assert_eq!(s.get(id).await.unwrap().unwrap().parent, None);
    }

    #[tokio::test]
    async fn repair_rejects_parent_mismatch() {
        let s = InMemoryStore::new();
        let id = Uuid::new_v4();
        let actual_parent = Uuid::new_v4();
        let mut r = record(id, MemoryTier::Episodic, "child", 10);
        r.parent = Some(actual_parent);
        s.put(r).await.unwrap();

        let result = s
            .repair(MemoryRepairRequest {
                mode: MemoryRepairMode::Apply,
                command: MemoryRepairCommand::DetachParent {
                    id,
                    expected_parent: Some(Uuid::new_v4()),
                },
                reason: "stale parent repair".into(),
            })
            .await;
        assert!(matches!(result, Err(MemoryError::ParentMismatch { .. })));
        assert_eq!(
            s.get(id).await.unwrap().unwrap().parent,
            Some(actual_parent)
        );
    }

    #[tokio::test]
    async fn repair_apply_deletes_record() {
        let s = SqliteStore::open_in_memory().unwrap();
        let id = Uuid::new_v4();
        s.put(record(id, MemoryTier::Working, "delete me", 10))
            .await
            .unwrap();

        let outcome = s
            .repair(MemoryRepairRequest {
                mode: MemoryRepairMode::Apply,
                command: MemoryRepairCommand::DeleteRecord { id },
                reason: "operator confirmed unsafe memory".into(),
            })
            .await
            .unwrap();

        assert!(outcome.changed);
        assert!(outcome.after.is_none());
        assert!(s.get(id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn repair_backfills_provenance_metadata() {
        let s = SqliteStore::open_in_memory().unwrap();
        let id = Uuid::new_v4();
        let mut r = record(id, MemoryTier::LongTerm, "needs provenance", 10);
        r.metadata = serde_json::json!({"source": "import"});
        s.put(r).await.unwrap();

        let provenance = serde_json::json!({
            "kind": "manual_backfill",
            "evidence": "audit window checked"
        });
        let outcome = s
            .repair(MemoryRepairRequest {
                mode: MemoryRepairMode::Apply,
                command: MemoryRepairCommand::BackfillProvenance {
                    id,
                    provenance: provenance.clone(),
                },
                reason: "missing provenance evidence".into(),
            })
            .await
            .unwrap();

        assert!(outcome.changed);
        let got = s.get(id).await.unwrap().unwrap();
        assert_eq!(got.metadata["source"], "import");
        assert_eq!(got.metadata["provenance"], provenance);
    }

    #[test]
    fn plan_repair_backfill_provenance_pins_non_object_metadata_under_previous_metadata() {
        // covenant_memory::plan_repair handles
        // MemoryRepairCommand::BackfillProvenance by reading the
        // record's existing metadata. Line 197-204:
        //
        //   let mut metadata = match after.metadata {
        //       serde_json::Value::Object(map) => map,
        //       other => {
        //           let mut map = serde_json::Map::new();
        //           map.insert("previous_metadata".into(), other);
        //           map
        //       }
        //   };
        //   metadata.insert("provenance".into(), provenance.clone());
        //
        // The 'other' arm preserves any non-object metadata under a
        // 'previous_metadata' key in a new object, so backfilling
        // provenance on a legacy record with null/string/array
        // metadata does not silently lose the prior value. The
        // existing repair_backfills_provenance_metadata test
        // only exercises the object-already arm.
        //
        // A refactor that replaces the match with
        // metadata.as_object_mut().unwrap() under a 'metadata is
        // always an object' rationale would panic on legacy records
        // with non-object metadata. A refactor that drops the
        // previous_metadata wrap and just sets metadata =
        // json!({"provenance": ...}) would silently lose the prior
        // non-object metadata value with no audit signal.

        let provenance = serde_json::json!({"kind": "manual_backfill"});
        let cmd = |id: Uuid| MemoryRepairCommand::BackfillProvenance {
            id,
            provenance: provenance.clone(),
        };

        let id = Uuid::new_v4();

        let mut null_record = record(id, MemoryTier::LongTerm, "n", 1);
        null_record.metadata = serde_json::Value::Null;
        let after = plan_repair(&null_record, &cmd(id))
            .expect("BackfillProvenance must not error on null metadata")
            .expect("BackfillProvenance always returns Some(after)");
        assert_eq!(
            after.metadata["previous_metadata"],
            serde_json::Value::Null,
            "null metadata must be preserved verbatim under \
             previous_metadata — pins the non-object arm. \
             A refactor that dropped the wrap would surface here as a \
             missing previous_metadata key, and a refactor that swapped \
             the arm for as_object_mut().unwrap() would have panicked \
             before reaching this assertion",
        );
        assert_eq!(
            after.metadata["provenance"], provenance,
            "provenance must be inserted into the new map alongside \
             previous_metadata — pins that the provenance write happens \
             AFTER the non-object wrap, not BEFORE (which would let \
             previous_metadata shadow it if the key names ever \
             collided)",
        );

        let mut string_record = record(id, MemoryTier::LongTerm, "s", 1);
        string_record.metadata = serde_json::Value::String("legacy-tag".into());
        let after = plan_repair(&string_record, &cmd(id))
            .expect("BackfillProvenance must not error on string metadata")
            .expect("BackfillProvenance always returns Some(after)");
        assert_eq!(
            after.metadata["previous_metadata"], "legacy-tag",
            "string metadata must be preserved verbatim under \
             previous_metadata — pins that the wrap is value-agnostic \
             across primitive JSON types, not just null",
        );
        assert_eq!(after.metadata["provenance"], provenance);

        let mut array_record = record(id, MemoryTier::LongTerm, "a", 1);
        array_record.metadata = serde_json::json!(["tag-a", "tag-b"]);
        let after = plan_repair(&array_record, &cmd(id))
            .expect("BackfillProvenance must not error on array metadata")
            .expect("BackfillProvenance always returns Some(after)");
        assert_eq!(
            after.metadata["previous_metadata"],
            serde_json::json!(["tag-a", "tag-b"]),
            "array metadata must be preserved verbatim under \
             previous_metadata — pins that the wrap captures container \
             values, not just scalars; a refactor that flattened the \
             array into individual map keys under a 'merge legacy tags \
             into provenance' rationale would surface here",
        );
        assert_eq!(after.metadata["provenance"], provenance);

        let mut object_record = record(id, MemoryTier::LongTerm, "o", 1);
        object_record.metadata = serde_json::json!({"source": "import", "rev": 7});
        let after = plan_repair(&object_record, &cmd(id))
            .expect("BackfillProvenance must not error on object metadata")
            .expect("BackfillProvenance always returns Some(after)");
        assert_eq!(
            after.metadata["source"], "import",
            "object metadata happy path: existing keys must be \
             preserved without re-keying under previous_metadata — \
             pins the first match arm. A refactor that \
             accidentally fell through to the 'other' arm for objects \
             would surface here as a missing 'source' key and an \
             unexpected previous_metadata wrap",
        );
        assert_eq!(after.metadata["rev"], 7);
        assert_eq!(after.metadata["provenance"], provenance);
        assert!(
            after.metadata.get("previous_metadata").is_none(),
            "object-already arm must NOT introduce a previous_metadata \
             key — pins that the wrap is exclusive to the non-object \
             case; a refactor that always wrapped under \
             previous_metadata would double-nest the object metadata \
             and silently break callers reading the legacy keys",
        );
    }

    #[tokio::test]
    async fn compaction_dry_run_plans_without_mutating() {
        let s = InMemoryStore::new();
        let old_working = Uuid::new_v4();
        let child = Uuid::new_v4();
        let missing_parent = Uuid::new_v4();
        s.put(record(old_working, MemoryTier::Working, "old", 10))
            .await
            .unwrap();
        let mut child_record = record(child, MemoryTier::Episodic, "child", 30);
        child_record.parent = Some(missing_parent);
        s.put(child_record).await.unwrap();

        let outcome = s
            .compact(MemoryCompactionRequest {
                mode: MemoryRepairMode::DryRun,
                policy: MemoryCompactionPolicy {
                    delete_working_before_ms: Some(20),
                    detach_stale_parents: true,
                    ..MemoryCompactionPolicy::default()
                },
                reason: "routine dry-run".into(),
            })
            .await
            .unwrap();

        assert!(outcome.would_change);
        assert!(!outcome.changed);
        assert_eq!(outcome.deleted, vec![old_working]);
        assert_eq!(outcome.parents_detached, vec![child]);
        assert!(s.get(old_working).await.unwrap().is_some());
        assert_eq!(
            s.get(child).await.unwrap().unwrap().parent,
            Some(missing_parent)
        );
    }

    #[tokio::test]
    async fn compaction_apply_deletes_short_horizon_marks_longterm_and_detaches_parents() {
        let s = SqliteStore::open_in_memory().unwrap();
        let old_working = Uuid::new_v4();
        let old_episodic = Uuid::new_v4();
        let child = Uuid::new_v4();
        let longterm = Uuid::new_v4();
        s.put(record(old_working, MemoryTier::Working, "old working", 10))
            .await
            .unwrap();
        s.put(record(
            old_episodic,
            MemoryTier::Episodic,
            "old episodic",
            10,
        ))
        .await
        .unwrap();
        let mut child_record = record(child, MemoryTier::Episodic, "child", 50);
        child_record.parent = Some(old_working);
        s.put(child_record).await.unwrap();
        s.put(record(longterm, MemoryTier::LongTerm, "durable fact", 10))
            .await
            .unwrap();

        let outcome = s
            .compact(MemoryCompactionRequest {
                mode: MemoryRepairMode::Apply,
                policy: MemoryCompactionPolicy {
                    delete_working_before_ms: Some(20),
                    delete_episodic_before_ms: Some(20),
                    mark_longterm_stale_before_ms: Some(20),
                    detach_stale_parents: true,
                    marked_at_ms: Some(99),
                },
                reason: "age-based compaction".into(),
            })
            .await
            .unwrap();

        assert!(outcome.changed);
        let mut expected_deleted = vec![old_working, old_episodic];
        expected_deleted.sort();
        assert_eq!(outcome.deleted, expected_deleted);
        assert_eq!(outcome.parents_detached, vec![child]);
        assert_eq!(outcome.stale_marked, vec![longterm]);
        assert!(s.get(old_working).await.unwrap().is_none());
        assert!(s.get(old_episodic).await.unwrap().is_none());
        assert_eq!(s.get(child).await.unwrap().unwrap().parent, None);
        let durable = s.get(longterm).await.unwrap().unwrap();
        assert_eq!(durable.metadata["stale_context"]["marked_at_ms"], 99);
        assert_eq!(
            durable.metadata["stale_context"]["reason"],
            "age-based compaction"
        );
    }

    #[test]
    fn plan_compaction_stale_context_pins_non_object_metadata_under_previous_metadata() {
        // covenant_memory::plan_compaction marks
        // LongTerm records stale when their created_at falls below
        // mark_longterm_stale_before_ms. Lines 274-281 use the same
        // non-object preservation pattern as plan_repair:
        //
        //   let mut metadata = match after.metadata {
        //       serde_json::Value::Object(map) => map,
        //       other => {
        //           let mut map = serde_json::Map::new();
        //           map.insert("previous_metadata".into(), other);
        //           map
        //       }
        //   };
        //
        // Every existing compaction test uses record()
        // which seeds metadata = serde_json::json!({}); so they all
        // hit the object-already arm. The 'other' arm
        // that handles legacy LongTerm records with
        // null/string/array metadata is dead code from the test
        // suite's perspective. A refactor that swapped the match for
        // metadata.as_object_mut().unwrap() under a 'metadata is
        // always an object' rationale would panic mid-compaction the
        // first time a legacy record aged past
        // mark_longterm_stale_before_ms.

        let policy = MemoryCompactionPolicy {
            mark_longterm_stale_before_ms: Some(100),
            marked_at_ms: Some(150),
            ..MemoryCompactionPolicy::default()
        };
        let request = MemoryCompactionRequest {
            mode: MemoryRepairMode::Apply,
            policy,
            reason: "stale-context pin".into(),
        };

        let build = |metadata: serde_json::Value| {
            let mut r = record(Uuid::new_v4(), MemoryTier::LongTerm, "legacy", 10);
            r.metadata = metadata;
            r
        };

        let cases = [
            ("null", serde_json::Value::Null, serde_json::Value::Null),
            (
                "string",
                serde_json::Value::String("legacy-tag".into()),
                serde_json::Value::String("legacy-tag".into()),
            ),
            (
                "array",
                serde_json::json!(["tag-a", "tag-b"]),
                serde_json::json!(["tag-a", "tag-b"]),
            ),
        ];
        for (label, initial, expected_previous) in cases {
            let records = vec![build(initial)];
            let (outcome, updates) = plan_compaction(&records, &request);
            assert!(
                outcome.changed,
                "{label}: outcome.changed must be true in Apply mode \
                 when the LongTerm record qualifies for stale marking",
            );
            assert_eq!(
                updates.len(),
                1,
                "{label}: exactly one updated record must be returned \
                 (the stale-marked LongTerm record); a refactor that \
                 skipped writes for non-object metadata would surface \
                 as an empty updates list",
            );
            let after = &updates[0];
            assert_eq!(
                after.metadata["previous_metadata"], expected_previous,
                "{label}: the non-object metadata must be preserved \
                 verbatim under previous_metadata — pins the 'other' \
                 'other' arm. A refactor that dropped the \
                 wrap would surface as a missing previous_metadata \
                 key; a refactor that swapped the match for \
                 as_object_mut().unwrap() would have panicked before \
                 reaching this assertion",
            );
            assert_eq!(
                after.metadata["stale_context"]["marked_at_ms"], 150,
                "{label}: stale_context.marked_at_ms must reflect the \
                 policy field — pins that the stale_context write \
                 happens AFTER the non-object wrap, not BEFORE",
            );
            assert_eq!(
                after.metadata["stale_context"]["reason"], "stale-context pin",
                "{label}: stale_context.reason must reflect the \
                 request.reason — pins reason propagation through the \
                 non-object preservation path",
            );
        }

        let object_record = build(serde_json::json!({"source": "import"}));
        let (outcome, updates) = plan_compaction(&[object_record], &request);
        assert!(outcome.changed);
        let after = &updates[0];
        assert_eq!(
            after.metadata["source"], "import",
            "object-already arm: existing keys must survive the \
             stale_context insert — pins the first match arm at line \
             275. A refactor that always wrapped under \
             previous_metadata would shadow the 'source' key",
        );
        assert_eq!(after.metadata["stale_context"]["marked_at_ms"], 150);
        assert!(
            after.metadata.get("previous_metadata").is_none(),
            "object-already arm must NOT introduce previous_metadata \
             — pins that the wrap is exclusive to the non-object \
             case",
        );
    }

    #[test]
    fn plan_compaction_skips_remarking_longterm_record_that_already_carries_matching_stale_context()
    {
        // plan_compaction stale-marks a LongTerm record by inserting a
        // stale_context object, but only when the record does not already
        // hold that exact value:
        //
        //   if metadata.get("stale_context") != Some(&stale_context) {
        //       metadata.insert("stale_context", stale_context);
        //       ... stale_marked.push(after.id); changed = true;
        //   } else {
        //       after.metadata = Value::Object(metadata);  // untouched
        //   }
        //
        // The else arm is the compaction idempotency guarantee documented
        // on compact() ("already present ... keeps repeated invocations
        // idempotent"): a second run over an already-marked corpus is a
        // no-op. Every other compaction test seeds records WITHOUT a
        // pre-existing stale_context, so they only exercise the insert
        // arm. Without this test a refactor that dropped the inequality
        // guard and always inserted + pushed would re-mark unchanged
        // records on every run — flipping would_change/changed to true on
        // a genuine no-op, re-emitting the row in `updates` (rewriting it
        // and recording a mutation each cycle at the store layer), and
        // poisoning stale_marked as a what-changed-this-run signal.
        let policy = MemoryCompactionPolicy {
            mark_longterm_stale_before_ms: Some(100),
            marked_at_ms: Some(150),
            ..MemoryCompactionPolicy::default()
        };
        let request = MemoryCompactionRequest {
            mode: MemoryRepairMode::Apply,
            policy,
            reason: "idempotent re-run".into(),
        };

        // created_at (10) < cutoff (100), so the record still qualifies
        // for stale marking — the skip must come from the value guard, not
        // from the record falling outside the staleness window. The seeded
        // stale_context is byte-identical to what this request would write:
        // marked_at_ms = policy.marked_at_ms (150), reason = request.reason.
        let id = Uuid::new_v4();
        let mut already_marked = record(id, MemoryTier::LongTerm, "durable", 10);
        already_marked.metadata = serde_json::json!({
            "stale_context": { "marked_at_ms": 150, "reason": "idempotent re-run" },
        });

        let (outcome, updates) = plan_compaction(&[already_marked], &request);

        assert!(
            outcome.stale_marked.is_empty(),
            "a LongTerm record already carrying the exact stale_context \
             must not be re-listed in stale_marked — pins the inequality \
             guard's else arm; a refactor that always re-marked would \
             surface here with stale_marked = [{id}]",
        );
        assert!(
            !outcome.would_change,
            "re-running compaction over an already-marked corpus must be a \
             no-op: would_change stays false when nothing is deleted, \
             detached, or freshly marked",
        );
        assert!(
            !outcome.changed,
            "changed must stay false on an idempotent re-run even in Apply \
             mode — changed is gated on would_change",
        );
        assert!(
            updates.is_empty(),
            "no updated record may be emitted for an unchanged stale \
             record — a spurious update would rewrite the row and record a \
             mutation on every compaction cycle",
        );
    }

    #[test]
    fn plan_repair_detach_parent_pins_parent_mismatch_arms_and_field_composition() {
        // covenant_memory::plan_repair, DetachParent arm, guards on
        //
        //   if expected_parent.is_some() && record.parent != *expected_parent
        //
        // The guard implements a four-way input semantics:
        //   (1) expected=None always passes (operator opts out of the
        //       check) — the detach proceeds regardless of what parent
        //       the record actually holds;
        //   (2) expected=Some(X), actual=Some(X) passes (matched);
        //   (3) expected=Some(X), actual=Some(Y) where X!=Y returns
        //       Err(ParentMismatch { id, expected: Some(X), actual:
        //       Some(Y) });
        //   (4) expected=Some(X), actual=None returns
        //       Err(ParentMismatch { id, expected: Some(X), actual:
        //       None }).
        //
        // Of these four arms, only arm (3) is pinned today via
        // repair_rejects_parent_mismatch, and only via
        // matches!(_, Err(MemoryError::ParentMismatch { .. })) — the
        // existing pin does NOT inspect the ParentMismatch field
        // values, so a refactor that swapped expected and actual under
        // a 'sort fields alphabetically' rationale would silently flip
        // operator-facing error messages while the existing pin still
        // passes. Arms (1) and (4) have no direct test.
        //
        // A refactor that flipped the != to == under a 'simplify the
        // negation' rationale would invert the entire mismatch
        // contract — matching parents would error and mismatching
        // parents would succeed; pinning the four-arm matrix means
        // each input combination explicitly anchors which side returns
        // Ok vs Err so a flipped check fails three of four arms.
        let id = Uuid::new_v4();
        let parent_a = Uuid::new_v4();
        let parent_b = Uuid::new_v4();

        let with_parent = |p: Option<Uuid>| {
            let mut r = record(id, MemoryTier::Episodic, "child", 10);
            r.parent = p;
            r
        };
        let detach = |expected: Option<Uuid>| MemoryRepairCommand::DetachParent {
            id,
            expected_parent: expected,
        };

        // Arm 1a: expected=None, actual=None — guard short-circuits;
        // detach proceeds.
        let after = plan_repair(&with_parent(None), &detach(None))
            .expect("expected=None must always pass; the guard short-circuits via expected_parent.is_some()")
            .expect("DetachParent always returns Some(after) on success");
        assert_eq!(
            after.parent, None,
            "arm 1a (expected=None, actual=None): detach proceeds; \
             after.parent must be None — pins that DetachParent \
             always sets parent to None on success regardless of the \
             input combination",
        );

        // Arm 1b: expected=None, actual=Some(X) — guard short-circuits;
        // detach proceeds even though the record HAS a parent. This is
        // the documented 'I do not care what parent the record has'
        // contract; a refactor that dropped the expected_parent.is_some()
        // guard would silently turn this into ParentMismatch because
        // record.parent (Some(X)) != *expected_parent (None) is true.
        let after = plan_repair(&with_parent(Some(parent_a)), &detach(None))
            .expect(
                "expected=None must pass even when actual=Some(X); the \
                 expected_parent.is_some() guard is the documented \
                 'operator opts out of the parent check' contract — a \
                 refactor that dropped the guard under a 'simplify the \
                 boolean' rationale would silently turn this arm into \
                 Err(ParentMismatch) because None != Some(X) is true",
            )
            .expect("DetachParent always returns Some(after) on success");
        assert_eq!(after.parent, None, "arm 1b: parent cleared");

        // Arm 2: expected=Some(X), actual=Some(X) — guard true, equality
        // false, no error; detach proceeds.
        let after = plan_repair(&with_parent(Some(parent_a)), &detach(Some(parent_a)))
            .expect(
                "matched expected/actual must pass — pins the equality semantics on the happy path",
            )
            .expect("DetachParent always returns Some(after) on success");
        assert_eq!(after.parent, None, "arm 2: parent cleared on match");

        // Arm 3: expected=Some(X), actual=Some(Y) where X!=Y — guard
        // true, equality false (records differ), ParentMismatch fires.
        // Inspect the FIELD VALUES so a refactor that swapped expected
        // and actual fails the destructure assertion.
        let err = plan_repair(&with_parent(Some(parent_b)), &detach(Some(parent_a))).expect_err(
            "expected=Some(X), actual=Some(Y) with X!=Y must \
                 return ParentMismatch — the existing \
                 repair_rejects_parent_mismatch pin covers \
                 this arm only at the matches!(_, Err(_)) level; this \
                 destructure pins the field VALUES so a refactor that \
                 swapped expected and actual cannot land silently",
        );
        match err {
            MemoryError::ParentMismatch {
                id: err_id,
                expected,
                actual,
            } => {
                assert_eq!(err_id, id, "ParentMismatch.id must equal record.id");
                assert_eq!(
                    expected,
                    Some(parent_a),
                    "ParentMismatch.expected must equal *expected_parent — \
                     a refactor that swapped expected and actual under a \
                     'sort fields alphabetically' rationale would surface \
                     here as expected==Some(parent_b)",
                );
                assert_eq!(
                    actual,
                    Some(parent_b),
                    "ParentMismatch.actual must equal record.parent — \
                     paired with the expected assertion above, this \
                     destructure makes the field-swap regression \
                     fail BOTH arms instead of silently passing",
                );
            }
            other => panic!("expected ParentMismatch, got {other:?}"),
        }

        // Arm 4: expected=Some(X), actual=None — guard true, equality
        // false (None != Some(X)), ParentMismatch fires. This arm is
        // not exercised by repair_rejects_parent_mismatch (which uses
        // actual=Some(Y)). Field destructure pins the actual=None
        // value explicitly.
        let err = plan_repair(&with_parent(None), &detach(Some(parent_a))).expect_err(
            "expected=Some(X), actual=None must return ParentMismatch — \
             the guard fires because expected_parent.is_some() is true \
             and record.parent (None) != *expected_parent (Some(X)) is \
             true; this arm is not covered by \
             repair_rejects_parent_mismatch which only exercises \
             actual=Some(Y)",
        );
        match err {
            MemoryError::ParentMismatch {
                id: err_id,
                expected,
                actual,
            } => {
                assert_eq!(err_id, id);
                assert_eq!(
                    expected,
                    Some(parent_a),
                    "ParentMismatch.expected must equal *expected_parent on the actual=None arm",
                );
                assert_eq!(
                    actual, None,
                    "ParentMismatch.actual must equal record.parent (None) — \
                     pinning the None arm explicitly catches a refactor \
                     that special-cased actual=None to Some(Uuid::nil()) \
                     for 'consistent typing' which would silently change \
                     operator-facing error payloads",
                );
            }
            other => panic!("expected ParentMismatch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn compaction_rejects_empty_policy_and_reason() {
        let s = InMemoryStore::new();
        let empty_policy = s
            .compact(MemoryCompactionRequest {
                mode: MemoryRepairMode::DryRun,
                policy: MemoryCompactionPolicy::default(),
                reason: "no-op".into(),
            })
            .await;
        assert!(matches!(
            empty_policy,
            Err(MemoryError::InvalidCompaction(_))
        ));

        let empty_reason = s
            .compact(MemoryCompactionRequest {
                mode: MemoryRepairMode::DryRun,
                policy: MemoryCompactionPolicy {
                    detach_stale_parents: true,
                    ..MemoryCompactionPolicy::default()
                },
                reason: " ".into(),
            })
            .await;
        assert!(matches!(
            empty_reason,
            Err(MemoryError::InvalidCompaction(_))
        ));
    }

    #[tokio::test]
    async fn plan_compaction_marked_at_ms_defaults_to_before_ms_when_policy_field_is_none() {
        // covenant_memory::plan_compaction binds the
        // stale_context.marked_at_ms value via:
        //
        //   let marked_at_ms = request.policy.marked_at_ms.unwrap_or(before_ms);
        //
        // where `before_ms` is the mark_longterm_stale_before_ms cutoff
        // bound on the outer `if let Some(before_ms) = ...`. When
        // MemoryCompactionPolicy::marked_at_ms is None, the cutoff
        // itself is the recorded timestamp — anchoring stale_context to
        // the policy boundary that triggered the mark.
        //
        // The existing compaction_apply_deletes_short_horizon_marks_longterm_and_detaches_parents
        // always sets marked_at_ms = Some(99); the
        // default-to-cutoff arm is exercised by no direct test. A
        // refactor that swapped .unwrap_or(before_ms) for
        // .unwrap_or(0), .unwrap_or_default(), or
        // .unwrap_or_else(now_ms) under a 'use sensible default' pass
        // would silently change every stale_context.marked_at_ms emitted
        // by operators who omit the field — dashboards would see 0 or
        // wall-clock timestamps that no longer bind to the policy
        // cutoff, and audit reconstruction would lose its anchor.
        //
        // Cross-bind: the sibling
        // compaction_apply_deletes_short_horizon_marks_longterm_and_detaches_parents
        // pin covers the Some(M) override; this pin closes the None
        // default arm and re-asserts Some(M) inside the same test so
        // both arms travel together.

        let s = InMemoryStore::new();
        let id_default = Uuid::new_v4();
        let id_explicit = Uuid::new_v4();
        s.put(record(id_default, MemoryTier::LongTerm, "defaulted", 10))
            .await
            .unwrap();
        s.put(record(id_explicit, MemoryTier::LongTerm, "explicit", 10))
            .await
            .unwrap();

        // Default arm: marked_at_ms = None, mark_longterm_stale_before_ms = Some(200).
        // The recorded stale_context.marked_at_ms must equal the cutoff (200).
        let outcome = s
            .compact(MemoryCompactionRequest {
                mode: MemoryRepairMode::Apply,
                policy: MemoryCompactionPolicy {
                    mark_longterm_stale_before_ms: Some(200),
                    marked_at_ms: None,
                    ..MemoryCompactionPolicy::default()
                },
                reason: "default-cutoff stale-mark".into(),
            })
            .await
            .unwrap();

        assert!(
            outcome.changed,
            "apply mode with a record below the cutoff must report changed=true",
        );
        assert!(
            outcome.stale_marked.contains(&id_default),
            "the defaulted record must appear in stale_marked: got {:?}",
            outcome.stale_marked,
        );
        let got_default = s.get(id_default).await.unwrap().unwrap();
        assert_eq!(
            got_default.metadata["stale_context"]["marked_at_ms"], 200,
            "marked_at_ms must default to the before_ms cutoff (200) when \
             MemoryCompactionPolicy::marked_at_ms is None — a refactor that \
             swapped .unwrap_or(before_ms) for .unwrap_or(0) would silently \
             surface 0 here on every operator dashboard, breaking the \
             policy-cutoff anchor that audit reconstruction binds to; a \
             refactor to .unwrap_or_else(now_ms) would surface a non-\
             deterministic wall-clock timestamp instead, breaking the \
             reproducibility property that plan_compaction is currently a \
             pure function of (records, policy, reason); a refactor that \
             omitted the field entirely when policy.marked_at_ms is None \
             would make stale_context.marked_at_ms absent here, breaking \
             JSON consumers that assume a uniform schema across records",
        );
        assert_eq!(
            got_default.metadata["stale_context"]["reason"], "default-cutoff stale-mark",
            "reason must travel verbatim into stale_context alongside the \
             marked_at_ms default arm — pinning both fields anchors the \
             two-field stale_context contract that plan_compaction \
             constructs",
        );

        // Explicit-override arm: marked_at_ms = Some(150), mark_longterm_stale_before_ms = Some(300).
        // The recorded stale_context.marked_at_ms must equal the explicit
        // override (150), NOT the cutoff (300). Pinning the override arm
        // here prevents a 'simplify by always using before_ms' regression
        // that would silently drop the Some(M) path while leaving the
        // default arm intact.
        let outcome = s
            .compact(MemoryCompactionRequest {
                mode: MemoryRepairMode::Apply,
                policy: MemoryCompactionPolicy {
                    mark_longterm_stale_before_ms: Some(300),
                    marked_at_ms: Some(150),
                    ..MemoryCompactionPolicy::default()
                },
                reason: "explicit-override stale-mark".into(),
            })
            .await
            .unwrap();

        assert!(outcome.changed);
        assert!(outcome.stale_marked.contains(&id_explicit));
        let got_explicit = s.get(id_explicit).await.unwrap().unwrap();
        assert_eq!(
            got_explicit.metadata["stale_context"]["marked_at_ms"], 150,
            "explicit marked_at_ms = Some(150) must win over the cutoff \
             (300) — a refactor that always used before_ms (dropping the \
             .unwrap_or arm) would silently surface 300 here while the \
             default-arm assertion above would still pass; pinning the \
             explicit override on the same record family anchors both \
             halves of the .unwrap_or contract",
        );
    }

    #[test]
    fn plan_compaction_pins_inclusive_keep_arm_at_each_cutoff_boundary() {
        // plan_compaction gates all three age-based actions on a STRICT
        // `created_at < before` comparison: delete_working_before_ms
        // (lib.rs:297), delete_episodic_before_ms (:305), and
        // mark_longterm_stale_before_ms (:342). The `<` is exclusive, so a
        // record created EXACTLY at the cutoff is on the keep side —
        // working/episodic records are not deleted and a long-term record
        // is not marked stale. Every other compaction test seeds
        // created_at strictly below the cutoff (10 vs 20, 10 vs 200), so
        // the keep-arm at created_at == before is pinned by no test. A
        // `<` -> `<=` flip on a delete site would silently destroy a
        // record the operator's "delete strictly older than N" policy
        // intended to keep — an irreversible loss of the boundary record;
        // the same flip on the stale-mark site would mark a long-term
        // record stale one tick early. Pin the equality keep-arm at all
        // three sites and bracket each from below (created_at == before-1
        // must still act) so the cutoff is proven exact, not off by one.
        const CUTOFF: u64 = 100;
        let working_at = Uuid::new_v4();
        let working_below = Uuid::new_v4();
        let episodic_at = Uuid::new_v4();
        let episodic_below = Uuid::new_v4();
        let longterm_at = Uuid::new_v4();
        let longterm_below = Uuid::new_v4();

        let records = vec![
            record(working_at, MemoryTier::Working, "working at cutoff", CUTOFF),
            record(
                working_below,
                MemoryTier::Working,
                "working below",
                CUTOFF - 1,
            ),
            record(
                episodic_at,
                MemoryTier::Episodic,
                "episodic at cutoff",
                CUTOFF,
            ),
            record(
                episodic_below,
                MemoryTier::Episodic,
                "episodic below",
                CUTOFF - 1,
            ),
            record(
                longterm_at,
                MemoryTier::LongTerm,
                "longterm at cutoff",
                CUTOFF,
            ),
            record(
                longterm_below,
                MemoryTier::LongTerm,
                "longterm below",
                CUTOFF - 1,
            ),
        ];
        // plan_compaction is pure, so the mode only colors `changed`; the
        // deleted/stale_marked sets are computed independent of mode.
        let request = MemoryCompactionRequest {
            mode: MemoryRepairMode::Apply,
            policy: MemoryCompactionPolicy {
                delete_working_before_ms: Some(CUTOFF),
                delete_episodic_before_ms: Some(CUTOFF),
                mark_longterm_stale_before_ms: Some(CUTOFF),
                ..MemoryCompactionPolicy::default()
            },
            reason: "boundary-equality pin".into(),
        };

        let (outcome, _updates) = plan_compaction(&records, &request);

        assert!(
            !outcome.deleted.contains(&working_at),
            "a Working record created exactly at delete_working_before_ms must be \
             kept (created_at < before is strict); a `<` -> `<=` flip would delete it",
        );
        assert!(
            !outcome.deleted.contains(&episodic_at),
            "an Episodic record created exactly at delete_episodic_before_ms must be \
             kept (created_at < before is strict); a `<` -> `<=` flip would delete it",
        );
        let mut expected_deleted = vec![working_below, episodic_below];
        expected_deleted.sort();
        assert_eq!(
            outcome.deleted, expected_deleted,
            "only the strictly-below-cutoff working/episodic records may be deleted — \
             exactly the CUTOFF-1 pair and neither at-cutoff record; this brackets the \
             `<` delete boundary from both sides so an off-by-one cannot pass",
        );

        assert!(
            !outcome.stale_marked.contains(&longterm_at),
            "a LongTerm record created exactly at mark_longterm_stale_before_ms must NOT \
             be marked stale (created_at < before is strict); a `<` -> `<=` flip would \
             mark it one tick early",
        );
        assert_eq!(
            outcome.stale_marked,
            vec![longterm_below],
            "only the strictly-below-cutoff LongTerm record may be stale-marked, pinning \
             the exclusive stale-mark cutoff against an off-by-one",
        );
    }

    #[test]
    fn plan_compaction_pins_tier_matched_delete_cutoffs_under_differing_horizons() {
        // plan_compaction deletes a Working record only on
        // delete_working_before_ms (lib.rs:293-300) and an Episodic record
        // only on delete_episodic_before_ms (lib.rs:301-308); LongTerm has no
        // delete arm. The two cutoffs are independent and tier-matched, but
        // every other compaction test sets them EQUAL (apply: both 20;
        // plan_compaction_pins_inclusive_keep_arm_at_each_cutoff_boundary:
        // both 100) or sets only the working cutoff with a too-new episodic
        // record (compaction_dry_run_plans_without_mutating: episodic@30 vs
        // cutoff 20). Under equal cutoffs a swap of which field each arm
        // reads — or an arm widened to also match the other tier on the wider
        // horizon — leaves `deleted` unchanged and survives. Pin the binding
        // with DIFFERENT horizons (working=100, episodic=200) and records in
        // the gap between them: a Working record at 150 is above its own
        // cutoff and must be KEPT (a swap reading episodic's 200 would delete
        // it); an Episodic record at 150 is below its own cutoff and must be
        // DELETED (a swap reading working's 100 would keep it). An old
        // LongTerm record stays immune.
        const WORKING_CUTOFF: u64 = 100;
        const EPISODIC_CUTOFF: u64 = 200;
        let working_old = Uuid::new_v4();
        let working_between = Uuid::new_v4();
        let episodic_between = Uuid::new_v4();
        let episodic_above = Uuid::new_v4();
        let longterm_old = Uuid::new_v4();

        let records = vec![
            record(working_old, MemoryTier::Working, "working old", 50),
            record(
                working_between,
                MemoryTier::Working,
                "working between cutoffs",
                150,
            ),
            record(
                episodic_between,
                MemoryTier::Episodic,
                "episodic between cutoffs",
                150,
            ),
            record(
                episodic_above,
                MemoryTier::Episodic,
                "episodic above its cutoff",
                250,
            ),
            record(longterm_old, MemoryTier::LongTerm, "durable fact", 50),
        ];
        let request = MemoryCompactionRequest {
            mode: MemoryRepairMode::Apply,
            policy: MemoryCompactionPolicy {
                delete_working_before_ms: Some(WORKING_CUTOFF),
                delete_episodic_before_ms: Some(EPISODIC_CUTOFF),
                ..MemoryCompactionPolicy::default()
            },
            reason: "tier-matched cutoff pin".into(),
        };

        let (outcome, _updates) = plan_compaction(&records, &request);

        // The Working record at 150 is ABOVE its own 100 cutoff: it must be
        // kept. A swap that read the wider episodic 200 cutoff would delete it.
        assert!(
            !outcome.deleted.contains(&working_between),
            "a Working record above delete_working_before_ms (150 vs 100) must be kept; \
             deleting it means the Working arm read the wider episodic cutoff (200)",
        );
        // The Episodic record at 150 is BELOW its own 200 cutoff: it must be
        // deleted. A swap that read the narrower working 100 cutoff would keep it.
        assert!(
            outcome.deleted.contains(&episodic_between),
            "an Episodic record below delete_episodic_before_ms (150 vs 200) must be deleted; \
             keeping it means the Episodic arm read the narrower working cutoff (100)",
        );
        // The Episodic record at 250 is above its own 200 cutoff: a regression
        // that dropped the episodic cutoff and deleted the whole tier is caught here.
        assert!(
            !outcome.deleted.contains(&episodic_above),
            "an Episodic record above delete_episodic_before_ms (250 vs 200) must be kept",
        );
        // LongTerm has no delete arm regardless of age.
        assert!(
            !outcome.deleted.contains(&longterm_old),
            "a LongTerm record is never deleted by age — only stale-marked",
        );

        let mut expected = vec![working_old, episodic_between];
        expected.sort();
        assert_eq!(
            outcome.deleted, expected,
            "deleted must be exactly the tier-matched set: working_old (50<100) and \
             episodic_between (150<200); working_between (150) and episodic_above (250) sit \
             above their own cutoffs and longterm_old is immune, so an arm swap, a cross-tier \
             widening, or any LongTerm delete path all change this set",
        );
    }

    #[test]
    fn plan_compaction_detach_stale_parents_flag_gates_detachment() {
        // plan_compaction detaches a record from a stale parent (one absent
        // from the retained set) only when policy.detach_stale_parents is set
        // (lib.rs:330). Every compaction test that exercises detachment sets
        // the flag TRUE (compaction_dry_run_plans_without_mutating,
        // compaction_apply_deletes_short_horizon_marks_longterm_and_detaches_parents);
        // the two tests that set it false (sqlite_compact_apply_rolls_back...,
        // sqlite_compact_apply_persists_full_plan...) seed no parented
        // records, so the OFF arm — a record whose parent is dangling stays
        // attached when the operator did not opt in — is pinned by no test.
        // Dropping the `if detach_stale_parents` guard would always rewrite
        // parents, silently severing lineage the operator never asked to
        // touch. Pin both arms over the SAME dangling-parent input so the
        // flag is proven to be the sole discriminator.
        let child = Uuid::new_v4();
        let missing_parent = Uuid::new_v4();
        let mut child_record = record(child, MemoryTier::Episodic, "child", 10);
        child_record.parent = Some(missing_parent);
        let records = vec![child_record];

        let off = MemoryCompactionRequest {
            mode: MemoryRepairMode::Apply,
            policy: MemoryCompactionPolicy {
                detach_stale_parents: false,
                ..MemoryCompactionPolicy::default()
            },
            reason: "detach gate off".into(),
        };
        let (outcome_off, updates_off) = plan_compaction(&records, &off);
        assert!(
            outcome_off.parents_detached.is_empty(),
            "with detach_stale_parents=false a dangling parent must NOT be detached; \
             a dropped or inverted gate would sever it",
        );
        assert!(
            !outcome_off.would_change,
            "the off arm performs no work over this input, so would_change must be false",
        );
        assert!(
            updates_off.is_empty(),
            "no record may be rewritten when the detach flag is off and no other policy fires",
        );

        // Control: same record, flag ON. The parent IS dangling (absent from
        // the retained set), so detachment proceeds — proving the parent was
        // detachable all along and only the flag held it back.
        let on = MemoryCompactionRequest {
            mode: MemoryRepairMode::Apply,
            policy: MemoryCompactionPolicy {
                detach_stale_parents: true,
                ..MemoryCompactionPolicy::default()
            },
            reason: "detach gate on".into(),
        };
        let (outcome_on, updates_on) = plan_compaction(&records, &on);
        assert_eq!(
            outcome_on.parents_detached,
            vec![child],
            "with detach_stale_parents=true the dangling parent is detached; flipping the \
             !retained.contains(parent) detection would leave this orphan attached",
        );
        assert_eq!(
            updates_on.len(),
            1,
            "the single detached record is emitted as exactly one update",
        );
        assert_eq!(
            updates_on[0].parent, None,
            "detachment sets the record's parent to None",
        );
    }

    #[test]
    fn plan_compaction_apply_mode_no_op_plan_is_not_marked_changed() {
        // outcome.changed = (mode == Apply) && would_change (lib.rs:384), and
        // would_change is false unless something was deleted, stale-marked, or
        // detached (lib.rs:378-379). Every test that asserts changed seeds a
        // plan that DOES change something; the DryRun tests assert !changed
        // because mode != Apply, not because would_change is false. The
        // Apply-mode no-op — a non-empty policy whose cutoffs match nothing —
        // is pinned by no test, yet it is what gates the daemon from opening a
        // write transaction and emitting a MemoryCompactionApplied audit row
        // for a compaction that touched zero rows. Simplifying changed to
        // (mode == Apply) would pass every existing test but flip changed to
        // true here.
        let untouched = Uuid::new_v4();
        // created_at 100 is well above the cutoff, so nothing is deleted.
        let records = vec![record(untouched, MemoryTier::Working, "fresh", 100)];
        let request = MemoryCompactionRequest {
            mode: MemoryRepairMode::Apply,
            policy: MemoryCompactionPolicy {
                delete_working_before_ms: Some(5),
                ..MemoryCompactionPolicy::default()
            },
            reason: "no-op apply".into(),
        };

        let (outcome, updates) = plan_compaction(&records, &request);

        assert!(
            !outcome.would_change,
            "a policy whose cutoff (5) matches no record (created_at 100) plans no change",
        );
        assert!(
            !outcome.changed,
            "changed must stay false on a no-op plan even in Apply mode — it is \
             (mode == Apply) && would_change, not mode == Apply alone; a spurious true here \
             makes the daemon emit a MemoryCompactionApplied audit row for zero rows",
        );
        assert!(outcome.deleted.is_empty(), "no record may be deleted");
        assert!(
            outcome.stale_marked.is_empty(),
            "no record may be stale-marked"
        );
        assert!(
            outcome.parents_detached.is_empty(),
            "no record may be detached"
        );
        assert!(
            updates.is_empty(),
            "a no-op plan emits no record updates to apply"
        );
    }

    #[test]
    fn plan_compaction_detaches_only_stale_parent_not_retained() {
        // With detach_stale_parents on, plan_compaction detaches a parent only
        // when it is absent from the retained set — `if let Some(parent) =
        // after.parent { if !retained.contains(&parent)` (lib.rs:331-332).
        // Every detach test uses a parent that is NOT retained (missing, or a
        // deleted old_working), so the detection predicate is unpinned: no
        // test has a child whose parent SURVIVES the compaction and asserts it
        // stays attached. Run both shapes in ONE flag-on plan with no delete
        // cutoffs (so every input record is retained): a child whose parent is
        // a surviving record must keep it; a child whose parent is a dangling
        // id must lose it. An unconditional detach, or an inverted membership
        // test, flips which child is detached.
        let alive_parent = Uuid::new_v4();
        let child_alive = Uuid::new_v4();
        let child_dangling = Uuid::new_v4();
        let dangling_parent = Uuid::new_v4();

        let mut child_alive_record = record(child_alive, MemoryTier::Episodic, "kept lineage", 50);
        child_alive_record.parent = Some(alive_parent);
        let mut child_dangling_record = record(child_dangling, MemoryTier::Episodic, "orphan", 50);
        child_dangling_record.parent = Some(dangling_parent);
        let records = vec![
            record(alive_parent, MemoryTier::LongTerm, "surviving parent", 100),
            child_alive_record,
            child_dangling_record,
        ];

        // detach on, NO delete cutoffs => nothing is deleted, so alive_parent
        // is in the retained set and child_alive's edge is live.
        let request = MemoryCompactionRequest {
            mode: MemoryRepairMode::Apply,
            policy: MemoryCompactionPolicy {
                detach_stale_parents: true,
                ..MemoryCompactionPolicy::default()
            },
            reason: "stale-only detach pin".into(),
        };

        let (outcome, updates) = plan_compaction(&records, &request);

        assert!(
            outcome.deleted.is_empty(),
            "no delete cutoff is set, so every record is retained",
        );
        assert_eq!(
            outcome.parents_detached,
            vec![child_dangling],
            "only the child whose parent is absent from the retained set may be detached; \
             an unconditional detach would also list child_alive and an inverted \
             retained.contains would list child_alive INSTEAD of child_dangling",
        );
        assert!(
            !outcome.parents_detached.contains(&child_alive),
            "a child whose parent survives the compaction keeps its parent edge",
        );
        assert_eq!(
            updates.len(),
            1,
            "exactly one record changed: the orphan whose dangling parent was detached",
        );
        assert_eq!(
            updates[0].id, child_dangling,
            "the detached record is the orphan"
        );
        assert_eq!(
            updates[0].parent, None,
            "detachment clears the parent to None"
        );
    }

    #[test]
    fn memory_error_display_messages_pin_five_string_variant_format_strings() {
        let worker = format!("{}", MemoryError::Worker("channel closed".into()));
        assert_eq!(
            worker, "worker: channel closed",
            "MemoryError::Worker Display drifted (typo or dropped 'worker:' prefix regression class)"
        );

        let not_found = format!("{}", MemoryError::RecordNotFound(Uuid::nil()));
        assert_eq!(
            not_found, "memory record 00000000-0000-0000-0000-000000000000 not found",
            "MemoryError::RecordNotFound Display drifted (typo or dropped qualifier regression class)"
        );

        let parent_mismatch = format!(
            "{}",
            MemoryError::ParentMismatch {
                id: Uuid::nil(),
                expected: Some(Uuid::from_u128(1)),
                actual: Some(Uuid::from_u128(2)),
            }
        );
        assert!(
            parent_mismatch.contains("parent mismatch for memory 00000000-0000-0000-0000-000000000000"),
            "MemoryError::ParentMismatch id slot drifted (typo regression class): {parent_mismatch}"
        );
        assert!(
            parent_mismatch.contains("expected Some(") && parent_mismatch.contains("00000000-0000-0000-0000-000000000001"),
            "MemoryError::ParentMismatch expected slot drifted (slot-swap or debug-vs-display regression class): {parent_mismatch}"
        );
        assert!(
            parent_mismatch.contains("actual Some(") && parent_mismatch.contains("00000000-0000-0000-0000-000000000002"),
            "MemoryError::ParentMismatch actual slot drifted (slot-swap or debug-vs-display regression class): {parent_mismatch}"
        );
        assert!(
            !parent_mismatch.contains("expected Some(00000000-0000-0000-0000-000000000002"),
            "MemoryError::ParentMismatch expected slot bound to actual value (slot-swap regression class): {parent_mismatch}"
        );
        assert!(
            !parent_mismatch.contains("actual Some(00000000-0000-0000-0000-000000000001"),
            "MemoryError::ParentMismatch actual slot bound to expected value (slot-swap regression class): {parent_mismatch}"
        );

        let invalid_repair = format!("{}", MemoryError::InvalidRepair("reason empty".into()));
        assert_eq!(
            invalid_repair, "invalid memory repair request: reason empty",
            "MemoryError::InvalidRepair Display drifted (typo or dropped 'memory' qualifier regression class — \
             would collide with A2AError::InvalidRepair)"
        );

        let invalid_compaction =
            format!("{}", MemoryError::InvalidCompaction("window empty".into()));
        assert_eq!(
            invalid_compaction, "invalid memory compaction request: window empty",
            "MemoryError::InvalidCompaction Display drifted (typo or prefix-convergence regression class)"
        );
        assert_ne!(
            invalid_compaction, invalid_repair,
            "MemoryError::InvalidCompaction must not converge with MemoryError::InvalidRepair \
             (prefix-convergence regression class would merge memory-compaction with memory-repair rejection paths)"
        );
    }

    #[test]
    fn memory_error_io_and_sqlite_and_serde_display_messages_pin_prefix_and_external_source_display_delegation(
    ) {
        let io_err = MemoryError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "memory.sqlite missing",
        ));
        let io_message = format!("{io_err}");
        assert!(
            io_message.starts_with("io: "),
            "MemoryError::Io must surface the literal 'io: ' bootstrap-stage prefix so audit-log filters can distinguish memory-store disk faults from SQLite engine faults and JSON parse faults during read/write/repair/compaction (dropped-prefix regression class): {io_message}"
        );
        assert!(
            io_message.contains("memory.sqlite missing"),
            "MemoryError::Io must surface the inner std::io::Error Display rendering after the colon ({{0}}, not {{0:?}}); a Debug refactor would render 'Custom {{ kind: NotFound, error: ... }}' instead of the message payload (Debug-vs-Display formatting regression class on the {{0}} interpolation): {io_message}"
        );
        assert!(
            !io_message.contains("Custom {") && !io_message.contains("Os {"),
            "MemoryError::Io must NOT surface the std::io::Error Debug rendering; a Debug refactor on {{0}} would expose internal struct fields like 'Custom {{ kind: ..., error: ... }}' or 'Os {{ code: ..., kind: ..., message: ... }}' (Debug-vs-Display formatting regression class on the {{0}} interpolation): {io_message}"
        );

        let sqlite_source = rusqlite::Connection::open_in_memory()
            .expect("open in-memory")
            .execute("INSERT INTO nonexistent_table VALUES(1)", [])
            .expect_err("sqlite must fail with no such table");
        let sqlite_err = MemoryError::Sqlite(sqlite_source);
        let sqlite_message = format!("{sqlite_err}");
        assert!(
            sqlite_message.starts_with("sqlite: "),
            "MemoryError::Sqlite must surface the literal 'sqlite: ' bootstrap-stage prefix so audit-log filters can distinguish SQLite engine faults from disk faults and JSON parse faults during read/write/repair/compaction (dropped-prefix regression class): {sqlite_message}"
        );
        assert!(
            sqlite_message.contains("no such table"),
            "MemoryError::Sqlite must surface the inner rusqlite::Error Display rendering after the colon ({{0}}, not {{0:?}}); rusqlite v0.31 Display renders missing-table failures as 'no such table: ...', a Debug refactor on {{0}} would render 'SqliteFailure(Error {{ code: ..., extended_code: ... }}, Some(\"...\"))' instead (Debug-vs-Display formatting regression class on the {{0}} interpolation): {sqlite_message}"
        );
        assert!(
            !sqlite_message.contains("SqliteFailure(Error"),
            "MemoryError::Sqlite must NOT surface the rusqlite::Error Debug rendering; a Debug refactor on {{0}} would expose 'SqliteFailure(Error {{ code: ..., extended_code: ... }}, Some(\"...\"))' internal struct fields and leak extended_code internals (Debug-vs-Display formatting regression class on the {{0}} interpolation): {sqlite_message}"
        );

        let serde_source =
            serde_json::from_str::<serde_json::Value>("not json").expect_err("parse must fail");
        let serde_err = MemoryError::Serde(serde_source);
        let serde_message = format!("{serde_err}");
        assert!(
            serde_message.starts_with("serde: "),
            "MemoryError::Serde must surface the literal 'serde: ' bootstrap-stage prefix so audit-log filters can distinguish JSON parse faults from disk faults and SQLite engine faults during memory-record marshalling (dropped-prefix regression class): {serde_message}"
        );
        assert!(
            serde_message.contains("expected"),
            "MemoryError::Serde must surface the inner serde_json::Error Display rendering after the colon (serde_json renders parse failures with 'expected ...' Display strings); a Debug refactor on {{0}} would render 'Error(\"...\", line: N, column: M)' instead (Debug-vs-Display formatting regression class on the {{0}} interpolation): {serde_message}"
        );
        assert!(
            !serde_message.contains("Error("),
            "MemoryError::Serde must NOT surface the serde_json::Error Debug rendering; a Debug refactor on {{0}} would expose 'Error(\"...\", line: N, column: M)' buffer-position structs (Debug-vs-Display formatting regression class on the {{0}} interpolation): {serde_message}"
        );

        assert_ne!(
            io_message, sqlite_message,
            "MemoryError::Io and MemoryError::Sqlite Display must not converge (prefix-convergence regression class): io={io_message} sqlite={sqlite_message}"
        );
        assert_ne!(
            io_message, serde_message,
            "MemoryError::Io and MemoryError::Serde Display must not converge (prefix-convergence regression class): io={io_message} serde={serde_message}"
        );
        assert_ne!(
            sqlite_message, serde_message,
            "MemoryError::Sqlite and MemoryError::Serde Display must not converge (prefix-convergence regression class): sqlite={sqlite_message} serde={serde_message}"
        );

        assert!(
            !io_message.starts_with("sqlite:") && !io_message.starts_with("serde:"),
            "MemoryError::Io must not start with 'sqlite:' or 'serde:'; a sibling-prefix swap would silently mis-route incident triage (sibling-prefix-swap regression class): {io_message}"
        );
        assert!(
            !sqlite_message.starts_with("io:") && !sqlite_message.starts_with("serde:"),
            "MemoryError::Sqlite must not start with 'io:' or 'serde:'; a sibling-prefix swap would silently mis-route incident triage (sibling-prefix-swap regression class): {sqlite_message}"
        );
        assert!(
            !serde_message.starts_with("io:") && !serde_message.starts_with("sqlite:"),
            "MemoryError::Serde must not start with 'io:' or 'sqlite:'; a sibling-prefix swap would silently mis-route incident triage (sibling-prefix-swap regression class): {serde_message}"
        );

        let string_variant_prefixes = [
            "worker:",
            "memory record ",
            "parent mismatch for memory",
            "invalid memory repair request:",
            "invalid memory compaction request:",
        ];
        for prefix in string_variant_prefixes {
            assert!(
                !io_message.starts_with(prefix),
                "MemoryError::Io must not converge with the string-variant surface '{prefix}' pinned by memory_error_display_messages_pin_five_string_variant_format_strings; a disk fault must not be mis-routed as a structured store-invariant violation (string-surface-convergence regression class): {io_message}"
            );
            assert!(
                !sqlite_message.starts_with(prefix),
                "MemoryError::Sqlite must not converge with the string-variant surface '{prefix}' pinned by memory_error_display_messages_pin_five_string_variant_format_strings; a SQLite engine fault must not be mis-routed as a structured store-invariant violation (string-surface-convergence regression class): {sqlite_message}"
            );
            assert!(
                !serde_message.starts_with(prefix),
                "MemoryError::Serde must not converge with the string-variant surface '{prefix}' pinned by memory_error_display_messages_pin_five_string_variant_format_strings; a JSON parse fault must not be mis-routed as a structured store-invariant violation (string-surface-convergence regression class): {serde_message}"
            );
        }
    }

    #[test]
    fn memory_error_sqlite_source_delegation_pin_returns_inner_rusqlite_error_via_std_error_source()
    {
        use std::error::Error;

        let inner = rusqlite::Connection::open_in_memory()
            .expect("open in-memory")
            .execute("INSERT INTO nonexistent_table VALUES(1)", [])
            .expect_err("sqlite must fail with no such table");
        let expected_display = format!("{inner}");
        let err = MemoryError::Sqlite(inner);
        let source = err.source().expect(
            "MemoryError::Sqlite must surface the inner rusqlite::Error via std::error::Error::source so daemon-side memory repair/compaction retry-policy classifiers can walk the error chain and downcast source() to rusqlite::Error to extract rusqlite::ErrorCode for distinct retry decisions (SQLITE_BUSY/LOCKED retry with backoff, SQLITE_CORRUPT escalates to operator-attention); a refactor that converted the variant from #[from] to a hand-written Error impl returning None (under a 'simpler error wrapping' rationale) would silently change source() to return None while leaving Display intact (dropped-source-attribute regression class)",
        );
        assert_eq!(
            format!("{source}"),
            expected_display,
            "MemoryError::Sqlite source() Display must match a direct format!() of the same rusqlite::Error verbatim; a refactor that swapped the inner field type to Box<dyn Error + Send + Sync> or any other wrapper would silently break daemon-side downcasts even though the wrapper's Display would continue to flow through {{0}} (concrete-source-type regression class)"
        );
        assert!(
            source.downcast_ref::<rusqlite::Error>().is_some(),
            "MemoryError::Sqlite source() must downcast_ref to rusqlite::Error so daemon-side memory retry-policy classifiers can extract rusqlite::ErrorCode for retry decisions; a refactor that wrapped the inner in a project-local newtype (e.g., MemorySqliteError(rusqlite::Error) under a 'distinguish memory-store SQLite failures from sibling SQL surfaces' rationale) would silently break downcast_ref::<rusqlite::Error>() at every downstream callsite (concrete-source-type downcast regression class)"
        );
    }

    #[test]
    fn memory_error_io_source_delegation_pin_returns_inner_std_io_error_via_std_error_source() {
        use std::error::Error;

        let inner = std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "memory.sqlite: permission denied",
        );
        let expected_display = format!("{inner}");
        let err = MemoryError::Io(inner);
        let source = err.source().expect(
            "covenant_memory::MemoryError::Io must surface the inner std::io::Error via std::error::Error::source so daemon-side memory retry-policy classifiers can walk the error chain and downcast source() to std::io::Error to extract io::ErrorKind for distinct retry decisions on memory-store IO (Interrupted retries immediately, WouldBlock backs off briefly, PermissionDenied escalates as security-sensitive on a non-readable memory store, NoSpace blocks new write/repair/compaction); a refactor that converted the variant from #[from] to a hand-written Error impl returning None (under a 'simpler error wrapping' rationale) would silently change source() to return None while leaving Display intact (dropped-source-attribute regression class)",
        );
        assert_eq!(
            format!("{source}"),
            expected_display,
            "covenant_memory::MemoryError::Io source() Display must match a direct format!() of the same std::io::Error verbatim; a refactor that swapped the inner field type to Box<dyn Error + Send + Sync> or any other wrapper would silently break daemon-side downcasts even though the wrapper's Display would continue to flow through {{0}} (concrete-source-type regression class)"
        );
        let kind = source.downcast_ref::<std::io::Error>().map(|e| e.kind());
        assert_eq!(
            kind,
            Some(std::io::ErrorKind::PermissionDenied),
            "covenant_memory::MemoryError::Io source() must downcast_ref to std::io::Error so daemon-side memory retry-policy classifiers can extract io::ErrorKind for retry decisions on memory-store IO; a refactor that wrapped the inner in a project-local newtype (e.g., MemoryIoError(std::io::Error) under a 'tag memory-store IO failures distinctly from sibling Io variants in other crates' rationale) would silently break downcast_ref::<std::io::Error>() at every downstream callsite that classifies memory-store IO faults (concrete-source-type downcast regression class)"
        );
    }

    #[test]
    fn memory_error_serde_source_delegation_pin_returns_inner_serde_json_error_via_std_error_source(
    ) {
        use std::error::Error;

        let inner =
            serde_json::from_str::<serde_json::Value>("not json").expect_err("parse must fail");
        let expected_display = format!("{inner}");
        let err = MemoryError::Serde(inner);
        let source = err.source().expect(
            "covenant_memory::MemoryError::Serde must surface the inner serde_json::Error via std::error::Error::source so daemon-side memory diagnostics can walk the error chain and downcast source() to serde_json::Error to inspect line/column or classify() for malformed-row identification (line/column points the operator at the offending payload buffer offset, classify() distinguishes Syntax-vs-Data-vs-EOF for incident triage on a corrupted JSON-bearing memory record); a refactor that converted the variant from #[from] to a hand-written Error impl returning None (under a 'simpler error wrapping' rationale) would silently change source() to return None while leaving Display intact (dropped-source-attribute regression class)",
        );
        assert_eq!(
            format!("{source}"),
            expected_display,
            "covenant_memory::MemoryError::Serde source() Display must match a direct format!() of the same serde_json::Error verbatim; a refactor that swapped the inner field type to Box<dyn Error + Send + Sync> or any other wrapper would silently break daemon-side downcasts even though the wrapper's Display would continue to flow through {{0}} (concrete-source-type regression class)"
        );
        assert!(
            source.downcast_ref::<serde_json::Error>().is_some(),
            "covenant_memory::MemoryError::Serde source() must downcast_ref to serde_json::Error so daemon-side memory diagnostics can call serde_json::Error::line/column/classify for malformed-row identification; a refactor that wrapped the inner in a project-local newtype (e.g., MemorySerdeError(serde_json::Error) under a 'consolidate parse errors into one Wire variant' rationale) would silently break downcast_ref::<serde_json::Error>() at every downstream callsite that classifies memory-store JSON parse faults (concrete-source-type downcast regression class)"
        );
    }

    async fn store_with_records(records: &[MemoryRecord]) -> SqliteStore {
        let s = SqliteStore::open_in_memory().expect("open_in_memory");
        for r in records {
            s.put(r.clone()).await.expect("put");
        }
        s
    }

    #[tokio::test]
    async fn backfill_receipt_correlation_apply_writes_receipt_id_into_metadata() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let receipt_a = Uuid::new_v4();
        let receipt_b = Uuid::new_v4();
        let s = store_with_records(&[
            record(id_a, MemoryTier::Working, "a", 1),
            record(id_b, MemoryTier::Episodic, "b", 2),
        ])
        .await;

        let outcome = s
            .backfill_receipt_correlation(
                false,
                vec![
                    MemoryReceiptBackfillCorrelation {
                        memory_record_id: id_a,
                        receipt_id: receipt_a,
                    },
                    MemoryReceiptBackfillCorrelation {
                        memory_record_id: id_b,
                        receipt_id: receipt_b,
                    },
                ],
            )
            .await
            .expect("apply succeeds");

        assert_eq!(
            outcome,
            BackfillReceiptCorrelationOutcome {
                row_count: 2,
                savepoint_name: MEMORY_BACKFILL_SAVEPOINT_NAME.into(),
                dry_run: false,
            },
            "apply outcome must report the exact row_count rewritten plus the \
             stable savepoint identifier audit/surface sub-slices will pin on; \
             a refactor that returned the input length instead of the \
             actually-changed count would let an idempotent-second-call \
             produce a phantom audit row of mutations that did not happen"
        );

        let got_a = s.get(id_a).await.unwrap().unwrap();
        assert_eq!(
            got_a.metadata["receipt_id"].as_str(),
            Some(receipt_a.to_string().as_str()),
            "apply must write the receipt_id into the metadata.receipt_id \
             string field verbatim — the planner emits Uuid::to_string() and \
             a downstream reconciler comparing this field to the settlement \
             receipt's id MUST see the same canonical-hyphenated form"
        );
        let got_b = s.get(id_b).await.unwrap().unwrap();
        assert_eq!(
            got_b.metadata["receipt_id"].as_str(),
            Some(receipt_b.to_string().as_str())
        );
    }

    #[tokio::test]
    async fn backfill_receipt_correlation_dry_run_reports_count_without_writes() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let s = store_with_records(&[
            record(id_a, MemoryTier::Working, "a", 1),
            record(id_b, MemoryTier::Working, "b", 2),
        ])
        .await;

        let outcome = s
            .backfill_receipt_correlation(
                true,
                vec![
                    MemoryReceiptBackfillCorrelation {
                        memory_record_id: id_a,
                        receipt_id: Uuid::new_v4(),
                    },
                    MemoryReceiptBackfillCorrelation {
                        memory_record_id: id_b,
                        receipt_id: Uuid::new_v4(),
                    },
                ],
            )
            .await
            .expect("dry-run succeeds");

        assert_eq!(
            outcome,
            BackfillReceiptCorrelationOutcome {
                row_count: 2,
                savepoint_name: MEMORY_BACKFILL_SAVEPOINT_NAME.into(),
                dry_run: true,
            },
            "dry-run must report the apply-path row_count plus dry_run=true so \
             the planner-equivalent surface can advertise the exact mutation \
             count without writing; a refactor that returned row_count=0 on \
             dry-run would silently hide the planned mutation size from the \
             operator's pre-apply review"
        );

        let got_a = s.get(id_a).await.unwrap().unwrap();
        assert!(
            got_a.metadata.get("receipt_id").is_none(),
            "dry-run must NOT write metadata.receipt_id — a refactor that \
             shared the apply UPDATE path under a 'dry_run flag toggles \
             commit only' rationale would silently mutate the store on every \
             dry-run preview, defeating the planner-equivalence guarantee"
        );
        let got_b = s.get(id_b).await.unwrap().unwrap();
        assert!(got_b.metadata.get("receipt_id").is_none());
    }

    #[tokio::test]
    async fn backfill_receipt_correlation_rolls_back_when_one_correlation_targets_missing_record() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let missing = Uuid::new_v4();
        let receipt_a = Uuid::new_v4();
        let receipt_b = Uuid::new_v4();
        let receipt_missing = Uuid::new_v4();
        let s = store_with_records(&[
            record(id_a, MemoryTier::Working, "a", 1),
            record(id_b, MemoryTier::Working, "b", 2),
        ])
        .await;

        let err = s
            .backfill_receipt_correlation(
                false,
                vec![
                    MemoryReceiptBackfillCorrelation {
                        memory_record_id: id_a,
                        receipt_id: receipt_a,
                    },
                    MemoryReceiptBackfillCorrelation {
                        memory_record_id: id_b,
                        receipt_id: receipt_b,
                    },
                    MemoryReceiptBackfillCorrelation {
                        memory_record_id: missing,
                        receipt_id: receipt_missing,
                    },
                ],
            )
            .await
            .expect_err("missing record must surface as an error");

        match err {
            MemoryError::RecordNotFound(id) => assert_eq!(
                id, missing,
                "RecordNotFound must name the exact missing memory_record_id \
                 so the caller can attribute the rejection to a specific \
                 planner row; a refactor that returned RecordNotFound(Uuid::nil()) \
                 would silently strip the diagnostic the operator needs to \
                 refuse the bad batch"
            ),
            other => panic!("expected MemoryError::RecordNotFound, got {other:?}"),
        }

        let got_a = s.get(id_a).await.unwrap().unwrap();
        assert!(
            got_a.metadata.get("receipt_id").is_none(),
            "the SAVEPOINT-wrapped batch must roll back EVERY prior UPDATE \
             when a later row fails — the first two correlations were \
             applied inside the savepoint before the missing-id failure, so \
             a refactor that escalated each per-row UPDATE to its own \
             autocommit transaction would leave id_a half-mutated, breaking \
             the all-or-nothing contract the umbrella task pins"
        );
        let got_b = s.get(id_b).await.unwrap().unwrap();
        assert!(
            got_b.metadata.get("receipt_id").is_none(),
            "second pre-failure row must also be unchanged after rollback — \
             a refactor that only rolled back the failing row would leave \
             id_b mutated and produce a half-applied batch"
        );
    }

    #[tokio::test]
    async fn backfill_receipt_correlation_is_idempotent_when_receipt_id_already_matches() {
        let id_a = Uuid::new_v4();
        let receipt_a = Uuid::new_v4();
        let mut existing = record(id_a, MemoryTier::Working, "a", 1);
        existing.metadata = serde_json::json!({
            "receipt_id": receipt_a.to_string(),
            "provenance": {"source": "operator"}
        });
        let s = store_with_records(&[existing]).await;

        let outcome = s
            .backfill_receipt_correlation(
                false,
                vec![MemoryReceiptBackfillCorrelation {
                    memory_record_id: id_a,
                    receipt_id: receipt_a,
                }],
            )
            .await
            .expect("apply succeeds");

        assert_eq!(
            outcome.row_count, 0,
            "a correlation that would not change the stored metadata must \
             NOT increment row_count — the audit-row count must reflect \
             actually-mutated rows so repeated invocations don't inflate \
             the SettlementReceiptBackfill-equivalent audit claim"
        );

        let got = s.get(id_a).await.unwrap().unwrap();
        assert_eq!(
            got.metadata["receipt_id"].as_str(),
            Some(receipt_a.to_string().as_str())
        );
        assert_eq!(
            got.metadata["provenance"]["source"].as_str(),
            Some("operator"),
            "pre-existing sibling metadata keys (provenance, stale_context, \
             ...) MUST survive the no-op apply — a refactor that always \
             rewrote the metadata field on every correlation would silently \
             clobber every other planner's prior backfill output"
        );
    }

    #[tokio::test]
    async fn backfill_receipt_correlation_wraps_non_object_metadata_under_previous_metadata() {
        let id_a = Uuid::new_v4();
        let receipt_a = Uuid::new_v4();
        let mut existing = record(id_a, MemoryTier::Working, "a", 1);
        existing.metadata = serde_json::json!("legacy-string-metadata");
        let s = store_with_records(&[existing]).await;

        let outcome = s
            .backfill_receipt_correlation(
                false,
                vec![MemoryReceiptBackfillCorrelation {
                    memory_record_id: id_a,
                    receipt_id: receipt_a,
                }],
            )
            .await
            .expect("apply succeeds");

        assert_eq!(outcome.row_count, 1);

        let got = s.get(id_a).await.unwrap().unwrap();
        assert_eq!(
            got.metadata["receipt_id"].as_str(),
            Some(receipt_a.to_string().as_str())
        );
        assert_eq!(
            got.metadata["previous_metadata"].as_str(),
            Some("legacy-string-metadata"),
            "non-object pre-existing metadata MUST be wrapped under the \
             previous_metadata key so a downstream audit operator can still \
             retrieve the v0 value; this mirrors MemoryRepairCommand::BackfillProvenance \
             behavior on non-object metadata — a refactor that dropped the \
             non-object branch under the rationale that 'metadata is always \
             an object by convention' would silently destroy legacy memory \
             records on the first backfill apply against a pre-convention \
             store"
        );
    }

    #[test]
    fn memory_receipt_backfill_plan_json_pairs_legacy_receipts_by_owner() {
        let owner = AgentId::new("owner@local", [4u8; 32]);
        let memory_id = uuid::Uuid::from_u128(10);
        let memory = MemoryRecord {
            id: memory_id,
            tier: MemoryTier::Working,
            owner: owner.clone(),
            text: "legacy memory".into(),
            embedding: Vec::new(),
            metadata: serde_json::json!({}),
            created_at: 1,
            parent: None,
        };
        let receipt_id = uuid::Uuid::from_u128(20);
        let receipt = SettlementReceipt {
            id: receipt_id,
            payer: owner.clone(),
            resource: ResourceKind::Memory,
            memory_record_id: None,
            credits_consumed: 3,
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

        let value = memory_receipt_backfill_plan_json(100, &[memory], &[receipt]);
        assert_eq!(value["kind"], "memory_receipt_backfill_plan");
        assert_eq!(value["mode"], "dry_run");
        assert_eq!(value["mutation_supported"], false);
        assert_eq!(value["records"].as_array().map(Vec::len), Some(1));
        assert_eq!(value["records"][0]["receipt_id"], receipt_id.to_string());
        assert_eq!(
            value["records"][0]["memory_record_id"],
            memory_id.to_string()
        );
        assert_eq!(value["records"][0]["status"], "candidate");
        assert_eq!(
            value["unmatched_legacy_receipts"].as_array().map(Vec::len),
            Some(0)
        );
        assert_eq!(
            value["unmatched_memory_records"].as_array().map(Vec::len),
            Some(0)
        );
        assert_eq!(value["refusal"]["apply_supported"], false);
    }

    #[test]
    fn memory_receipt_backfill_plan_json_binds_two_same_payer_receipts_to_distinct_records() {
        // match_legacy_receipts_to_memory_records makes the greedy
        // assignment injective within a run: each receipt binds to the
        // first eligible record in slice order, which is then inserted
        // into used_memory so a later same-payer receipt skips it
        //
        //   let candidate = memories.iter().find(|memory| {
        //       memory.owner.pubkey == receipt.payer.pubkey
        //           && !correlated.contains(&memory.id)
        //           && !used_memory.contains(&memory.id)   // <- the guard
        //   });
        //   if let Some(memory) = candidate { used_memory.insert(memory.id); ... }
        //
        // The pairs_legacy_receipts_by_owner test uses one receipt + one
        // record and never reaches a second same-payer receipt, so the
        // used_memory guard is unexercised. Drop it and both receipts'
        // .find() would resolve to the same first record — double-binding
        // two payments onto one memory_record_id and stranding the second
        // record as unmatched. (This is the within-run guard; the cross-run
        // `correlated` set is covered separately.)
        let owner = AgentId::new("owner@local", [4u8; 32]);
        let mk_memory = |n: u128| MemoryRecord {
            id: uuid::Uuid::from_u128(n),
            tier: MemoryTier::Working,
            owner: owner.clone(),
            text: "legacy memory".into(),
            embedding: Vec::new(),
            metadata: serde_json::json!({}),
            created_at: 1,
            parent: None,
        };
        let mk_receipt = |n: u128| SettlementReceipt {
            id: uuid::Uuid::from_u128(n),
            payer: owner.clone(),
            resource: ResourceKind::Memory,
            memory_record_id: None,
            credits_consumed: 3,
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

        // Input order fixes the deterministic outcome: the first receipt
        // takes the first record; the second receipt skips it (now used)
        // and takes the second.
        let value = memory_receipt_backfill_plan_json(
            100,
            &[mk_memory(10), mk_memory(11)],
            &[mk_receipt(20), mk_receipt(21)],
        );

        let records = value["records"]
            .as_array()
            .expect("records must be an array");
        assert_eq!(
            records.len(),
            2,
            "both same-payer receipts must match — each to its own record",
        );
        assert_eq!(
            records[0]["receipt_id"],
            uuid::Uuid::from_u128(20).to_string()
        );
        assert_eq!(
            records[0]["memory_record_id"],
            uuid::Uuid::from_u128(10).to_string(),
            "first receipt (slice order) binds the first eligible record",
        );
        assert_eq!(
            records[1]["receipt_id"],
            uuid::Uuid::from_u128(21).to_string()
        );
        assert_eq!(
            records[1]["memory_record_id"],
            uuid::Uuid::from_u128(11).to_string(),
            "second receipt skips the used first record and binds the second",
        );
        assert_ne!(
            records[0]["memory_record_id"], records[1]["memory_record_id"],
            "the two receipts must bind to DISTINCT memory records — pins the \
             used_memory anti-double-bind guard; dropping it binds both to \
             the first record",
        );
        assert_eq!(
            value["unmatched_legacy_receipts"].as_array().map(Vec::len),
            Some(0),
            "neither receipt may be left unmatched when two eligible records exist",
        );
        assert_eq!(
            value["unmatched_memory_records"].as_array().map(Vec::len),
            Some(0),
            "no record may be stranded — a double-bind would leave the second here",
        );
    }

    #[test]
    fn memory_receipt_backfill_plan_json_lists_unmatched_rows() {
        let memory_owner = AgentId::new("memory@local", [5u8; 32]);
        let payer = AgentId::new("payer@local", [6u8; 32]);
        let memory = MemoryRecord {
            id: uuid::Uuid::from_u128(30),
            tier: MemoryTier::LongTerm,
            owner: memory_owner,
            text: "unmatched memory".into(),
            embedding: Vec::new(),
            metadata: serde_json::json!({}),
            created_at: 1,
            parent: None,
        };
        let receipt = SettlementReceipt {
            id: uuid::Uuid::from_u128(40),
            payer,
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

        let value = memory_receipt_backfill_plan_json(10, &[memory], &[receipt]);
        assert_eq!(value["records"].as_array().map(Vec::len), Some(0));
        assert_eq!(
            value["unmatched_legacy_receipts"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(
            value["unmatched_memory_records"].as_array().map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn memory_receipt_backfill_plan_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &[
            "kind",
            "limit",
            "mode",
            "mutation_supported",
            "records",
            "refusal",
            "unmatched_legacy_receipts",
            "unmatched_memory_records",
        ];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("memory_receipt_backfill_plan_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "memory_receipt_backfill_plan_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("memory_receipt_backfill_plan"));
            assert!(value["mode"].is_string(), "mode must be a string: {value}");
            assert!(
                value["limit"].is_u64(),
                "limit must be a non-negative integer, not a string: {value}",
            );
            assert!(
                value["mutation_supported"].is_boolean(),
                "mutation_supported must be a JSON bool, not 0/1 or a string: {value}",
            );
            assert!(
                value["records"].is_array(),
                "records must be an array, not a string blob: {value}",
            );
            assert!(
                value["unmatched_legacy_receipts"].is_array(),
                "unmatched_legacy_receipts must be an array, not a string blob: {value}",
            );
            assert!(
                value["unmatched_memory_records"].is_array(),
                "unmatched_memory_records must be an array, not a string blob: {value}",
            );
            assert!(
                value["refusal"].is_object(),
                "refusal must be a structured object, not a string blob: {value}",
            );
        }

        let owner = AgentId::new("owner@local", [4u8; 32]);
        let memory = MemoryRecord {
            id: uuid::Uuid::from_u128(10),
            tier: MemoryTier::Working,
            owner: owner.clone(),
            text: "legacy memory".into(),
            embedding: Vec::new(),
            metadata: serde_json::json!({}),
            created_at: 1,
            parent: None,
        };
        let receipt = SettlementReceipt {
            id: uuid::Uuid::from_u128(20),
            payer: owner,
            resource: ResourceKind::Memory,
            memory_record_id: None,
            credits_consumed: 3,
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

        assert_shape(&memory_receipt_backfill_plan_json(
            100,
            &[memory],
            &[receipt],
        ));
        assert_shape(&memory_receipt_backfill_plan_json(0, &[], &[]));
    }

    #[test]
    fn memory_receipt_backfill_plan_json_pins_refusal_object_schema() {
        const EXPECTED_KEYS: &[&str] = &["apply_supported", "reason"];

        fn assert_refusal_shape(value: &serde_json::Value) {
            let refusal = value["refusal"]
                .as_object()
                .expect("memory_receipt_backfill_plan_json refusal field must be an object");
            let mut keys: Vec<String> = refusal.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "memory_receipt_backfill_plan_json refusal object keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(
                value["refusal"]["apply_supported"].is_boolean(),
                "refusal.apply_supported must be a JSON bool, not 0/1 or a string: {value}",
            );
            assert_eq!(
                value["refusal"]["apply_supported"].as_bool(),
                Some(false),
                "refusal.apply_supported must be false until receipt backfill mutation lands: {value}",
            );
            assert!(
                value["refusal"]["reason"].is_string(),
                "refusal.reason must be a string, not a structured object: {value}",
            );
        }

        let owner = AgentId::new("owner@local", [4u8; 32]);
        let memory = MemoryRecord {
            id: uuid::Uuid::from_u128(10),
            tier: MemoryTier::Working,
            owner: owner.clone(),
            text: "legacy memory".into(),
            embedding: Vec::new(),
            metadata: serde_json::json!({}),
            created_at: 1,
            parent: None,
        };
        let receipt = SettlementReceipt {
            id: uuid::Uuid::from_u128(20),
            payer: owner,
            resource: ResourceKind::Memory,
            memory_record_id: None,
            credits_consumed: 3,
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

        assert_refusal_shape(&memory_receipt_backfill_plan_json(
            100,
            &[memory],
            &[receipt],
        ));
        assert_refusal_shape(&memory_receipt_backfill_plan_json(0, &[], &[]));
    }

    #[test]
    fn memory_receipt_backfill_plan_json_pins_records_element_schema() {
        const EXPECTED_KEYS: &[&str] = &[
            "credits_consumed",
            "memory_owner_display",
            "memory_owner_pubkey",
            "memory_record_id",
            "payer_display",
            "payer_pubkey",
            "reason",
            "receipt_id",
            "status",
        ];

        fn assert_records_element_shape(value: &serde_json::Value) {
            let records = value["records"]
                .as_array()
                .expect("memory_receipt_backfill_plan_json records field must be an array");
            assert!(
                records.len() >= 2,
                "fixture must produce at least two records to pin the per-element schema across distinct payers: {value}",
            );
            for record in records {
                let object = record
                    .as_object()
                    .expect("each records[] element must be an object");
                let mut keys: Vec<String> = object.keys().cloned().collect();
                keys.sort();
                let expected: Vec<String> =
                    EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
                assert_eq!(
                    keys, expected,
                    "memory_receipt_backfill_plan_json records[] element keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
                );

                assert!(
                    record["receipt_id"].is_string(),
                    "records[].receipt_id must be a string uuid: {record}"
                );
                assert!(
                    record["memory_record_id"].is_string(),
                    "records[].memory_record_id must be a string uuid: {record}"
                );
                assert!(
                    record["payer_display"].is_string(),
                    "records[].payer_display must be a string: {record}"
                );
                assert!(
                    record["payer_pubkey"].is_string(),
                    "records[].payer_pubkey must be a base58 string: {record}"
                );
                assert!(
                    record["memory_owner_display"].is_string(),
                    "records[].memory_owner_display must be a string: {record}"
                );
                assert!(
                    record["memory_owner_pubkey"].is_string(),
                    "records[].memory_owner_pubkey must be a base58 string: {record}"
                );
                assert!(
                    record["credits_consumed"].is_u64(),
                    "records[].credits_consumed must be a non-negative integer, not a stringified number: {record}",
                );
                assert!(
                    record["status"].is_string(),
                    "records[].status must be a string slug: {record}"
                );
                assert!(
                    record["reason"].is_string(),
                    "records[].reason must be a string, not a structured object: {record}"
                );
            }
        }

        let owner_a = AgentId::new("owner-a@local", [10u8; 32]);
        let owner_b = AgentId::new("owner-b@local", [11u8; 32]);
        let memory_a = MemoryRecord {
            id: uuid::Uuid::from_u128(101),
            tier: MemoryTier::Working,
            owner: owner_a.clone(),
            text: "memory a".into(),
            embedding: Vec::new(),
            metadata: serde_json::json!({}),
            created_at: 1,
            parent: None,
        };
        let memory_b = MemoryRecord {
            id: uuid::Uuid::from_u128(102),
            tier: MemoryTier::LongTerm,
            owner: owner_b.clone(),
            text: "memory b".into(),
            embedding: Vec::new(),
            metadata: serde_json::json!({}),
            created_at: 1,
            parent: None,
        };
        let receipt_a = SettlementReceipt {
            id: uuid::Uuid::from_u128(201),
            payer: owner_a,
            resource: ResourceKind::Memory,
            memory_record_id: None,
            credits_consumed: 5,
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
        let receipt_b = SettlementReceipt {
            id: uuid::Uuid::from_u128(202),
            payer: owner_b,
            resource: ResourceKind::Memory,
            memory_record_id: None,
            credits_consumed: 7,
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

        assert_records_element_shape(&memory_receipt_backfill_plan_json(
            100,
            &[memory_a, memory_b],
            &[receipt_a, receipt_b],
        ));
    }

    #[test]
    fn memory_receipt_backfill_plan_json_pins_unmatched_legacy_receipts_element_schema() {
        const EXPECTED_KEYS: &[&str] = &[
            "credits_consumed",
            "payer_display",
            "payer_pubkey",
            "reason",
            "receipt_id",
        ];

        fn assert_unmatched_legacy_receipt_shape(value: &serde_json::Value) {
            let entries = value["unmatched_legacy_receipts"]
                .as_array()
                .expect("memory_receipt_backfill_plan_json unmatched_legacy_receipts field must be an array");
            assert!(
                entries.len() >= 2,
                "fixture must produce at least two unmatched_legacy_receipts to pin the per-element schema across distinct payers: {value}",
            );
            for entry in entries {
                let object = entry
                    .as_object()
                    .expect("each unmatched_legacy_receipts[] element must be an object");
                let mut keys: Vec<String> = object.keys().cloned().collect();
                keys.sort();
                let expected: Vec<String> =
                    EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
                assert_eq!(
                    keys, expected,
                    "memory_receipt_backfill_plan_json unmatched_legacy_receipts[] element keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
                );

                assert!(
                    entry["receipt_id"].is_string(),
                    "unmatched_legacy_receipts[].receipt_id must be a string uuid: {entry}"
                );
                assert!(
                    entry["payer_display"].is_string(),
                    "unmatched_legacy_receipts[].payer_display must be a string: {entry}"
                );
                assert!(
                    entry["payer_pubkey"].is_string(),
                    "unmatched_legacy_receipts[].payer_pubkey must be a base58 string: {entry}"
                );
                assert!(
                    entry["credits_consumed"].is_u64(),
                    "unmatched_legacy_receipts[].credits_consumed must be a non-negative integer, not a stringified number: {entry}",
                );
                assert!(entry["reason"].is_string(), "unmatched_legacy_receipts[].reason must be a string, not a structured object: {entry}");
            }
        }

        let payer_a = AgentId::new("payer-a@local", [20u8; 32]);
        let payer_b = AgentId::new("payer-b@local", [21u8; 32]);
        let receipt_a = SettlementReceipt {
            id: uuid::Uuid::from_u128(301),
            payer: payer_a,
            resource: ResourceKind::Memory,
            memory_record_id: None,
            credits_consumed: 11,
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
        let receipt_b = SettlementReceipt {
            id: uuid::Uuid::from_u128(302),
            payer: payer_b,
            resource: ResourceKind::Memory,
            memory_record_id: None,
            credits_consumed: 13,
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

        assert_unmatched_legacy_receipt_shape(&memory_receipt_backfill_plan_json(
            100,
            &[],
            &[receipt_a, receipt_b],
        ));
    }

    #[test]
    fn memory_receipt_backfill_plan_json_pins_unmatched_memory_records_element_schema() {
        const EXPECTED_KEYS: &[&str] = &[
            "memory_record_id",
            "owner_display",
            "owner_pubkey",
            "reason",
            "tier",
        ];

        fn assert_unmatched_memory_record_shape(value: &serde_json::Value) {
            let entries = value["unmatched_memory_records"].as_array().expect(
                "memory_receipt_backfill_plan_json unmatched_memory_records field must be an array",
            );
            assert!(
                entries.len() >= 2,
                "fixture must produce at least two unmatched_memory_records to pin the per-element schema across distinct owners: {value}",
            );
            for entry in entries {
                let object = entry
                    .as_object()
                    .expect("each unmatched_memory_records[] element must be an object");
                let mut keys: Vec<String> = object.keys().cloned().collect();
                keys.sort();
                let expected: Vec<String> =
                    EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
                assert_eq!(
                    keys, expected,
                    "memory_receipt_backfill_plan_json unmatched_memory_records[] element keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
                );

                assert!(
                    entry["memory_record_id"].is_string(),
                    "unmatched_memory_records[].memory_record_id must be a string uuid: {entry}"
                );
                assert!(
                    entry["owner_display"].is_string(),
                    "unmatched_memory_records[].owner_display must be a string: {entry}"
                );
                assert!(
                    entry["owner_pubkey"].is_string(),
                    "unmatched_memory_records[].owner_pubkey must be a base58 string: {entry}"
                );
                assert!(
                    entry["tier"].is_string(),
                    "unmatched_memory_records[].tier must be a documented tier slug string, not a structured object: {entry}",
                );
                assert!(entry["reason"].is_string(), "unmatched_memory_records[].reason must be a string, not a structured object: {entry}");
            }
        }

        let owner_a = AgentId::new("owner-a@local", [30u8; 32]);
        let owner_b = AgentId::new("owner-b@local", [31u8; 32]);
        let memory_a = MemoryRecord {
            id: uuid::Uuid::from_u128(401),
            tier: MemoryTier::Working,
            owner: owner_a,
            text: "memory a".into(),
            embedding: Vec::new(),
            metadata: serde_json::json!({}),
            created_at: 1,
            parent: None,
        };
        let memory_b = MemoryRecord {
            id: uuid::Uuid::from_u128(402),
            tier: MemoryTier::LongTerm,
            owner: owner_b,
            text: "memory b".into(),
            embedding: Vec::new(),
            metadata: serde_json::json!({}),
            created_at: 1,
            parent: None,
        };

        assert_unmatched_memory_record_shape(&memory_receipt_backfill_plan_json(
            100,
            &[memory_a, memory_b],
            &[],
        ));
    }

    #[test]
    fn memory_receipt_backfill_correlations_mirrors_planner_pairings() {
        // Pin parity between the JSON planner envelope and the typed
        // correlator: every record in records[] must produce one
        // MemoryReceiptBackfillCorrelation with the same id pair, and
        // unmatched rows must NOT show up as correlations. A regression
        // that filtered legacy receipts differently in one path than the
        // other would let a dry-run preview claim N changes while an
        // apply changed M ≠ N rows.
        let owner_a = AgentId::new("owner-a@local", [40u8; 32]);
        let owner_b = AgentId::new("owner-b@local", [41u8; 32]);
        let memory_a = MemoryRecord {
            id: uuid::Uuid::from_u128(501),
            tier: MemoryTier::Working,
            owner: owner_a.clone(),
            text: "memory a".into(),
            embedding: Vec::new(),
            metadata: serde_json::json!({}),
            created_at: 1,
            parent: None,
        };
        let memory_b_unmatched = MemoryRecord {
            id: uuid::Uuid::from_u128(502),
            tier: MemoryTier::LongTerm,
            owner: owner_b.clone(),
            text: "memory b (no receipt)".into(),
            embedding: Vec::new(),
            metadata: serde_json::json!({}),
            created_at: 1,
            parent: None,
        };
        let receipt_a = SettlementReceipt {
            id: uuid::Uuid::from_u128(601),
            payer: owner_a,
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
        let unmatched_receipt = SettlementReceipt {
            id: uuid::Uuid::from_u128(602),
            payer: AgentId::new("payer-c@local", [42u8; 32]),
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

        let plan = memory_receipt_backfill_plan_json(
            100,
            &[memory_a.clone(), memory_b_unmatched.clone()],
            &[receipt_a.clone(), unmatched_receipt.clone()],
        );
        let correlations = memory_receipt_backfill_correlations(
            &[memory_a.clone(), memory_b_unmatched],
            &[receipt_a.clone(), unmatched_receipt],
        );

        let plan_pairs: Vec<(String, String)> = plan["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["receipt_id"].as_str().unwrap().to_string(),
                    r["memory_record_id"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let correlation_pairs: Vec<(String, String)> = correlations
            .iter()
            .map(|c| (c.receipt_id.to_string(), c.memory_record_id.to_string()))
            .collect();
        assert_eq!(
            plan_pairs, correlation_pairs,
            "memory_receipt_backfill_correlations must yield exactly the (receipt_id, memory_record_id) pairs the JSON planner records[] list — any divergence means the planner's preview and the apply path will report different counts and rewrite different rows",
        );
        assert_eq!(correlations.len(), 1);
        assert_eq!(correlations[0].receipt_id, uuid::Uuid::from_u128(601));
        assert_eq!(correlations[0].memory_record_id, uuid::Uuid::from_u128(501));
    }

    #[test]
    fn memory_receipt_backfill_correlations_skips_already_correlated_rows() {
        let owner = AgentId::new("owner@local", [50u8; 32]);
        let memory_id = uuid::Uuid::from_u128(701);
        let memory = MemoryRecord {
            id: memory_id,
            tier: MemoryTier::Working,
            owner: owner.clone(),
            text: "already correlated".into(),
            embedding: Vec::new(),
            metadata: serde_json::json!({}),
            created_at: 1,
            parent: None,
        };
        let prior_receipt = SettlementReceipt {
            id: uuid::Uuid::from_u128(801),
            payer: owner.clone(),
            resource: ResourceKind::Memory,
            memory_record_id: Some(memory_id),
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
        let legacy_receipt = SettlementReceipt {
            id: uuid::Uuid::from_u128(802),
            payer: owner,
            resource: ResourceKind::Memory,
            memory_record_id: None,
            credits_consumed: 1,
            settled_at: 3,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        };

        let correlations =
            memory_receipt_backfill_correlations(&[memory], &[prior_receipt, legacy_receipt]);
        assert!(
            correlations.is_empty(),
            "a memory record already bound by a prior correlated receipt must not be re-paired by the legacy backfill — the second receipt has no other candidate so the apply path must leave both unchanged: got {correlations:?}",
        );
    }

    #[tokio::test]
    async fn sqlite_compact_apply_rolls_back_when_mid_apply_update_fails() {
        // SqliteStore::compact wraps every apply-mode mutation in a single
        // BEGIN IMMEDIATE + SAVEPOINT compact_apply, so a mid-apply error
        // (full disk, SQLITE_CORRUPT, a trigger ABORT, etc.) leaves the
        // store byte-identical to its pre-call state. The trait default
        // ran each delete/put through a separate spawn_blocking with no
        // shared transaction, so the same failure would leave the store
        // half-compacted: some rows deleted, some parent refs detached,
        // others not — exactly the audit-invisible inconsistency this
        // task closes.
        //
        // Simulating a real mid-apply error: install a SQLite BEFORE
        // UPDATE trigger on memories that ABORTs whenever the new
        // metadata contains "stale_context" (i.e., exactly the stale-
        // marking write the compact plan emits for the long-term row).
        // The compact apply path runs deletes first, then updates; the
        // trigger fires on the first update and forces a rollback that
        // must undo the prior deletes.
        let id_working = Uuid::new_v4();
        let id_episodic = Uuid::new_v4();
        let id_longterm = Uuid::new_v4();
        let s = store_with_records(&[
            record(id_working, MemoryTier::Working, "w", 1),
            record(id_episodic, MemoryTier::Episodic, "e", 2),
            record(id_longterm, MemoryTier::LongTerm, "l", 3),
        ])
        .await;

        // Install the failure trigger after the rows are seeded so put()
        // for the seed doesn't trip on it.
        {
            let g = s.conn.lock().expect("conn lock");
            g.execute_batch(
                "CREATE TRIGGER fail_compact_stale_mark
                 BEFORE UPDATE ON memories
                 WHEN NEW.metadata LIKE '%stale_context%'
                 BEGIN
                   SELECT RAISE(ABORT, 'simulated mid-apply failure');
                 END;",
            )
            .expect("install trigger");
        }

        let request = MemoryCompactionRequest {
            mode: MemoryRepairMode::Apply,
            reason: "rollback-test".into(),
            policy: MemoryCompactionPolicy {
                delete_working_before_ms: Some(10),
                delete_episodic_before_ms: Some(10),
                mark_longterm_stale_before_ms: Some(10),
                marked_at_ms: Some(10),
                detach_stale_parents: false,
            },
        };
        let err = s
            .compact(request)
            .await
            .expect_err("trigger ABORT must surface as a compact() error");
        match err {
            MemoryError::Sqlite(_) => {}
            other => panic!("expected MemoryError::Sqlite from the trigger ABORT, got {other:?}"),
        }

        // The savepoint must roll back EVERY prior delete — without the
        // wrap, id_working + id_episodic would be gone here and only
        // the long-term update would have failed. With the wrap, every
        // pre-call row is still present.
        let after = s.all().await.expect("all after rollback");
        assert_eq!(
            after.len(),
            3,
            "rollback must restore every pre-call row — a refactor that \
             escalated each delete or update to its own autocommit \
             transaction would leave 1 or 2 rows here (the deletes \
             that landed before the failing update)"
        );
        let mut ids: Vec<Uuid> = after.iter().map(|r| r.id).collect();
        ids.sort();
        let mut expected = vec![id_working, id_episodic, id_longterm];
        expected.sort();
        assert_eq!(
            ids, expected,
            "rolled-back state must contain exactly the original ids; any \
             drift implies the savepoint didn't bracket the full mutation \
             set"
        );
        // And the long-term row's metadata must NOT carry stale_context
        // — the update that triggered the failure must have been rolled
        // back too.
        let lt = after
            .iter()
            .find(|r| r.id == id_longterm)
            .expect("longterm row");
        assert!(
            lt.metadata.get("stale_context").is_none(),
            "long-term row's metadata must NOT carry stale_context after \
             rollback — pins that the failing UPDATE was undone, not \
             merely caught"
        );
    }

    #[tokio::test]
    async fn sqlite_compact_apply_persists_full_plan_on_success() {
        // Counterpart to the rollback test: the happy path must commit
        // every delete and every update, with the SAVEPOINT released and
        // the IMMEDIATE transaction COMMITted before compact returns.
        let id_working = Uuid::new_v4();
        let id_episodic = Uuid::new_v4();
        let id_longterm = Uuid::new_v4();
        let s = store_with_records(&[
            record(id_working, MemoryTier::Working, "w", 1),
            record(id_episodic, MemoryTier::Episodic, "e", 2),
            record(id_longterm, MemoryTier::LongTerm, "l", 3),
        ])
        .await;

        let request = MemoryCompactionRequest {
            mode: MemoryRepairMode::Apply,
            reason: "happy-path".into(),
            policy: MemoryCompactionPolicy {
                delete_working_before_ms: Some(10),
                delete_episodic_before_ms: Some(10),
                mark_longterm_stale_before_ms: Some(10),
                marked_at_ms: Some(10),
                detach_stale_parents: false,
            },
        };
        let outcome = s.compact(request).await.expect("compact must succeed");
        assert!(outcome.would_change);
        assert!(outcome.changed);
        assert_eq!(outcome.deleted.len(), 2);
        assert_eq!(outcome.stale_marked.len(), 1);

        let after = s.all().await.expect("all after compact");
        assert_eq!(after.len(), 1, "two rows must be deleted");
        let lt = after
            .iter()
            .find(|r| r.id == id_longterm)
            .expect("longterm row must survive");
        assert!(
            lt.metadata.get("stale_context").is_some(),
            "stale-mark UPDATE must persist on success — counterpart to \
             the rollback assertion"
        );
    }
}
