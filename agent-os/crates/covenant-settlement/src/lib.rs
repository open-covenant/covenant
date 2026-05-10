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
use covenant_types::{ResourceKind, SettlementReceipt};
use sha2::{Digest, Sha256};
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
    #[error("no unsettled receipts")]
    EmptyBatch,
}

#[async_trait]
pub trait Settlement: Send + Sync {
    async fn record(&self, receipt: SettlementReceipt) -> Result<(), SettlementError>;
    async fn recent(&self, limit: usize) -> Result<Vec<SettlementReceipt>, SettlementError>;
    async fn mark_batch_confirmed(
        &self,
        receipt_ids: &[uuid::Uuid],
        confirmation: ChainConfirmation,
    ) -> Result<u64, SettlementError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainConfirmation {
    pub chain: String,
    pub cluster: String,
    pub batch_id: String,
    pub merkle_root: String,
    pub tx_sig: Option<String>,
    pub slot: Option<u64>,
    pub confirmed_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptBatch {
    pub batch_id: String,
    pub merkle_root: String,
    pub receipt_ids: Vec<uuid::Uuid>,
    pub receipt_count: u32,
}

pub fn build_receipt_batch(
    receipts: &[SettlementReceipt],
) -> Result<ReceiptBatch, SettlementError> {
    let unsettled: Vec<&SettlementReceipt> = receipts
        .iter()
        .filter(|receipt| receipt.batch_id.is_none())
        .collect();
    if unsettled.is_empty() {
        return Err(SettlementError::EmptyBatch);
    }

    let mut level: Vec<[u8; 32]> = unsettled
        .iter()
        .map(|receipt| receipt_hash(receipt))
        .collect();
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        for pair in level.chunks(2) {
            let right = pair.get(1).copied().unwrap_or(pair[0]);
            let mut hasher = Sha256::new();
            hasher.update(pair[0]);
            hasher.update(right);
            next.push(hasher.finalize().into());
        }
        level = next;
    }

    let merkle_root = hex32(level[0]);
    let batch_id = hex32(Sha256::digest(format!("covenant-receipts:{merkle_root}")).into());
    Ok(ReceiptBatch {
        batch_id,
        merkle_root,
        receipt_ids: unsettled.iter().map(|receipt| receipt.id).collect(),
        receipt_count: unsettled.len() as u32,
    })
}

pub fn receipt_migration_plan_json(receipts: &[SettlementReceipt]) -> serde_json::Value {
    let memory_receipts = receipts
        .iter()
        .filter(|receipt| receipt.resource == ResourceKind::Memory)
        .collect::<Vec<_>>();
    let legacy_memory_receipts = memory_receipts
        .iter()
        .copied()
        .filter(|receipt| receipt.memory_record_id.is_none())
        .collect::<Vec<_>>();
    let correlated_memory_receipts = memory_receipts
        .iter()
        .copied()
        .filter(|receipt| receipt.memory_record_id.is_some())
        .collect::<Vec<_>>();
    let batched_receipt_count = receipts
        .iter()
        .filter(|receipt| receipt.batch_id.is_some())
        .count();

    serde_json::json!({
        "schema": "covenant.settlement.receipt_migration.plan.v1",
        "mode": "dry_run",
        "mutation_supported": false,
        "summary": {
            "receipt_count": receipts.len(),
            "memory_receipt_count": memory_receipts.len(),
            "correlated_memory_receipt_count": correlated_memory_receipts.len(),
            "legacy_memory_receipt_count": legacy_memory_receipts.len(),
            "non_memory_receipt_count": receipts.len().saturating_sub(memory_receipts.len()),
            "batched_receipt_count": batched_receipt_count,
            "unbatched_receipt_count": receipts.len().saturating_sub(batched_receipt_count),
            "malformed_row_count": 0
        },
        "expected_correlation_inputs": [
            "memory_record_id from the originating memory write",
            "payer pubkey match between receipt.payer and memory.owner",
            "before and after receipt hash evidence for any future mutation",
            "audit event id for the future authorized mutation"
        ],
        "legacy_uncorrelated_receipts": legacy_memory_receipts
            .iter()
            .map(|receipt| serde_json::json!({
                "receipt_id": receipt.id,
                "payer_pubkey": receipt.payer.pubkey_base58(),
                "resource": receipt.resource,
                "credits_consumed": receipt.credits_consumed,
                "settled_at": receipt.settled_at,
                "batch_id": receipt.batch_id.as_deref(),
                "onchain_settled": receipt.tx_sig.is_some() || receipt.onchain_sig.is_some(),
                "status": "needs_memory_record_match"
            }))
            .collect::<Vec<_>>(),
        "correlated_memory_receipts": correlated_memory_receipts
            .iter()
            .map(|receipt| serde_json::json!({
                "receipt_id": receipt.id,
                "payer_pubkey": receipt.payer.pubkey_base58(),
                "memory_record_id": receipt.memory_record_id,
                "batch_id": receipt.batch_id.as_deref(),
                "status": "already_correlated"
            }))
            .collect::<Vec<_>>(),
        "refusal": {
            "apply_supported": false,
            "reason": "settlement receipt migration is read-only; mutation requires a separate authorized command with rollback and audit evidence"
        }
    })
}

fn receipt_hash(receipt: &SettlementReceipt) -> [u8; 32] {
    let mut payload = serde_json::json!({
        "id": receipt.id,
        "payer": receipt.payer.pubkey_base58(),
        "resource": receipt.resource,
        "credits_consumed": receipt.credits_consumed,
        "settled_at": receipt.settled_at,
    });
    if let Some(id) = receipt.memory_record_id {
        payload["memory_record_id"] = serde_json::json!(id);
    }

    Sha256::digest(serde_json::to_vec(&payload).expect("receipt hash payload serializes")).into()
}

fn hex32(bytes: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn annotate_receipt(receipt: &mut SettlementReceipt, confirmation: &ChainConfirmation) {
    receipt.chain = Some(confirmation.chain.clone());
    receipt.cluster = Some(confirmation.cluster.clone());
    receipt.batch_id = Some(confirmation.batch_id.clone());
    receipt.merkle_root = Some(confirmation.merkle_root.clone());
    receipt.tx_sig = confirmation.tx_sig.clone();
    receipt.slot = confirmation.slot;
    receipt.confirmed_at = confirmation.confirmed_at;
    receipt.onchain_sig = confirmation.tx_sig.clone();
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

    async fn mark_batch_confirmed(
        &self,
        receipt_ids: &[uuid::Uuid],
        confirmation: ChainConfirmation,
    ) -> Result<u64, SettlementError> {
        let _guard = self.lock.lock().await;
        let f = match fs::File::open(&self.path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        let mut reader = BufReader::new(f);
        let mut receipts = Vec::new();
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break;
            }
            let trimmed = line.trim_end();
            if !trimmed.is_empty() {
                receipts.push(serde_json::from_str::<SettlementReceipt>(trimmed)?);
            }
        }

        let mut updated = 0;
        for receipt in &mut receipts {
            if receipt_ids.contains(&receipt.id) {
                annotate_receipt(receipt, &confirmation);
                updated += 1;
            }
        }

        let mut body = String::new();
        for receipt in receipts {
            body.push_str(&serde_json::to_string(&receipt)?);
            body.push('\n');
        }
        fs::write(&self.path, body).await?;
        Ok(updated)
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

    async fn mark_batch_confirmed(
        &self,
        receipt_ids: &[uuid::Uuid],
        confirmation: ChainConfirmation,
    ) -> Result<u64, SettlementError> {
        let mut records = self.records.lock().await;
        let mut updated = 0;
        for receipt in &mut *records {
            if receipt_ids.contains(&receipt.id) {
                annotate_receipt(receipt, &confirmation);
                updated += 1;
            }
        }
        Ok(updated)
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

    async fn mark_batch_confirmed(
        &self,
        _receipt_ids: &[uuid::Uuid],
        _confirmation: ChainConfirmation,
    ) -> Result<u64, SettlementError> {
        Ok(0)
    }
}

/// Compute the credit cost of a memory write (Phase 1 placeholder; real
/// pricing arrives once the credit model is wired in Phase 5). The minimum
/// cost is 1 credit so even empty writes show up on the burn surface.
pub fn memory_write_credits(bytes: usize) -> u64 {
    ((bytes as u64).div_ceil(1024)).max(1)
}

/// Credit cost of one intent dispatch — the unit `BudgetLedger::try_debit`
/// charges in the daemon's pre-spawn budget gate. Flat 1-credit-per-intent
/// for v0: the spec phrase "budget credits" connotes a quota, not a meter;
/// v0 is single-operator with no price-discrimination pressure; and a flat
/// cost gives `BudgetError::Exhausted::refill_eta_ms` a deterministic value
/// that the pause-and-queue resume verb can size the wait around. A future
/// per-agent `cost_per_intent` manifest field would land at this call site.
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
            memory_record_id: None,
            credits_consumed: amount,
            settled_at: amount,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
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

    #[test]
    fn receipt_batch_uses_only_unsettled_receipts() {
        let mut settled = receipt(1);
        settled.batch_id = Some("done".to_string());
        let pending = receipt(2);

        let batch = build_receipt_batch(&[settled, pending.clone()]).unwrap();
        assert_eq!(batch.receipt_ids, vec![pending.id]);
        assert_eq!(batch.receipt_count, 1);
        assert_eq!(batch.merkle_root.len(), 64);
        assert_eq!(batch.batch_id.len(), 64);
    }

    #[test]
    fn receipt_batch_root_changes_with_memory_record_id() {
        let a = receipt(1);
        let mut b = a.clone();
        b.memory_record_id = Some(Uuid::new_v4());

        let batch_a = build_receipt_batch(&[a]).unwrap();
        let batch_b = build_receipt_batch(&[b]).unwrap();
        assert_ne!(batch_a.merkle_root, batch_b.merkle_root);
    }

    #[test]
    fn receipt_migration_plan_splits_legacy_and_correlated_memory_receipts() {
        let mut legacy = receipt(1);
        legacy.id = Uuid::from_u128(1);
        legacy.payer = AgentId::new("legacy@local", [1u8; 32]);

        let mut correlated = receipt(2);
        correlated.id = Uuid::from_u128(2);
        correlated.payer = AgentId::new("correlated@local", [2u8; 32]);
        correlated.memory_record_id = Some(Uuid::from_u128(20));
        correlated.batch_id = Some("batch".to_string());

        let mut compute = receipt(3);
        compute.id = Uuid::from_u128(3);
        compute.resource = ResourceKind::Compute;

        let value = receipt_migration_plan_json(&[legacy, correlated, compute]);

        assert_eq!(
            value["schema"],
            "covenant.settlement.receipt_migration.plan.v1"
        );
        assert_eq!(value["mode"], "dry_run");
        assert_eq!(value["mutation_supported"], false);
        assert_eq!(value["summary"]["receipt_count"], 3);
        assert_eq!(value["summary"]["memory_receipt_count"], 2);
        assert_eq!(value["summary"]["legacy_memory_receipt_count"], 1);
        assert_eq!(value["summary"]["correlated_memory_receipt_count"], 1);
        assert_eq!(value["summary"]["non_memory_receipt_count"], 1);
        assert_eq!(
            value["legacy_uncorrelated_receipts"][0]["receipt_id"],
            Uuid::from_u128(1).to_string()
        );
        assert_eq!(
            value["correlated_memory_receipts"][0]["memory_record_id"],
            Uuid::from_u128(20).to_string()
        );
        assert_eq!(value["refusal"]["apply_supported"], false);
    }

    #[test]
    fn receipt_migration_plan_does_not_export_display_identity() {
        let mut legacy = receipt(1);
        legacy.payer = AgentId::new("private-display@local", [1u8; 32]);

        let raw = serde_json::to_string(&receipt_migration_plan_json(&[legacy])).unwrap();

        assert!(!raw.contains("private-display@local"));
        assert!(raw.contains("payer_pubkey"));
    }

    #[tokio::test]
    async fn in_memory_marks_batch_confirmed() {
        let s = InMemorySettlement::new();
        let r = receipt(9);
        let id = r.id;
        s.record(r).await.unwrap();

        let confirmation = ChainConfirmation {
            chain: "solana".to_string(),
            cluster: "devnet".to_string(),
            batch_id: "batch".to_string(),
            merkle_root: "root".to_string(),
            tx_sig: Some("sig".to_string()),
            slot: Some(12),
            confirmed_at: Some(34),
        };
        assert_eq!(
            s.mark_batch_confirmed(&[id], confirmation).await.unwrap(),
            1
        );

        let rows = s.recent(10).await.unwrap();
        assert_eq!(rows[0].chain.as_deref(), Some("solana"));
        assert_eq!(rows[0].onchain_sig.as_deref(), Some("sig"));
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
