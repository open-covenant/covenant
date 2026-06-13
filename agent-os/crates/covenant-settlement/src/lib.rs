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
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
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

    /// Apply the legacy-receipt correlation backfill to this store's durable
    /// receipts. The file-backed store overrides this to hold its write lock
    /// across the read-modify-write so a receipt recorded concurrently is not
    /// lost to the atomic rewrite. The default is a no-op for backends without
    /// a durable file (in-memory and disabled stores).
    async fn backfill(
        &self,
        dry_run: bool,
        correlations: &[ReceiptBackfillCorrelation],
    ) -> Result<BackfillOutcome, SettlementError> {
        let _ = correlations;
        Ok(BackfillOutcome {
            row_count: 0,
            rollback_path: None,
            dry_run,
        })
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptBackfillCorrelation {
    pub receipt_id: uuid::Uuid,
    pub memory_record_id: uuid::Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillOutcome {
    pub row_count: u64,
    pub rollback_path: Option<PathBuf>,
    pub dry_run: bool,
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
    let batch_id = derive_batch_id(&merkle_root);
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

// Lock-free file backfill logic. Reachable only through `JsonlReceiptStore::backfill`,
// which holds the store write lock; callers must not invoke it directly on a path a
// live store also writes, or a concurrent record() can be lost to the rewrite.
pub(crate) async fn backfill_receipts_with_correlations(
    store_path: impl AsRef<Path>,
    dry_run: bool,
    correlations: &[ReceiptBackfillCorrelation],
) -> Result<BackfillOutcome, SettlementError> {
    let store_path = store_path.as_ref();
    let original = match fs::read_to_string(store_path).await {
        Ok(body) => body,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BackfillOutcome {
                row_count: 0,
                rollback_path: None,
                dry_run,
            });
        }
        Err(e) => return Err(e.into()),
    };

    let correlation_map = correlations
        .iter()
        .map(|correlation| (correlation.receipt_id, correlation.memory_record_id))
        .collect::<HashMap<_, _>>();
    let mut original_values = Vec::new();
    let mut original_receipts = Vec::new();

    for line in original.lines().filter(|line| !line.trim().is_empty()) {
        let value = serde_json::from_str::<serde_json::Value>(line)?;
        let receipt = serde_json::from_value::<SettlementReceipt>(value.clone())?;
        original_values.push(value);
        original_receipts.push(receipt);
    }

    let migration_plan = receipt_migration_plan_json(&original_receipts);
    let legacy_receipt_ids = legacy_uncorrelated_receipt_ids(&migration_plan);
    let mut planned = Vec::new();
    let mut row_count = 0;

    for (original_value, mut receipt) in original_values.into_iter().zip(original_receipts) {
        if legacy_receipt_ids.contains(&receipt.id) {
            if let Some(memory_record_id) = correlation_map.get(&receipt.id) {
                receipt.memory_record_id = Some(*memory_record_id);
            }
        }

        let next_value = serde_json::to_value(&receipt)?;
        if next_value != original_value {
            row_count += 1;
        }
        planned.push(receipt);
    }

    if dry_run || row_count == 0 {
        return Ok(BackfillOutcome {
            row_count,
            rollback_path: None,
            dry_run,
        });
    }

    if let Some(parent) = store_path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let rollback_path = rollback_checkpoint_path(store_path);
    let mut rollback = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&rollback_path)
        .await?;
    rollback.write_all(original.as_bytes()).await?;
    rollback.sync_all().await?;
    drop(rollback);

    let tmp = store_path.with_extension("jsonl.backfill.tmp");
    let mut out = fs::File::create(&tmp).await?;
    for receipt in planned {
        out.write_all(serde_json::to_string(&receipt)?.as_bytes())
            .await?;
        out.write_all(b"\n").await?;
    }
    out.sync_all().await?;
    drop(out);
    fs::rename(&tmp, store_path).await?;
    let store = OpenOptions::new().read(true).open(store_path).await?;
    store.sync_all().await?;

    // Best-effort directory fsync: the file-level sync_all calls above
    // flush the rollback file's contents, the rewritten store's contents,
    // and the renamed store file's inode, but POSIX does not guarantee
    // the rename's directory-entry update is durably on disk until the
    // containing directory is fsynced. Without this step, a crash
    // between the rename and the OS metadata flush could revert the
    // store to the pre-rename inode while the SettlementReceiptBackfillApplied
    // audit row already claims the backfill landed. The single fsync
    // here covers every directory-entry change the apply path made:
    // rollback file create, tmp file create, tmp removal, and store_path
    // retarget. Failures are logged rather than propagated: the rewrite
    // has already succeeded, and surfacing a returned error for a
    // mutation that landed would mislead the operator into thinking the
    // backfill failed. On platforms or filesystems where directory
    // fsync is unsupported the next file syscall typically flushes the
    // directory entry anyway, so the operational guarantee degrades
    // gracefully.
    if let Some(parent) = store_path.parent() {
        match fs::File::open(parent).await {
            Ok(dir) => {
                if let Err(e) = dir.sync_all().await {
                    tracing::warn!(
                        parent = %parent.display(),
                        error = %e,
                        "settlement backfill: parent directory fsync failed; rename may not be crash-durable",
                    );
                }
            }
            Err(e) => tracing::warn!(
                parent = %parent.display(),
                error = %e,
                "settlement backfill: could not open parent directory for fsync; rename may not be crash-durable",
            ),
        }
    }

    Ok(BackfillOutcome {
        row_count,
        rollback_path: Some(rollback_path),
        dry_run,
    })
}

fn rollback_checkpoint_path(store_path: &Path) -> PathBuf {
    let unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let file_name = store_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("receipts.jsonl");
    store_path.with_file_name(format!("{file_name}.backfill-rollback-{unix_ms}.jsonl"))
}

