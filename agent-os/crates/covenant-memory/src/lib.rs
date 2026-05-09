//! Three-tier memory store for Covenant.
//!
//! [`MemoryRecord`] values live in one of three tiers — working,
//! episodic, or long-term — backed by SQLite for persistence with an
//! in-memory implementation suitable for tests. The trait covers
//! recent-record reads, embedded-vector cosine search, and tier-scoped
//! garbage collection.

#![deny(unsafe_code)]

pub mod ignore;
pub use ignore::{IgnorePattern, IgnoreSet, IgnoreVerdict};

use async_trait::async_trait;
use covenant_types::{MemoryRecord, MemoryTier};
pub use covenant_types::{
    MemoryRepairAction, MemoryRepairCommand, MemoryRepairMode, MemoryRepairOutcome,
    MemoryRepairRequest,
};
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
}

#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn put(&self, record: MemoryRecord) -> Result<(), MemoryError>;
    async fn get(&self, id: Uuid) -> Result<Option<MemoryRecord>, MemoryError>;
    async fn recent(
        &self,
        tier: Option<MemoryTier>,
        limit: usize,
    ) -> Result<Vec<MemoryRecord>, MemoryError>;
    async fn delete(&self, id: Uuid) -> Result<bool, MemoryError>;
    /// Score every record's `embedding` against `query_embedding` via cosine
    /// similarity and return the top `limit`, optionally filtered by tier.
    /// Records with empty embeddings get score 0 and are returned last (or
    /// dropped, depending on the impl). v0 does an in-process linear scan;
    /// LanceDB / sqlite-vec arrive later.
    async fn search_similar(
        &self,
        query_embedding: Vec<f32>,
        tier: Option<MemoryTier>,
        limit: usize,
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
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        let g = self
            .records
            .lock()
            .map_err(|e| MemoryError::Worker(e.to_string()))?;
        let mut scored: Vec<(f32, MemoryRecord)> = g
            .iter()
            .filter(|r| tier.is_none_or(|t| r.tier == t))
            .map(|r| (cosine(&query_embedding, &r.embedding), r.clone()))
            .filter(|(s, _)| *s > 0.0)
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
        let mut pubkey = [0u8; 32];
        if pubkey_vec.len() == 32 {
            pubkey.copy_from_slice(&pubkey_vec);
        }
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
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        let conn = self.conn.clone();
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
                if s > 0.0 {
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
            .search_similar(vec![1.0, 0.0, 0.0], None, 2)
            .await
            .unwrap();
        assert_eq!(hits[0].text, "alpha");
        assert_eq!(hits[1].text, "gamma");
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
            .search_similar(vec![1.0, 0.0, 0.0], Some(MemoryTier::Episodic), 5)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].text, "e-alpha");
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
        let n = s
            .purge_older_than(Some(MemoryTier::Working), 200)
            .await
            .unwrap();
        assert_eq!(n, 1);
        let recent = s.recent(Some(MemoryTier::Working), 10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].text, "newer");
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
}
