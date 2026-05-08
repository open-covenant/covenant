//! Settlement primitive for Covenant.
//!
//! Implements the credits-and-buyback settlement model. Receipts
//! accumulate in a local JSONL log
//! (`$COVENANT_HOME/receipts/working.jsonl` by convention) until they
//! are batched and flushed to the on-chain settlement program — at
//! which point [`SettlementReceipt::onchain_sig`] is populated. Two
//! storage backends implement [`Settlement`]:
//! [`JsonlReceiptStore`] for production and [`InMemorySettlement`]
//! for tests.

#![deny(unsafe_code)]

use async_trait::async_trait;
use covenant_types::SettlementReceipt;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum SettlementError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
}

#[async_trait]
pub trait Settlement: Send + Sync {
    async fn record(&self, receipt: SettlementReceipt) -> Result<(), SettlementError>;
    async fn recent(&self, limit: usize) -> Result<Vec<SettlementReceipt>, SettlementError>;
}

/// Append-only JSONL store. One receipt per line.
pub struct JsonlReceiptStore {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl JsonlReceiptStore {
    pub async fn open(path: PathBuf) -> Result<Self, SettlementError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        // Touch the file so `recent()` on a fresh deployment doesn't error.
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
impl Settlement for JsonlReceiptStore {
    async fn record(&self, receipt: SettlementReceipt) -> Result<(), SettlementError> {
        let _guard = self.lock.lock().await;
        let line = serde_json::to_string(&receipt)?;
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

    async fn recent(&self, limit: usize) -> Result<Vec<SettlementReceipt>, SettlementError> {
        let _guard = self.lock.lock().await;
        let f = match fs::File::open(&self.path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut reader = BufReader::new(f);
        let mut all: Vec<SettlementReceipt> = Vec::new();
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
}

/// In-memory test backend.
#[derive(Default)]
pub struct InMemorySettlement {
    records: Mutex<Vec<SettlementReceipt>>,
}

impl InMemorySettlement {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Settlement for InMemorySettlement {
    async fn record(&self, receipt: SettlementReceipt) -> Result<(), SettlementError> {
        self.records.lock().await.push(receipt);
        Ok(())
    }

    async fn recent(&self, limit: usize) -> Result<Vec<SettlementReceipt>, SettlementError> {
        let g = self.records.lock().await;
        let start = g.len().saturating_sub(limit);
        Ok(g[start..].to_vec())
    }
}

/// No-op fallback (settlement disabled).
pub struct NoopSettlement;

#[async_trait]
impl Settlement for NoopSettlement {
    async fn record(&self, _receipt: SettlementReceipt) -> Result<(), SettlementError> {
        Ok(())
    }

    async fn recent(&self, _limit: usize) -> Result<Vec<SettlementReceipt>, SettlementError> {
        Ok(Vec::new())
    }
}

/// Compute the credit cost of a memory write (Phase 1 placeholder; real
/// pricing arrives once the credit model is wired in Phase 5). The minimum
/// cost is 1 credit so even empty writes show up on the burn surface.
pub fn memory_write_credits(bytes: usize) -> u64 {
    ((bytes as u64).div_ceil(1024)).max(1)
}

/// Credit cost of one intent dispatch — the unit `BudgetLedger::try_debit`
/// charges in Sprint 58b's daemon wiring. Flat 1-credit-per-intent for v0
/// per the Plan-gate decision: the spec phrase "budget credits" connotes a
/// quota, not a meter; v0 is single-operator with no price-discrimination
/// pressure; and a flat cost gives `BudgetError::Exhausted::refill_eta_ms`
/// a deterministic value that Sprint 58c's pause-and-queue verb can size
/// the resume around. A future per-agent `cost_per_intent` manifest field
/// would land at this call site.
pub const INTENT_DISPATCH_CREDITS: u64 = 1;

/// Accessor mirror of [`INTENT_DISPATCH_CREDITS`]. Future variants that
/// price per agent or per tool-call can replace the body without touching
/// callers.
pub fn intent_dispatch_credits() -> u64 {
    INTENT_DISPATCH_CREDITS
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_types::{AgentId, ResourceKind};
    use uuid::Uuid;

    fn receipt(amount: u64) -> SettlementReceipt {
        SettlementReceipt {
            id: Uuid::new_v4(),
            payer: AgentId::new("user@local", [0u8; 32]),
            resource: ResourceKind::Memory,
            credits_consumed: amount,
            settled_at: amount,
            onchain_sig: None,
        }
    }

    #[tokio::test]
    async fn in_memory_record_and_recent() {
        let s = InMemorySettlement::new();
        s.record(receipt(1)).await.unwrap();
        s.record(receipt(2)).await.unwrap();
        s.record(receipt(3)).await.unwrap();
        let last_two = s.recent(2).await.unwrap();
        assert_eq!(last_two.len(), 2);
        assert_eq!(last_two[0].credits_consumed, 2);
        assert_eq!(last_two[1].credits_consumed, 3);
    }

    #[tokio::test]
    async fn jsonl_round_trip_through_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("receipts.jsonl");
        let s = JsonlReceiptStore::open(path.clone()).await.unwrap();
        s.record(receipt(10)).await.unwrap();
        s.record(receipt(20)).await.unwrap();

        // Reopen to verify on-disk persistence.
        let s2 = JsonlReceiptStore::open(path.clone()).await.unwrap();
        let all = s2.recent(10).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].credits_consumed, 10);
        assert_eq!(all[1].credits_consumed, 20);

        // Inspect the file by hand to make sure the format is JSONL.
        let raw = std::fs::read_to_string(&path).unwrap();
        let line_count = raw.lines().filter(|l| !l.is_empty()).count();
        assert_eq!(line_count, 2);
    }

    #[tokio::test]
    async fn jsonl_recent_on_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.jsonl");
        // Don't open, just construct a store-shaped struct that points there.
        // We open then delete to test the missing path during `recent`.
        let s = JsonlReceiptStore::open(path.clone()).await.unwrap();
        std::fs::remove_file(&path).unwrap();
        let r = s.recent(10).await.unwrap();
        assert!(r.is_empty());
    }

    #[tokio::test]
    async fn noop_swallows_records_and_returns_empty() {
        let s = NoopSettlement;
        s.record(receipt(7)).await.unwrap();
        assert!(s.recent(10).await.unwrap().is_empty());
    }

    #[test]
    fn memory_write_credits_minimum_one() {
        assert_eq!(memory_write_credits(0), 1);
        assert_eq!(memory_write_credits(1), 1);
        assert_eq!(memory_write_credits(1024), 1);
        assert_eq!(memory_write_credits(1025), 2);
        assert_eq!(memory_write_credits(2_500), 3);
    }
}