fn legacy_uncorrelated_receipt_ids(plan: &serde_json::Value) -> HashSet<uuid::Uuid> {
    plan.get("legacy_uncorrelated_receipts")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("receipt_id"))
        .filter_map(|value| value.as_str())
        .filter_map(|raw| uuid::Uuid::parse_str(raw).ok())
        .collect()
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
        // Atomic rewrite: tempfile + rename. A mid-write crash with the
        // raw `fs::write` shape would have truncated the receipts file
        // and lost every pre-batch row.
        let tmp = self.path.with_extension("jsonl.tmp");
        let mut out = fs::File::create(&tmp).await?;
        use tokio::io::AsyncWriteExt;
        out.write_all(body.as_bytes()).await?;
        out.sync_all().await?;
        drop(out);
        fs::rename(&tmp, &self.path).await?;
        Ok(updated)
    }

    async fn backfill(
        &self,
        dry_run: bool,
        correlations: &[ReceiptBackfillCorrelation],
    ) -> Result<BackfillOutcome, SettlementError> {
        // Hold the same lock as record()/recent()/mark_batch_confirmed():
        // the backfill reads the store, then atomically renames a rewritten
        // copy over it. Without this guard a receipt appended between the
        // read and the rename is silently clobbered (a lost settlement row).
        let _guard = self.lock.lock().await;
        backfill_receipts_with_correlations(&self.path, dry_run, correlations).await
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

/// Derive the canonical `batch_id` for a receipt batch from its `merkle_root`.
///
/// This is the single source of truth for the `batch_id` ↔ `merkle_root`
/// binding: [`build_receipt_batch`] stamps the id from this function, and the
/// covenantd Check 6 verifier recomputes it to detect a stored receipt whose
/// `batch_id` no longer matches its `merkle_root`. Sharing one implementation
/// is what lets the verifier treat a mismatch as drift in the data rather than
/// risk false-positives from a second copy of the derivation drifting out of
/// step with the producer.
pub fn derive_batch_id(merkle_root: &str) -> String {
    hex32(Sha256::digest(format!("covenant-receipts:{merkle_root}")).into())
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

    fn read_receipt_file(path: &Path) -> Vec<SettlementReceipt> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn legacy_receipt_wire_without_defaults(receipt: &SettlementReceipt) -> String {
        let mut value = serde_json::to_value(receipt).unwrap();
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
        serde_json::to_string(&value).unwrap()
    }

    fn rollback_files(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains(".backfill-rollback-")
            })
            .collect()
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
    async fn recent_malformed_row_aborts_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("receipts.jsonl");
        // A non-empty row that is not a valid SettlementReceipt must abort the
        // read loudly. A silent skip would drop the row's receipt_id and chain
        // context, corrupting correlation backfill and audit reconciliation.
        tokio::fs::write(&path, b"{bad}\n").await.unwrap();
        let store = JsonlReceiptStore::open(path).await.unwrap();
        let err = store
            .recent(10)
            .await
            .expect_err("a malformed row must abort recent()");
        assert!(
            matches!(err, SettlementError::Serde(_)),
            "a malformed receipt row must surface as SettlementError::Serde, not a skipped or defaulted receipt: {err:?}",
        );
    }

    #[tokio::test]
    async fn recent_malformed_truncated_frame_aborts_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("receipts.jsonl");
        // A receipt flushed mid-write leaves an unterminated frame on disk.
        tokio::fs::write(&path, b"{\"id\":\"\n").await.unwrap();
        let store = JsonlReceiptStore::open(path).await.unwrap();
        let err = store
            .recent(10)
            .await
            .expect_err("a truncated frame must abort recent()");
        assert!(
            matches!(err, SettlementError::Serde(_)),
            "a truncated JSON frame must surface as SettlementError::Serde: {err:?}",
        );
    }

    #[tokio::test]
    async fn recent_malformed_after_valid_rows_aborts_read_and_source_downcasts() {
        use std::error::Error;
        use tokio::io::AsyncWriteExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("receipts.jsonl");
        // Write one genuinely valid receipt through the real record() path, then
        // corrupt the log with a trailing malformed row so the failure is the
        // row itself, not an empty or wholly unparseable file.
        let store = JsonlReceiptStore::open(path.clone()).await.unwrap();
        store.record(receipt(10)).await.unwrap();
        let mut f = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .unwrap();
        f.write_all(b"{bad}\n").await.unwrap();
        f.flush().await.unwrap();

        let err = store
            .recent(10)
            .await
            .expect_err("a malformed row after a valid one must abort recent()");
        assert!(
            matches!(err, SettlementError::Serde(_)),
            "recent() must fail on the malformed row even after a valid receipt parses: {err:?}",
        );
        // The read-path error must carry the concrete serde_json::Error as its
        // source so daemon-side diagnostics can downcast and call line/column/
        // classify on the offending settlement.jsonl row. The hand-constructed
        // variant is pinned by the serde_source_delegation_pin test; this pins
        // the live recent() read path end to end.
        let source = err
            .source()
            .expect("recent()'s Serde error must expose its serde_json::Error source");
        assert!(
            source.downcast_ref::<serde_json::Error>().is_some(),
            "recent()'s Serde source must downcast_ref to serde_json::Error for malformed-receipt-row identification: {source}",
        );
    }

    #[tokio::test]
    async fn backfill_receipts_applies_correlations_and_writes_rollback_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("receipts.jsonl");
        let mut legacy = receipt(10);
        legacy.id = Uuid::from_u128(0x10);
        let mut already_correlated = receipt(20);
        already_correlated.id = Uuid::from_u128(0x20);
        already_correlated.memory_record_id = Some(Uuid::from_u128(0x200));
        let mut compute = receipt(30);
        compute.id = Uuid::from_u128(0x30);
        compute.resource = ResourceKind::Compute;
        let original = format!(
            "{}\n{}\n{}\n",
            serde_json::to_string(&legacy).unwrap(),
            serde_json::to_string(&already_correlated).unwrap(),
            serde_json::to_string(&compute).unwrap()
        );
        std::fs::write(&path, &original).unwrap();

        let memory_record_id = Uuid::from_u128(0x100);
        let outcome = backfill_receipts_with_correlations(
            &path,
            false,
            &[
                ReceiptBackfillCorrelation {
                    receipt_id: legacy.id,
                    memory_record_id,
                },
                ReceiptBackfillCorrelation {
                    receipt_id: compute.id,
                    memory_record_id: Uuid::from_u128(0x300),
                },
            ],
        )
        .await
        .unwrap();

        assert_eq!(outcome.row_count, 1);
        assert!(!outcome.dry_run);
        let rollback_path = outcome.rollback_path.expect("apply writes rollback");
        assert_eq!(std::fs::read_to_string(&rollback_path).unwrap(), original);
        assert!(rollback_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("receipts.jsonl.backfill-rollback-"));

        let rows = read_receipt_file(&path);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].memory_record_id, Some(memory_record_id));
        assert_eq!(
            rows[1].memory_record_id,
            already_correlated.memory_record_id
        );
        assert_eq!(rows[2].memory_record_id, None);
    }

    #[tokio::test]
    async fn jsonl_backfill_trait_method_applies_correlations_through_the_store() {
        // The trait method delegates to the same file logic under the store
        // lock; it must produce the identical outcome to the free function so
        // the lock wrapper provably does not alter the rewrite.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("receipts.jsonl");
        let legacy_id = Uuid::from_u128(0x10);
        let mut legacy = receipt(10);
        legacy.id = legacy_id;
        std::fs::write(
            &path,
            format!("{}\n", legacy_receipt_wire_without_defaults(&legacy)),
        )
        .unwrap();

        let store = JsonlReceiptStore::open(path.clone()).await.unwrap();
        let memory_record_id = Uuid::from_u128(0x100);
        let outcome = store
            .backfill(
                false,
                &[ReceiptBackfillCorrelation {
                    receipt_id: legacy_id,
                    memory_record_id,
                }],
            )
            .await
            .unwrap();

        assert_eq!(outcome.row_count, 1);
        assert!(outcome.rollback_path.is_some());
        let rows = read_receipt_file(&path);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].memory_record_id, Some(memory_record_id));
    }

    #[tokio::test]
    async fn jsonl_backfill_does_not_drop_a_concurrently_recorded_receipt() {
        // Regression: backfill must hold the store write lock across its
        // read-modify-rename. The daemon records settlement receipts during
        // intent dispatch while a backfill request runs; without the lock a
        // receipt appended between the backfill's read and its atomic rename is
        // silently clobbered. With the lock either interleaving is safe.
        for i in 0..12u128 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("receipts.jsonl");
            let legacy_id = Uuid::from_u128(0x10);
            let mut legacy = receipt(10);
            legacy.id = legacy_id;
            std::fs::write(
                &path,
                format!("{}\n", legacy_receipt_wire_without_defaults(&legacy)),
            )
            .unwrap();
            let store = JsonlReceiptStore::open(path.clone()).await.unwrap();

            let fresh_id = Uuid::from_u128(0x900 + i);
            let mut fresh = receipt(99);
            fresh.id = fresh_id;
            let correlations = [ReceiptBackfillCorrelation {
                receipt_id: legacy_id,
                memory_record_id: Uuid::from_u128(0x100),
            }];

            let (backfill_res, record_res) =
                tokio::join!(store.backfill(false, &correlations), store.record(fresh));
            backfill_res.unwrap();
            record_res.unwrap();

            let ids: Vec<Uuid> = read_receipt_file(&path).into_iter().map(|r| r.id).collect();
            assert!(
                ids.contains(&fresh_id),
                "iteration {i}: a receipt recorded concurrently with backfill was lost: {ids:?}",
            );
            assert!(
                ids.contains(&legacy_id),
                "iteration {i}: the backfilled legacy receipt is missing: {ids:?}",
            );
        }
    }

    #[tokio::test]
    async fn backfill_receipts_dry_run_reports_without_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("receipts.jsonl");
        let mut legacy = receipt(11);
        legacy.id = Uuid::from_u128(0x11);
        let original = legacy_receipt_wire_without_defaults(&legacy);
        std::fs::write(&path, format!("{original}\n")).unwrap();

        let outcome = backfill_receipts_with_correlations(
            &path,
            true,
            &[ReceiptBackfillCorrelation {
                receipt_id: legacy.id,
                memory_record_id: Uuid::from_u128(0x111),
            }],
        )
        .await
        .unwrap();

        assert_eq!(outcome.row_count, 1);
        assert!(outcome.dry_run);
        assert_eq!(outcome.rollback_path, None);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{original}\n")
        );
        assert!(rollback_files(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn backfill_propagates_non_notfound_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("receipts.jsonl");

        // A directory at the store path makes read_to_string fail with a
        // non-NotFound I/O fault instead of the missing-file `Ok(row_count: 0)`
        // short-circuit.
        std::fs::create_dir(&path).unwrap();

        assert!(matches!(
            backfill_receipts_with_correlations(&path, true, &[])
                .await
                .unwrap_err(),
            SettlementError::Io(_)
        ));
    }

    #[tokio::test]
    async fn recent_and_mark_batch_confirmed_propagate_non_notfound_io_error() {
        // open() always leaves receipts.jsonl on disk, so the NotFound arms
        // (recent -> Ok(empty), mark_batch_confirmed -> Ok(0)) are reserved for
        // a genuinely missing log. Replace it with a directory: File::open
        // succeeds but the first read faults non-NotFound. Both read paths must
        // surface SettlementError::Io, never the empty/zero short-circuit. A
        // silently empty recent() would hide unsettled receipts from batching
        // and audit reconciliation; a silent Ok(0) from mark_batch_confirmed
        // would record a chain confirmation as a no-op on an unreadable log
        // (settlement fail-open regression class).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("receipts.jsonl");
        let store = JsonlReceiptStore::open(path.clone()).await.unwrap();

        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();

        let err = store
            .recent(10)
            .await
            .expect_err("a non-NotFound read fault must abort recent()");
        assert!(
            matches!(err, SettlementError::Io(_)),
            "recent must surface a non-NotFound read fault as SettlementError::Io, not swallow it \
             into an empty receipt view the way only a genuinely missing log may: {err:?}",
        );

        let confirmation = ChainConfirmation {
            chain: "solana".to_string(),
            cluster: "devnet".to_string(),
            batch_id: "batch".to_string(),
            merkle_root: "root".to_string(),
            tx_sig: Some("sig".to_string()),
            slot: Some(12),
            confirmed_at: Some(34),
        };
        let err = store
            .mark_batch_confirmed(&[Uuid::from_u128(1)], confirmation)
            .await
            .expect_err("a non-NotFound read fault must abort mark_batch_confirmed()");
        assert!(
            matches!(err, SettlementError::Io(_)),
            "mark_batch_confirmed must surface a non-NotFound read fault as SettlementError::Io, \
             not report Ok(0) and record a chain confirmation as a no-op on an unreadable log: {err:?}",
        );
    }

    #[tokio::test]
    async fn backfill_receipts_repairs_serde_decodable_legacy_wire_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("receipts.jsonl");
        let mut legacy = receipt(12);
        legacy.id = Uuid::from_u128(0x12);
        let original = legacy_receipt_wire_without_defaults(&legacy);
        std::fs::write(&path, format!("{original}\n")).unwrap();

        let outcome = backfill_receipts_with_correlations(&path, false, &[])
            .await
            .unwrap();

        assert_eq!(outcome.row_count, 1);
        assert!(outcome.rollback_path.is_some());
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_ne!(raw, format!("{original}\n"));
        let value: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
        assert_eq!(value["chain"], serde_json::Value::Null);
        assert_eq!(value["onchain_sig"], serde_json::Value::Null);
        assert!(!value.as_object().unwrap().contains_key("memory_record_id"));
    }

    #[tokio::test]
    async fn backfill_receipts_noops_when_no_row_changes_are_planned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("receipts.jsonl");
        let row = receipt(13);
        let original = format!("{}\n", serde_json::to_string(&row).unwrap());
        std::fs::write(&path, &original).unwrap();

        let outcome = backfill_receipts_with_correlations(&path, false, &[])
            .await
            .unwrap();

        assert_eq!(outcome.row_count, 0);
        assert_eq!(outcome.rollback_path, None);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(rollback_files(dir.path()).is_empty());
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
    fn build_receipt_batch_pins_batch_id_domain_separator_prefix() {
        // covenant_settlement::build_receipt_batch derives
        // batch_id with:
        //
        //   hex32(Sha256::digest(format!("covenant-receipts:{merkle_root}")).into())
        //
        // The literal prefix 'covenant-receipts:' is a domain
        // separator — it isolates the batch_id hash from the
        // merkle_root hash (also a sha256 product) so an attacker
        // cannot pre-compute one from the other under a length-
        // extension or hash-collision attack on the same input
        // domain. The prefix appears exactly ONCE in non-test code,
        // inside `derive_batch_id`, which `build_receipt_batch` calls.
        //
        // receipt_batch_uses_only_unsettled_receipts pins
        // 'batch.batch_id.len() == 64' (length, not content);
        // receipt_batch_root_changes_with_memory_record_id pins
        // merkle_root inequality but not batch_id. A refactor
        // that dropped the prefix, renamed to underscore form, or
        // swapped the ':' separator for '/' would silently shift
        // every batch_id and break on-chain anchoring against
        // historical batches without parse-time signal.

        let mut a = receipt(1);
        a.id = Uuid::from_u128(0xa);
        let mut b = receipt(2);
        b.id = Uuid::from_u128(0xb);
        let mut c = receipt(3);
        c.id = Uuid::from_u128(0xc);

        let batch = build_receipt_batch(&[a.clone(), b.clone(), c.clone()]).expect(
            "fixture batch must build — used as the input whose \
             batch_id is checked against the manually computed \
             'covenant-receipts:'||merkle_root sha256",
        );

        let expected =
            hex32(Sha256::digest(format!("covenant-receipts:{}", batch.merkle_root)).into());
        assert_eq!(
            batch.batch_id, expected,
            "batch.batch_id must equal hex32(sha256('covenant-receipts:'||merkle_root)) \
             verbatim — anchors the literal domain-separator prefix. \
             A refactor that dropped the prefix, renamed it (e.g., \
             'covenant_receipts:' with underscore), or swapped the \
             ':' separator for '/' would silently shift every \
             batch_id and break on-chain anchoring against historical \
             batches",
        );
        assert_eq!(
            batch.batch_id,
            derive_batch_id(&batch.merkle_root),
            "build_receipt_batch must stamp batch_id through the shared \
             derive_batch_id so the covenantd Check 6 verifier recomputes \
             the identical batch_id<->merkle_root binding; a second copy of \
             the derivation drifting out of step would make the verifier \
             false-positive on every confirmed receipt",
        );

        // Second batch with different receipts produces a different
        // merkle_root; the prefix is constant across that, so the
        // recomputation under the same prefix must still match. This
        // anchors that the prefix is not accidentally tied to a
        // specific merkle_root value.
        let mut d = receipt(4);
        d.id = Uuid::from_u128(0xd);
        let mut e = receipt(5);
        e.id = Uuid::from_u128(0xe);
        let batch2 = build_receipt_batch(&[d, e]).expect("second batch must build");
        assert_ne!(
            batch.merkle_root, batch2.merkle_root,
            "the two batches must have different merkle_roots — \
             otherwise the test below trivially passes on identical \
             inputs and does not anchor the prefix constancy",
        );

        let expected2 =
            hex32(Sha256::digest(format!("covenant-receipts:{}", batch2.merkle_root)).into());
        assert_eq!(
            batch2.batch_id, expected2,
            "the second batch_id must also equal hex32(sha256('covenant-receipts:'||merkle_root)) \
             — anchors that the prefix is reused across different \
             merkle_root inputs rather than being tied to a specific \
             root value",
        );
        assert_ne!(
            batch.batch_id, batch2.batch_id,
            "the two batch_ids must differ because the underlying \
             merkle_root differs — sanity check on the test \
             distinctness",
        );
    }

    #[test]
    fn build_receipt_batch_pins_odd_leaf_count_duplicates_last_leaf_convention() {
        // covenant_settlement::build_receipt_batch computes
        // the Merkle root of all unsettled receipts in a batch.
        // The critical convention is:
        //
        //   let right = pair.get(1).copied().unwrap_or(pair[0]);
        //
        // For an odd-count level, the LAST leaf is duplicated as its
        // own right sibling — the Bitcoin/Solana Merkle convention.
        // The resulting H(left || left) becomes a parent node in the
        // reduction. This root is what the on-chain Solana settlement
        // program will verify against the receipt batch anchor; a
        // different convention silently produces a different root for
        // every odd-count batch and on-chain verification would fail.
        //
        // receipt_batch_uses_only_unsettled_receipts and
        // receipt_batch_root_changes_with_memory_record_id pin 1-leaf
        // batches. The 1-leaf path SKIPS the while-loop
        // because level.len() > 1 is false from the start, so the
        // odd-count branch is never executed. This pin fills the
        // gap by exercising a 3-leaf batch where the chunks(2)
        // iterator produces a final pair with a single element.

        let mut a = receipt(1);
        a.id = Uuid::from_u128(0xa);
        let mut b = receipt(2);
        b.id = Uuid::from_u128(0xb);
        let mut c = receipt(3);
        c.id = Uuid::from_u128(0xc);

        let batch3 = build_receipt_batch(&[a.clone(), b.clone(), c.clone()]).expect(
            "the 3-leaf odd-count batch must produce a valid root — \
             this is the case the existing 1-leaf tests do not \
             exercise because the while-loop never iterates with one \
             leaf",
        );

        // The 4-leaf even-count input where the last leaf is EXPLICITLY
        // duplicated. Under the duplicate-last convention, the 3-leaf
        // and 4-leaf reductions must produce the SAME root because
        // the implicit duplication in chunks(2) matches the explicit
        // duplication in the input.
        let batch4 = build_receipt_batch(&[a.clone(), b.clone(), c.clone(), c.clone()]).expect(
            "the 4-leaf input with explicit c-duplication must \
             produce a valid root — used as the algebraic equivalent \
             of the 3-leaf implicit duplication",
        );

        assert_eq!(
            batch3.merkle_root, batch4.merkle_root,
            "merkle_root([a,b,c]) must equal merkle_root([a,b,c,c]) \
             — anchors the duplicate-last-leaf convention. A refactor \
             that swapped 'unwrap_or(pair[0])' for \
             'unwrap_or([0u8; 32])' (null-right-child convention) or \
             that dropped the second hasher.update for odd singletons \
             would silently produce a different 3-leaf root while \
             the 4-leaf root stays the same, and on-chain Solana \
             verification would reject every odd-count batch with no \
             parse-time signal",
        );

        // Bookkeeping pins: the convention applies to root
        // computation only, NOT to receipt_count or receipt_ids. The
        // 3-leaf batch must report 3 receipts and 3 ids; the 4-leaf
        // batch must report 4 receipts and 4 ids (including the
        // duplicated c.id). A refactor that deduplicated the input
        // before building receipt_ids would silently change the
        // settled-row identification.
        assert_eq!(
            batch3.receipt_count, 3,
            "3-leaf batch must report receipt_count=3; the duplicate-\
             last-leaf convention applies to the Merkle tree level, \
             not to the receipt bookkeeping fields",
        );
        assert_eq!(
            batch4.receipt_count, 4,
            "4-leaf batch must report receipt_count=4 (including the \
             explicit duplicate); a refactor that deduplicated input \
             by id would silently make the 3-leaf and 4-leaf inputs \
             indistinguishable at the bookkeeping level",
        );
        assert_eq!(batch3.receipt_ids.len(), 3);
        assert_eq!(batch4.receipt_ids.len(), 4);
        assert_eq!(
            batch4.receipt_ids[2], batch4.receipt_ids[3],
            "the explicit c-duplication in the 4-leaf input must \
             surface as identical receipt_ids[2] and [3] — the \
             bookkeeping path carries the input verbatim",
        );
    }

    #[test]
    fn build_receipt_batch_pins_two_leaf_merkle_root_concatenates_left_then_right() {
        // build_receipt_batch reduces a level of leaf hashes pairwise as
        //
        //   hasher.update(pair[0]); hasher.update(right);   (lib.rs:116-117)
        //
        // i.e. sha256(left || right) with the left leaf hashed first.
        // That root is the settlement anchor the on-chain Solana program
        // recomputes to verify a batch. The other batch tests pin only
        // RELATIONAL properties of merkle_root: length 64, inequality
        // across distinct inputs, and merkle_root([a,b,c]) ==
        // merkle_root([a,b,c,c]) for the odd-leaf convention. Every one of
        // those is invariant under a left/right transposition of the
        // concatenation — under sha256(right || left) the lengths, the
        // cross-input inequalities, and the odd-leaf equality all still
        // hold — so the exact byte order of the Merkle combination is
        // unpinned. A transposed off-chain root stays a valid 64-char hex
        // string and passes the whole suite while diverging from every
        // independent verifier that recomputes the anchor. Pin the
        // two-leaf root to the exact left-then-right combination, recomputed
        // from the same receipt_hash primitive, and pin that leaf order is
        // load-bearing.
        let mut a = receipt(1);
        a.id = Uuid::from_u128(0xa);
        let mut b = receipt(2);
        b.id = Uuid::from_u128(0xb);

        let batch =
            build_receipt_batch(&[a.clone(), b.clone()]).expect("two-leaf batch must build");

        let mut hasher = Sha256::new();
        hasher.update(receipt_hash(&a));
        hasher.update(receipt_hash(&b));
        let expected_root = hex32(hasher.finalize().into());
        assert_eq!(
            batch.merkle_root, expected_root,
            "two-leaf merkle_root must equal hex32(sha256(receipt_hash(a) || receipt_hash(b))) \
             with a hashed before b; a transposed sha256(receipt_hash(b) || receipt_hash(a)) \
             still yields a valid 64-char hex root and passes every relational batch test but \
             diverges from the on-chain anchor recomputation",
        );

        // Leaf order is load-bearing: sha256 is not concatenation-commutative,
        // so [a,b] and [b,a] must not collide unless a refactor sorted or
        // otherwise order-normalized the pair before hashing — which would
        // let two batches settling the same receipt set in different leaf
        // positions share one anchor.
        let swapped = build_receipt_batch(&[b, a]).expect("swapped two-leaf batch must build");
        assert_ne!(
            batch.merkle_root, swapped.merkle_root,
            "merkle_root([a,b]) must not equal merkle_root([b,a]); an order-normalizing \
             refactor would collapse distinct leaf orderings onto one settlement anchor",
        );
    }

    #[test]
    fn build_receipt_batch_pins_four_leaf_two_level_root_across_pair_order() {
        // build_receipt_batch reduces leaves level by level (while
        // level.len() > 1, lib.rs:111-121): each level pairs adjacent hashes
        // left-to-right and pushes sha256(left || right) into the next level IN
        // ORDER. build_receipt_batch_pins_two_leaf_merkle_root_concatenates_-
        // left_then_right pins WITHIN-pair order (a before b) and
        // build_receipt_batch_pins_odd_leaf_count_duplicates_last_leaf_-
        // convention pins the duplicate-last rule, but both run a single
        // non-trivial pair per level, so ACROSS-pair order -- which parent lands
        // first in the next level -- is unpinned. A 4-leaf batch is the smallest
        // balanced two-level tree that exercises it:
        //
        //   root = sha256( sha256(h_a || h_b) || sha256(h_c || h_d) )
        //
        // A refactor that built a level in reverse (next.reverse(), or walking
        // chunks back-to-front) leaves the two-leaf root unchanged (one pair =>
        // one parent, reversal is a no-op) and leaves the odd-leaf RELATIONAL
        // equality intact (the 3-leaf and 4-leaf-dup roots reverse identically),
        // so it passes every existing batch test while diverging from the
        // on-chain anchor for any batch with two or more pairs. Pin the exact
        // two-level root, recomputed from receipt_hash via fresh hashers.
        let mut a = receipt(1);
        a.id = Uuid::from_u128(0xa);
        let mut b = receipt(2);
        b.id = Uuid::from_u128(0xb);
        let mut c = receipt(3);
        c.id = Uuid::from_u128(0xc);
        let mut d = receipt(4);
        d.id = Uuid::from_u128(0xd);

        let batch = build_receipt_batch(&[a.clone(), b.clone(), c.clone(), d.clone()])
            .expect("four-leaf batch must build");

        let pair = |left: [u8; 32], right: [u8; 32]| -> [u8; 32] {
            let mut hasher = Sha256::new();
            hasher.update(left);
            hasher.update(right);
            hasher.finalize().into()
        };
        let l_ab = pair(receipt_hash(&a), receipt_hash(&b));
        let l_cd = pair(receipt_hash(&c), receipt_hash(&d));
        let expected_root = hex32(pair(l_ab, l_cd));
        assert_eq!(
            batch.merkle_root, expected_root,
            "four-leaf merkle_root must equal sha256(sha256(h_a||h_b) || \
             sha256(h_c||h_d)) with the first pair's parent hashed before the \
             second's; a level built in reverse keeps the two-leaf and odd-leaf \
             tests green but diverges from the on-chain anchor for any tree with \
             two or more pairs",
        );

        // Across-pair order is load-bearing: swapping the two pairs ([c,d,a,b])
        // must change the root, or a pair-sort / order-normalization refactor
        // would collapse distinct batch orderings onto one anchor.
        let swapped =
            build_receipt_batch(&[c, d, a, b]).expect("reordered four-leaf batch must build");
        assert_ne!(
            batch.merkle_root, swapped.merkle_root,
            "merkle_root([a,b,c,d]) must not equal merkle_root([c,d,a,b]); an \
             across-pair sort or order-normalization would collapse distinct \
             batch orderings onto one settlement anchor",
        );
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
    fn receipt_migration_plan_json_pins_additional_contract_fields() {
        // covenant_settlement::receipt_migration_plan_json (line
        // 100-168) emits the schema-versioned operator-facing
        // migration-plan envelope. The sibling pin
        // (receipt_migration_plan_splits_legacy_and_correlated_memory_receipts)
        // covers ~60% of the documented schema. This pin
        // closes the remaining contract surfaces:
        //
        //   summary.batched_receipt_count / unbatched_receipt_count
        //   summary.malformed_row_count zero-literal default
        //   expected_correlation_inputs (4-element documented array)
        //   refusal.reason exact operator-facing rationale
        //   status discriminants on per-row entries
        //   onchain_settled boolean OR semantics across both arms
        //
        // A refactor that flipped onchain_settled from
        // tx_sig.is_some() || onchain_sig.is_some() to AND under a
        // 'tighten the on-chain check' rationale would silently flip
        // every dashboard row that has only one of the two signature
        // fields populated. A refactor that renamed the status
        // discriminants would silently break operator triage filters.
        // A refactor that omitted malformed_row_count under an 'omit
        // zero defaults' rationale would silently change the schema
        // for downstream parsers that pin the field as required u64.

        let mut legacy = receipt(1);
        legacy.id = Uuid::from_u128(0xa);
        legacy.payer = AgentId::new("legacy@local", [1u8; 32]);

        let mut correlated = receipt(2);
        correlated.id = Uuid::from_u128(0xb);
        correlated.memory_record_id = Some(Uuid::from_u128(0x20));
        correlated.batch_id = Some("batch-correlated".to_string());

        let mut compute = receipt(3);
        compute.id = Uuid::from_u128(0xc);
        compute.resource = ResourceKind::Compute;

        let value = receipt_migration_plan_json(&[legacy, correlated, compute]);

        assert_eq!(
            value["summary"]["batched_receipt_count"], 1,
            "batched_receipt_count must equal the number of receipts \
             with batch_id Some — only the correlated receipt has a \
             batch_id in this fixture; a refactor that swapped the \
             filter to receipt.tx_sig.is_some() under a 'batched means \
             on-chain' conflation would silently surface all \
             tx_sig-bearing receipts as batched even when batch_id is \
             None",
        );
        assert_eq!(
            value["summary"]["unbatched_receipt_count"], 2,
            "unbatched_receipt_count must equal receipts.len() minus \
             batched_receipt_count via .saturating_sub — the saturating \
             form is load-bearing because a future refactor that \
             populated batched_receipt_count from a different source \
             (e.g., a join against the chain index) could exceed the \
             receipt count and a plain subtraction would underflow \
             usize, panicking the daemon's read-only planning command",
        );
        assert_eq!(
            value["summary"]["malformed_row_count"], 0,
            "malformed_row_count must be the zero-literal default — \
             pins the field as a present-and-zero u64 so downstream \
             parsers that decode it as required u64 do not regress to \
             a 'missing field' error if a refactor swapped 0 for None \
             under an 'omit zero defaults' rationale",
        );
        assert_eq!(
            value["expected_correlation_inputs"],
            serde_json::json!([
                "memory_record_id from the originating memory write",
                "payer pubkey match between receipt.payer and memory.owner",
                "before and after receipt hash evidence for any future mutation",
                "audit event id for the future authorized mutation"
            ]),
            "expected_correlation_inputs must equal the 4-element \
             documented contract verbatim — each string is a \
             load-bearing operator-facing input contract for the \
             future authorized correlation mutation; a refactor that \
             dropped, reordered, or rewrote any string would silently \
             change the documented operator workflow without bumping \
             the schema version",
        );
        assert_eq!(
            value["refusal"]["reason"],
            "settlement receipt migration is read-only; mutation requires a separate \
             authorized command with rollback and audit evidence",
            "refusal.reason must remain the exact operator-facing \
             rationale — the string is the documented justification \
             for why apply_supported is false and operator dashboards \
             surface it verbatim; a refactor that paraphrased it under \
             a 'shorten the message' pass would silently change \
             support-channel reproductions",
        );
        assert_eq!(
            value["legacy_uncorrelated_receipts"][0]["status"], "needs_memory_record_match",
            "legacy_uncorrelated_receipts[*].status must be the \
             documented 'needs_memory_record_match' discriminant — \
             operator triage tools filter on this exact string; a \
             refactor that renamed it to 'uncorrelated' or \
             'pending_correlation' under a 'shorter status names' \
             rationale would silently break every grep that anchors \
             on the documented value",
        );
        assert_eq!(
            value["correlated_memory_receipts"][0]["status"], "already_correlated",
            "correlated_memory_receipts[*].status must be the \
             documented 'already_correlated' discriminant — paired with \
             needs_memory_record_match above, the two strings are the \
             read-side state machine for the upcoming correlation \
             mutation; a refactor that collapsed both into a single \
             'status' enum field with different literals would surface \
             here",
        );
        assert_eq!(
            value["legacy_uncorrelated_receipts"][0]["onchain_settled"], false,
            "onchain_settled must be false when both tx_sig and \
             onchain_sig are None — pins the negative arm of the OR \
             (tx_sig.is_some() || onchain_sig.is_some())",
        );

        // Cover both arms of the OR so a refactor flipping || to &&
        // surfaces here. tx_sig-only and onchain_sig-only must each
        // map to onchain_settled=true.
        let mut tx_only = receipt(4);
        tx_only.id = Uuid::from_u128(0xd);
        tx_only.tx_sig = Some("tx-sig-only".to_string());
        let value_tx_only = receipt_migration_plan_json(&[tx_only]);
        assert_eq!(
            value_tx_only["legacy_uncorrelated_receipts"][0]["onchain_settled"], true,
            "tx_sig=Some, onchain_sig=None must map onchain_settled to \
             true — pins the LHS arm of the OR. A refactor that flipped \
             || to && would surface here as false because the AND \
             requires both signature fields to be Some",
        );

        let mut onchain_only = receipt(5);
        onchain_only.id = Uuid::from_u128(0xe);
        onchain_only.onchain_sig = Some("onchain-sig-only".to_string());
        let value_onchain_only = receipt_migration_plan_json(&[onchain_only]);
        assert_eq!(
            value_onchain_only["legacy_uncorrelated_receipts"][0]["onchain_settled"], true,
            "tx_sig=None, onchain_sig=Some must map onchain_settled to \
             true — pins the RHS arm of the OR. Combined with the \
             tx-only and both-None arms above, the three input \
             combinations exhaustively pin the boolean truth table for \
             the documented OR semantics",
        );
    }

    #[test]
    fn receipt_hash_pins_conditional_memory_record_id_and_per_field_determinism() {
        // covenant_settlement::receipt_hash computes
        // the SHA-256 of a JSON payload over 5 always-present fields
        // (id, payer base58, resource, credits_consumed, settled_at)
        // plus an optional memory_record_id field that lands in the
        // payload ONLY when receipt.memory_record_id is Some. Two
        // receipts identical except for the memory_record_id being
        // None vs Some(uuid) must therefore produce different hashes
        // — un-correlated and correlated receipts are intentionally
        // distinct at the Merkle leaf level.
        //
        // receipt_hash is used by build_receipt_batch to
        // compute Merkle leaves. A refactor that unconditionally
        // inserted memory_record_id (even with null when None) under
        // a 'be consistent about field presence' rationale would
        // silently change every un-correlated receipt hash on-chain
        // and break batch reconstruction. A refactor that dropped
        // credits_consumed or settled_at from the payload under a
        // 'these are also persisted elsewhere' rationale would let
        // credit-amount tampering go undetected at the leaf level.

        let fixed_id = Uuid::from_u128(0xabc);

        let mut none_a = receipt(1);
        none_a.id = fixed_id;
        let h_none_a = receipt_hash(&none_a);

        let mut none_b = receipt(1);
        none_b.id = fixed_id;
        let h_none_b = receipt_hash(&none_b);

        assert_eq!(
            h_none_a, h_none_b,
            "two receipts with identical field values must produce \
             identical hashes — pins determinism. A refactor that \
             introduced any non-deterministic input (timestamp, \
             nonce, random salt) into the payload would surface \
             here",
        );

        assert_eq!(
            h_none_a.len(),
            32,
            "hash output must be exactly 32 bytes (SHA-256 width) — \
             pins that the hash function is SHA-256 specifically, \
             not a different digest with a different output width",
        );

        let mut some = receipt(1);
        some.id = fixed_id;
        some.memory_record_id = Some(Uuid::from_u128(0xdef));
        let h_some = receipt_hash(&some);
        assert_ne!(
            h_none_a, h_some,
            "two receipts identical except memory_record_id being \
             None vs Some must produce DIFFERENT hashes — pins the \
             conditional 'if let Some(id) = receipt.memory_record_id' \
             payload insert. A refactor that \
             unconditionally inserted memory_record_id (even as null) \
             would collapse the two cases to the same hash and \
             silently change every un-correlated receipt's on-chain \
             hash. Settlement-chain audits would surface mass hash \
             drift with no clear cause",
        );

        let mut diff_credits = receipt(1);
        diff_credits.id = fixed_id;
        diff_credits.credits_consumed = 2;
        let h_diff_credits = receipt_hash(&diff_credits);
        assert_ne!(
            h_none_a, h_diff_credits,
            "two receipts identical except credits_consumed must \
             produce DIFFERENT hashes — pins that credits_consumed \
             is part of the payload. A refactor that \
             dropped the field under a 'these are also persisted in \
             the budget ledger' rationale would let credit-amount \
             tampering go undetected at the Merkle leaf level — a \
             malicious operator could rewrite credits_consumed in the \
             JSONL between batches and the Merkle root would remain \
             unchanged",
        );

        let mut diff_settled_at = receipt(1);
        diff_settled_at.id = fixed_id;
        diff_settled_at.settled_at = 999;
        let h_diff_settled_at = receipt_hash(&diff_settled_at);
        assert_ne!(
            h_none_a, h_diff_settled_at,
            "two receipts identical except settled_at must produce \
             DIFFERENT hashes — pins that settled_at is part of the \
             payload. A refactor that dropped settled_at \
             would let temporal-replay attacks against the Merkle \
             leaf set go undetected",
        );
    }

    #[test]
    fn receipt_hash_pins_canonical_json_field_order_and_value_encoding() {
        // receipt_hash (lib.rs:358) is sha256(serde_json::to_vec(payload)) over
        // a serde_json::json! object. With serde_json's default Map backing (no
        // preserve_order feature anywhere in the workspace) keys serialize in
        // BTreeMap ALPHABETICAL order, and the conditional memory_record_id is
        // sorted into that order rather than appended. This hash is the Merkle
        // leaf the on-chain Solana program recomputes, so the exact bytes --
        // field set, field ORDER, and per-field value encoding (id as a
        // hyphenated UUID string, payer as Bitcoin-base58, resource as the
        // lowercase ResourceKind rename) -- are a cross-system contract.
        //
        // receipt_hash_pins_conditional_memory_record_id_and_per_field_-
        // determinism pins determinism, the 32-byte width, and per-field
        // inequality, but every one of those survives a field-ORDER flip: if any
        // workspace dependency enabled serde_json/preserve_order the payload
        // would silently switch to insertion order, changing every leaf hash and
        // breaking on-chain verification while determinism and all inequalities
        // still hold. Pin the exact canonical bytes by recomputing the hash from
        // a hand-spelled alphabetical-order literal. payer is bs58 of the
        // all-zero key (32 '1's, pinned by covenant-types
        // pubkey_base58_pins_bitcoin_alphabet_leading_zero_ones_and_distinctness);
        // credits_consumed (7) and settled_at (42) differ so the two numeric
        // keys are distinguishable by value.
        let mut r = receipt(7);
        r.id = Uuid::from_u128(0xabc);
        r.settled_at = 42;

        let canonical = concat!(
            "{",
            "\"credits_consumed\":7,",
            "\"id\":\"00000000-0000-0000-0000-000000000abc\",",
            "\"payer\":\"11111111111111111111111111111111\",",
            "\"resource\":\"memory\",",
            "\"settled_at\":42",
            "}"
        );
        let expected: [u8; 32] = Sha256::digest(canonical.as_bytes()).into();
        assert_eq!(
            receipt_hash(&r),
            expected,
            "receipt_hash must equal sha256 of the exact alphabetical-order JSON \
             payload; a serde_json preserve_order flip (insertion order), a \
             value-encoding change (base58 -> hex or checked payer, a non-rename \
             resource, a non-hyphenated id), or a field add/drop would diverge \
             here while determinism and per-field inequality still hold",
        );

        // The conditional memory_record_id sorts into alphabetical position
        // (after id, before payer) -- it is NOT appended last. A refactor that
        // appended it, or an insertion-order regression, moves the field and
        // changes the leaf hash for every correlated receipt.
        let mut with_mem = r.clone();
        with_mem.memory_record_id = Some(Uuid::from_u128(0xdef));
        let canonical_mem = concat!(
            "{",
            "\"credits_consumed\":7,",
            "\"id\":\"00000000-0000-0000-0000-000000000abc\",",
            "\"memory_record_id\":\"00000000-0000-0000-0000-000000000def\",",
            "\"payer\":\"11111111111111111111111111111111\",",
            "\"resource\":\"memory\",",
            "\"settled_at\":42",
            "}"
        );
        let expected_mem: [u8; 32] = Sha256::digest(canonical_mem.as_bytes()).into();
        assert_eq!(
            receipt_hash(&with_mem),
            expected_mem,
            "with memory_record_id = Some, the field must serialize in \
             alphabetical position between id and payer; appending it last or an \
             insertion-order regression would change the Merkle leaf",
        );
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

    #[test]
    fn intent_dispatch_credits_pins_v0_flat_cost_constant_and_accessor_equality() {
        // covenant_settlement::INTENT_DISPATCH_CREDITS is the
        // v0 flat-cost floor: 1 credit per intent dispatch. The
        // intent_dispatch_credits() accessor mirrors the
        // constant — the docstring documents that 'future variants that
        // price per agent or per tool-call can replace the body without
        // touching callers', so the accessor is the indirection point
        // for v1 pricing migrations and the constant is the v0
        // tripwire. memory_write_credits_minimum_one pins the
        // byte-floor for memory writes; intent_dispatch_credits has
        // no analogous pin.
        //
        // Three load-bearing arms, each pinned independently:
        //   (1) INTENT_DISPATCH_CREDITS == 1 — the v0 flat-cost floor.
        //   (2) intent_dispatch_credits() == INTENT_DISPATCH_CREDITS —
        //       the accessor-mirror equality.
        //   (3) intent_dispatch_credits() == 1 — a redundant direct
        //       floor pin that catches a refactor where the constant
        //       AND the accessor body are both bumped in lockstep
        //       (e.g., a search-replace that hits both lines).
        //
        // A regression that flipped the constant to 0 during a
        // 'make it free for testing' refactor would silently disable
        // the budget gate — BudgetLedger::try_debit would always
        // approve every intent dispatch and the per-hour credit cap
        // would become a no-op surface with no parse-time signal.
        assert_eq!(
            INTENT_DISPATCH_CREDITS, 1u64,
            "INTENT_DISPATCH_CREDITS must remain the v0 flat-cost floor \
             of 1 credit per intent dispatch — a refactor that flipped \
             this to 0 during a 'make it free for testing' pass would \
             silently disable the daemon's pre-spawn budget gate, every \
             BudgetLedger::try_debit call would approve, and the \
             per-hour credit cap would become a write-only metric that \
             never throttles; a refactor that bumped it for an \
             experimental pricing trial would silently diverge \
             callers' refill-ETA sizing from the actual drain rate",
        );
        assert_eq!(
            intent_dispatch_credits(),
            INTENT_DISPATCH_CREDITS,
            "intent_dispatch_credits() must mirror INTENT_DISPATCH_CREDITS \
             — the docstring documents the accessor as the indirection \
             point for future per-agent pricing migrations, so the \
             accessor body must remain equal to the public constant for \
             v0 callers; a refactor that hard-coded a different value \
             in the accessor body during an experimental trial would \
             let downstream code reading INTENT_DISPATCH_CREDITS \
             observe one value while the daemon's debit path charges \
             another, making budget-exhaustion ETAs unreliable",
        );
        assert_eq!(
            intent_dispatch_credits(),
            1u64,
            "intent_dispatch_credits() must remain 1 — independent floor \
             pin that catches a lockstep refactor where INTENT_DISPATCH_CREDITS \
             AND the accessor body are bumped together (e.g., a \
             search-replace that hits both lines); the accessor-mirror \
             equality pin above passes for any matched pair but only \
             this direct value pin catches the lockstep regression",
        );
    }

    #[test]
    fn hex32_pins_lowercase_fixed_width_and_byte_order() {
        // All-zero input → 64 ASCII '0' chars. Pins the high-nibble
        // emission: a regression that skipped the high nibble of
        // zero-prefixed bytes would shrink the output below 64 chars.
        let zeros = hex32([0u8; 32]);
        assert_eq!(zeros, "0".repeat(64));

        // All-0xff input → 64 lowercase 'f' chars. Pins the lowercase
        // invariant against a {:X} regression and confirms the width
        // matches the upper byte boundary.
        let ones = hex32([0xff_u8; 32]);
        assert_eq!(ones, "f".repeat(64));

        // Determinism: repeated calls produce byte-identical output so
        // batch_id and merkle_root remain stable across the pipeline.
        let mut sample = [0u8; 32];
        for (i, slot) in sample.iter_mut().enumerate() {
            *slot = i as u8;
        }
        assert_eq!(hex32(sample), hex32(sample));

        // Known-byte-pattern reference. A byte-order or nibble-swap
        // regression on 0..32 would diverge from this exact string.
        let reference = "000102030405060708090a0b0c0d0e0f\
             101112131415161718191a1b1c1d1e1f";
        assert_eq!(hex32(sample), reference);

        // Lowercase invariant across an arbitrary mix; a {:X} regression
        // would surface here even on inputs that include digits both
        // above and below 0x0a.
        let mixed = [
            0x00, 0x0a, 0x10, 0xa0, 0xab, 0xcd, 0xef, 0xff, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc,
            0xde, 0xf0, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f, 0xfa, 0xfb,
        ];
        let encoded = hex32(mixed);
        assert_eq!(encoded.chars().count(), 64);
        assert!(
            encoded
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "hex32 must emit lowercase hex only; got {encoded}",
        );
    }

    #[test]
    fn annotate_receipt_pins_complete_confirmation_field_mapping_and_onchain_sig_alias() {
        // annotate_receipt stamps a SettlementReceipt
        // with ChainConfirmation evidence after the receipt batch is
        // confirmed on-chain. Eight assignments plus one load-bearing
        // aliasing contract: receipt.onchain_sig =
        // confirmation.tx_sig.clone() binds the legacy onchain_sig
        // field to the same Option<String> as receipt.tx_sig — the
        // SettlementReceipt struct docstring (covenant-types/src/lib.rs
        // line 333-337) documents 'onchain_sig remains as a backwards-
        // compatible alias for tx_sig while older clients roll forward'.
        //
        // in_memory_marks_batch_confirmed covers two of the eight
        // assignments (chain and onchain_sig) and does not assert
        // tx_sig <-> onchain_sig equality, leaving six field mappings
        // and the alias equality invariant unpinned. Pin each
        // assignment AND the alias equality so a refactor that swaps
        // annotate_receipt for a builder-pattern alternative or that
        // drops the redundant-looking onchain_sig binding (e.g., during
        // a 'simplify' pass that mistakes the alias for dead code) is
        // caught at review time.
        let mut r = receipt(7);
        // Sanity-check the receipt() fixture: all chain metadata fields
        // start as None so each assertion below can observe the
        // annotate_receipt assignment as a transition from None to
        // Some.
        assert!(r.chain.is_none());
        assert!(r.cluster.is_none());
        assert!(r.batch_id.is_none());
        assert!(r.merkle_root.is_none());
        assert!(r.tx_sig.is_none());
        assert!(r.slot.is_none());
        assert!(r.confirmed_at.is_none());
        assert!(r.onchain_sig.is_none());

        let confirmation = ChainConfirmation {
            chain: "solana".to_string(),
            cluster: "devnet".to_string(),
            batch_id: "batch-1".to_string(),
            merkle_root: "root-hex".to_string(),
            tx_sig: Some("sig-fixture".to_string()),
            slot: Some(12),
            confirmed_at: Some(34),
        };

        annotate_receipt(&mut r, &confirmation);

        assert_eq!(
            r.chain.as_deref(),
            Some("solana"),
            "annotate_receipt must bind receipt.chain to Some(confirmation.chain.clone()) — a refactor that wired ChainConfirmation.chain to a different receipt field would silently strand the chain metadata",
        );
        assert_eq!(
            r.cluster.as_deref(),
            Some("devnet"),
            "annotate_receipt must bind receipt.cluster to Some(confirmation.cluster.clone()) — a refactor that dropped this assignment during a 'sanitize for export' pass would silently strip every confirmed receipt of its cluster attribution",
        );
        assert_eq!(
            r.batch_id.as_deref(),
            Some("batch-1"),
            "annotate_receipt must bind receipt.batch_id to Some(confirmation.batch_id.clone()) — receipt.batch_id is the join key build_receipt_batch and the JSONL store use to identify settled rows; dropping it would silently make every confirmed receipt look unbatched and re-batchable",
        );
        assert_eq!(
            r.merkle_root.as_deref(),
            Some("root-hex"),
            "annotate_receipt must bind receipt.merkle_root to Some(confirmation.merkle_root.clone()) — downstream audit and provenance code uses merkle_root for inclusion-proof reconstruction; a regression would break the proof chain",
        );
        assert_eq!(
            r.tx_sig.as_deref(),
            Some("sig-fixture"),
            "annotate_receipt must bind receipt.tx_sig to confirmation.tx_sig.clone() (Option to Option) — the existing in_memory_marks_batch_confirmed pin only asserts onchain_sig; the tx_sig assignment is the load-bearing one for newer clients and the source for the onchain_sig alias",
        );
        assert_eq!(
            r.slot,
            Some(12),
            "annotate_receipt must bind receipt.slot to confirmation.slot — the existing partial-coverage test does not check slot; a refactor that defaulted slot to None for un-confirmed-but-batched receipts would silently lose every confirmed slot number",
        );
        assert_eq!(
            r.confirmed_at,
            Some(34),
            "annotate_receipt must bind receipt.confirmed_at to confirmation.confirmed_at — the existing partial-coverage test does not check confirmed_at; a refactor that defaulted it to None would silently lose every confirmation timestamp",
        );
        assert_eq!(
            r.onchain_sig.as_deref(),
            Some("sig-fixture"),
            "annotate_receipt must bind receipt.onchain_sig to confirmation.tx_sig.clone() — the documented backwards-compatibility alias; older clients read onchain_sig and newer clients read tx_sig, and the daemon serves both forms from the same source value",
        );
        assert_eq!(
            r.tx_sig, r.onchain_sig,
            "annotate_receipt must keep receipt.tx_sig and receipt.onchain_sig bound to the SAME Option<String> value — the alias equality invariant. A refactor that introduced a separate 'onchain_sig' field on ChainConfirmation (e.g., for an L2-hash separation flow) and bound onchain_sig from there would split the two fields and break consumer code that grep-asserts tx_sig == onchain_sig for a confirmed receipt; pinning the equality contract anchors this against future split-binding refactors",
        );

        // Second case: tx_sig=None must propagate to BOTH tx_sig and
        // onchain_sig — the alias is preserved across the Option
        // variant so a confirmation that arrives without a transaction
        // signature surfaces both fields as None on the receipt.
        let mut none_receipt = receipt(8);
        let none_confirmation = ChainConfirmation {
            chain: "solana".to_string(),
            cluster: "devnet".to_string(),
            batch_id: "batch-2".to_string(),
            merkle_root: "root-2".to_string(),
            tx_sig: None,
            slot: None,
            confirmed_at: None,
        };
        annotate_receipt(&mut none_receipt, &none_confirmation);
        assert_eq!(
            none_receipt.tx_sig, None,
            "annotate_receipt must propagate confirmation.tx_sig=None to receipt.tx_sig=None — a refactor that defaulted tx_sig to Some(String::new()) would silently surface every signature-less confirmation as an empty-string sig",
        );
        assert_eq!(
            none_receipt.onchain_sig, None,
            "annotate_receipt must propagate confirmation.tx_sig=None to receipt.onchain_sig=None — the alias must be preserved across the Option::None variant; a refactor that bound onchain_sig from a different fallback source would split the two fields on this code path",
        );
        assert_eq!(
            none_receipt.tx_sig, none_receipt.onchain_sig,
            "the alias equality invariant must hold for the None case identically to the Some case — the alias is on the BINDING, not just on the Some-variant content",
        );
        assert_eq!(none_receipt.slot, None);
        assert_eq!(none_receipt.confirmed_at, None);
    }

    #[test]
    fn settlement_error_empty_batch_display_message_pins_exact_phrase_and_no_io_or_serde_prefix_convergence(
    ) {
        let err = SettlementError::EmptyBatch;
        let message = format!("{err}");
        assert_eq!(
            message, "no unsettled receipts",
            "SettlementError::EmptyBatch Display drifted (typo, pluralization swap, dropped qualifier, or prefix-convergence regression class)"
        );
        assert!(
            message.contains("unsettled"),
            "SettlementError::EmptyBatch must surface the 'unsettled' qualifier so audit-log filters can distinguish a fully-flushed-ledger no-op from a missing-receipt-log incident (dropped-qualifier regression class): {message}"
        );
        assert!(
            message.contains("receipts"),
            "SettlementError::EmptyBatch must surface the plural noun 'receipts' so operator-facing CLI documentation parity and incident-triage scrapers that grep for the literal phrase stay aligned (pluralization-swap regression class): {message}"
        );
        assert!(
            !message.starts_with("io:"),
            "SettlementError::EmptyBatch must not converge with SettlementError::Io prefix; a benign 'nothing to flush' must not be mis-routed as a disk-IO incident (prefix-convergence regression class): {message}"
        );
        assert!(
            !message.starts_with("serde:"),
            "SettlementError::EmptyBatch must not converge with SettlementError::Serde prefix; a benign 'nothing to flush' must not be mis-routed as a JSON-parse incident (prefix-convergence regression class): {message}"
        );
    }

    #[test]
    fn settlement_error_io_and_serde_display_messages_pin_prefix_and_external_source_display_delegation(
    ) {
        let io_err = SettlementError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "working.jsonl missing",
        ));
        let io_message = format!("{io_err}");
        assert!(
            io_message.starts_with("io: "),
            "SettlementError::Io must surface the literal 'io: ' bootstrap-stage prefix so audit-log filters can distinguish receipt-log disk faults from JSON-parse faults (dropped-prefix regression class): {io_message}"
        );
        assert!(
            io_message.contains("working.jsonl missing"),
            "SettlementError::Io must surface the inner std::io::Error Display rendering after the colon ({{0}}, not {{0:?}}); a Debug refactor would render 'Custom {{ kind: NotFound, error: ... }}' instead of the message payload (Debug-vs-Display formatting regression class on the {{0}} interpolation): {io_message}"
        );
        assert!(
            !io_message.contains("Custom {") && !io_message.contains("Os {"),
            "SettlementError::Io must NOT surface the std::io::Error Debug rendering; a Debug refactor on {{0}} would expose internal struct fields like 'Custom {{ kind: ..., error: ... }}' or 'Os {{ code: ..., kind: ..., message: ... }}' (Debug-vs-Display formatting regression class on the {{0}} interpolation): {io_message}"
        );

        let serde_source =
            serde_json::from_str::<serde_json::Value>("not json").expect_err("parse must fail");
        let serde_err = SettlementError::Serde(serde_source);
        let serde_message = format!("{serde_err}");
        assert!(
            serde_message.starts_with("serde: "),
            "SettlementError::Serde must surface the literal 'serde: ' bootstrap-stage prefix so audit-log filters can distinguish receipt-log JSON-parse faults from disk faults (dropped-prefix regression class): {serde_message}"
        );
        assert!(
            serde_message.contains("expected"),
            "SettlementError::Serde must surface the inner serde_json::Error Display rendering after the colon (serde_json renders parse failures with 'expected ...' Display strings); a Debug refactor on {{0}} would render 'Error(\"...\", line: N, column: M)' instead (Debug-vs-Display formatting regression class on the {{0}} interpolation): {serde_message}"
        );
        assert!(
            !serde_message.contains("Error("),
            "SettlementError::Serde must NOT surface the serde_json::Error Debug rendering; a Debug refactor on {{0}} would expose 'Error(\"...\", line: N, column: M)' buffer-position structs (Debug-vs-Display formatting regression class on the {{0}} interpolation): {serde_message}"
        );

        assert_ne!(
            io_message, serde_message,
            "SettlementError::Io and SettlementError::Serde Display must not converge; merging the two prefixes would lose the disk-fault vs JSON-parse-fault discriminator (prefix-convergence regression class): io={io_message} serde={serde_message}"
        );
        assert!(
            !io_message.starts_with("serde:") && !serde_message.starts_with("io:"),
            "SettlementError::Io must not start with 'serde:' and SettlementError::Serde must not start with 'io:'; a sibling-prefix swap would silently mis-route incident triage (sibling-prefix-swap regression class): io={io_message} serde={serde_message}"
        );
        assert_ne!(
            io_message, "no unsettled receipts",
            "SettlementError::Io must not converge with SettlementError::EmptyBatch; a disk-IO incident must not be mis-routed as a benign 'nothing to flush' no-op (EmptyBatch-convergence regression class): {io_message}"
        );
        assert_ne!(
            serde_message, "no unsettled receipts",
            "SettlementError::Serde must not converge with SettlementError::EmptyBatch; a JSON-parse incident must not be mis-routed as a benign 'nothing to flush' no-op (EmptyBatch-convergence regression class): {serde_message}"
        );
    }

    #[test]
    fn settlement_error_io_source_delegation_pin_returns_inner_std_io_error_via_std_error_source() {
        use std::error::Error;

        let inner = std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "settlement.jsonl: permission denied",
        );
        let expected_display = format!("{inner}");
        let err = SettlementError::Io(inner);
        let source = err.source().expect(
            "SettlementError::Io must surface the inner std::io::Error via std::error::Error::source so daemon-side settlement retry-policy classifiers can walk the error chain and downcast source() to std::io::Error to extract io::ErrorKind for distinct retry decisions (Interrupted/WouldBlock retry with backoff, PermissionDenied escalates to operator-attention, disk-full blocks settlement completely); a refactor that converted the variant from #[from] to a hand-written Error impl returning None (under a 'simpler error wrapping' rationale) would silently change source() to return None while leaving Display intact (dropped-source-attribute regression class)",
        );
        assert_eq!(
            format!("{source}"),
            expected_display,
            "SettlementError::Io source() Display must match a direct format!() of the same std::io::Error verbatim; a refactor that swapped the inner field type to Box<dyn Error + Send + Sync> would silently break daemon-side downcasts (concrete-source-type regression class)"
        );
        let kind = source.downcast_ref::<std::io::Error>().map(|e| e.kind());
        assert_eq!(
            kind,
            Some(std::io::ErrorKind::PermissionDenied),
            "SettlementError::Io source() must downcast_ref to std::io::Error so daemon-side settlement retry-policy classifiers can extract io::ErrorKind for retry decisions; a refactor that wrapped the inner in a project-local newtype (e.g., SettlementIoError(std::io::Error)) would silently break downcast_ref::<std::io::Error>() at every downstream settlement callsite (concrete-source-type downcast regression class)"
        );
    }

    #[test]
    fn settlement_error_serde_source_delegation_pin_returns_inner_serde_json_error_via_std_error_source(
    ) {
        use std::error::Error;

        let inner =
            serde_json::from_str::<serde_json::Value>("not json").expect_err("parse must fail");
        let expected_display = format!("{inner}");
        let err = SettlementError::Serde(inner);
        let source = err.source().expect(
            "SettlementError::Serde must surface the inner serde_json::Error via std::error::Error::source so daemon-side settlement diagnostics can walk the error chain and downcast source() to serde_json::Error to inspect line/column or classify() for malformed-receipt-row identification (line/column points the operator at the offending settlement.jsonl row, classify() distinguishes Syntax-vs-Data-vs-EOF for incident triage on a corrupted receipt row); a refactor that converted the variant from #[from] to a hand-written Error impl returning None (under a 'simpler error wrapping' rationale) would silently change source() to return None while leaving Display intact (dropped-source-attribute regression class)",
        );
        assert_eq!(
            format!("{source}"),
            expected_display,
            "SettlementError::Serde source() Display must match a direct format!() of the same serde_json::Error verbatim; a refactor that swapped the inner field type to Box<dyn Error + Send + Sync> would silently break daemon-side downcasts (concrete-source-type regression class)"
        );
        assert!(
            source.downcast_ref::<serde_json::Error>().is_some(),
            "SettlementError::Serde source() must downcast_ref to serde_json::Error so daemon-side settlement diagnostics can call serde_json::Error::line/column/classify for malformed-receipt-row identification; a refactor that wrapped the inner in a project-local newtype (e.g., SettlementSerdeError(serde_json::Error) under a 'consolidate parse errors into one Wire variant' rationale) would silently break downcast_ref::<serde_json::Error>() at every downstream settlement callsite that classifies settlement.jsonl parse faults (concrete-source-type downcast regression class)"
        );
    }
}
