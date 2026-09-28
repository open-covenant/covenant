//! The credit-side accrual ledger. Genuinely greenfield: every ledger in
//! the codebase debits (`covenant-budget::BudgetLedger`'s
//! `try_debit`/`would_exceed`/`tokens_remaining`,
//! `covenant-budget/src/lib.rs:235-282`) — nothing anywhere credits an
//! agent for work performed. This is the operator's earned balance:
//! one entry per completed job, `Unpaid` until the reconcile loop sees
//! the coordinator's payout confirmation for it in the operator books
//! (`GET /federation/operators/:operator/jobs`) and flips it to `Paid`
//! with the reported transaction signature.

use async_trait::async_trait;
use covenant_compute_protocol::{
    payout_memo_for, verify_payout_transaction, FundingSource, PayoutProof,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EarningsStatus {
    Unpaid,
    Paid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EarningsEntry {
    pub job_id: Uuid,
    /// What the operator is actually owed for this job: the coordinator's
    /// disclosed released gross net of the marketplace fee. For a lease
    /// that released gross is the metered draw, which sits below the
    /// receipt's window ceiling — so this is sized from what was released,
    /// never from the receipt price.
    pub amount_micro_usdc: u64,
    /// The disclosed fee withheld upstream — kept so the books explain
    /// themselves (gross = amount + fee). Zero on entries earned
    /// against a fee-free (or pre-fee-era) coordinator.
    #[serde(default)]
    pub fee_micro_usdc: u64,
    pub funding_source: FundingSource,
    pub status: EarningsStatus,
    pub earned_at_ms: u64,
    /// The on-chain transaction the coordinator reported for this
    /// job's payout push. `None` while unpaid — and on paid entries
    /// whose payout backend submitted nothing on-chain.
    #[serde(default)]
    pub paid_tx_signature: Option<String>,
    #[serde(default)]
    pub paid_at_ms: Option<u64>,
    /// This node's own signature over the receipt it submitted —
    /// journaled at credit time so the operator can later derive the
    /// payout memo the coordinator's transfer must carry, without
    /// keeping whole receipts around. `None` on entries credited
    /// before receipt journaling existed.
    #[serde(default)]
    pub receipt_signature_b58: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum EarningsError {
    #[error("job {0} already credited")]
    AlreadyCredited(Uuid),
    #[error("no earnings entry for job {0}")]
    NotFound(Uuid),
    /// The file-backed ledger could not make a mutation durable; the
    /// in-memory state was left unchanged.
    #[error("earnings ledger persistence: {0}")]
    Persist(String),
}

#[async_trait]
pub trait EarningsLedger: Send + Sync {
    /// Credits a completed job. Idempotent on `job_id`: crediting the
    /// same job twice is an error, not a double-credit, so a retried
    /// receipt submission can't double the operator's balance.
    async fn credit(&self, entry: EarningsEntry) -> Result<(), EarningsError>;
    /// Settles a credited job against the coordinator's payout
    /// confirmation. Returns `true` when this call flipped the entry
    /// to `Paid`, `false` when it already was — the reconcile loop
    /// re-reads the same books every tick, so an already-paid entry is
    /// a no-op (nothing journaled), not an error.
    async fn mark_paid(
        &self,
        job_id: Uuid,
        tx_signature: Option<String>,
        paid_at_ms: u64,
    ) -> Result<bool, EarningsError>;
    async fn unpaid_total_micro_usdc(&self) -> u64;
    async fn recent(&self, limit: usize) -> Vec<EarningsEntry>;
    /// Whether this job already holds a credited entry. Boot recovery
    /// consults it to skip re-executing a job whose result a prior life
    /// already delivered and booked: the outbox-pending check alone
    /// misses a job the same boot's earlier drain already delivered,
    /// credited, and dequeued.
    async fn is_credited(&self, job_id: Uuid) -> bool;
}

#[derive(Default)]
pub struct InMemoryEarningsLedger {
    entries: Mutex<Vec<EarningsEntry>>,
}

impl InMemoryEarningsLedger {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl EarningsLedger for InMemoryEarningsLedger {
    async fn credit(&self, entry: EarningsEntry) -> Result<(), EarningsError> {
        let mut guard = self.entries.lock();
        if guard.iter().any(|e| e.job_id == entry.job_id) {
            return Err(EarningsError::AlreadyCredited(entry.job_id));
        }
        guard.push(entry);
        Ok(())
    }

    async fn mark_paid(
        &self,
        job_id: Uuid,
        tx_signature: Option<String>,
        paid_at_ms: u64,
    ) -> Result<bool, EarningsError> {
        let mut guard = self.entries.lock();
        let entry = guard
            .iter_mut()
            .find(|e| e.job_id == job_id)
            .ok_or(EarningsError::NotFound(job_id))?;
        if entry.status == EarningsStatus::Paid {
            return Ok(false);
        }
        entry.status = EarningsStatus::Paid;
        entry.paid_tx_signature = tx_signature;
        entry.paid_at_ms = Some(paid_at_ms);
        Ok(true)
    }

    async fn unpaid_total_micro_usdc(&self) -> u64 {
        self.entries
            .lock()
            .iter()
            .filter(|e| e.status == EarningsStatus::Unpaid)
            .map(|e| e.amount_micro_usdc)
            .sum()
    }

    async fn recent(&self, limit: usize) -> Vec<EarningsEntry> {
        let guard = self.entries.lock();
        guard.iter().rev().take(limit).cloned().collect()
    }

    async fn is_credited(&self, job_id: Uuid) -> bool {
        self.entries.lock().iter().any(|e| e.job_id == job_id)
    }
}

/// File-backed ledger: append-only JSONL of whole-entry upserts keyed
/// by `job_id` (same idiom as the coordinator's journal and this
/// node's own audit log), replayed at open — a node restart must not
/// zero the operator's earnings record. `credit` appends the entry,
/// `mark_paid` appends the entry again with its new status; replay
/// applies lines in order, updating in place, so the restored ledger
/// is exactly the in-memory one. Mutation journals before memory
/// commits and fails loudly if the write can't be made durable.
///
/// Every record the writer produces ends in `\n`, so trailing bytes
/// after the last newline can only be a torn write from a crash
/// mid-append: they are dropped AND truncated away at open (that
/// mutation was reported failed; truncating stops the next append
/// from gluing onto the garbage). A newline-terminated line that
/// fails to parse is real damage and refuses to open.
pub struct JsonlEarningsLedger {
    entries: Mutex<Vec<EarningsEntry>>,
    file: Option<Mutex<std::fs::File>>,
}

impl JsonlEarningsLedger {
    pub fn open(path: &std::path::Path) -> Result<Self, EarningsError> {
        let persist = |e: std::io::Error| EarningsError::Persist(e.to_string());

        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(persist(e)),
        };
        let valid_len = valid_prefix_len(&bytes);
        if valid_len < bytes.len() {
            tracing::warn!(
                dropped_bytes = bytes.len() - valid_len,
                "truncating torn final earnings record"
            );
            let f = std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .map_err(persist)?;
            f.set_len(valid_len as u64).map_err(persist)?;
        }

        let entries = replay(&bytes[..valid_len])?;

        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(persist)?;
        Ok(Self {
            entries: Mutex::new(entries),
            file: Some(Mutex::new(file)),
        })
    }

    /// Replays the ledger into memory WITHOUT touching the file — no
    /// torn-tail truncation, no append handle. This is what a `status`,
    /// `earnings`, or `earnings verify` read must use: an operator runs
    /// those against the same home a live `serve` process is appending
    /// to, and [`JsonlEarningsLedger::open`]'s torn-tail truncation would
    /// race that node, cutting the row it is mid-append into. A read-only
    /// ledger has no write handle, so `credit`/`mark_paid` on it fail
    /// loudly rather than silently dropping a row.
    pub fn read_only(path: &std::path::Path) -> Result<Self, EarningsError> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(EarningsError::Persist(e.to_string())),
        };
        let entries = replay(&bytes[..valid_prefix_len(&bytes)])?;
        Ok(Self {
            entries: Mutex::new(entries),
            file: None,
        })
    }

    fn append(&self, entry: &EarningsEntry) -> Result<(), EarningsError> {
        use std::io::Write;
        let Some(file) = &self.file else {
            return Err(EarningsError::Persist(
                "earnings ledger opened read-only".into(),
            ));
        };
        let persist = |e: std::io::Error| EarningsError::Persist(e.to_string());
        let mut line =
            serde_json::to_vec(entry).map_err(|e| EarningsError::Persist(e.to_string()))?;
        line.push(b'\n');
        let mut file = file.lock();
        file.write_all(&line).map_err(persist)?;
        // fdatasync, not flush: a credited row is the operator's claim
        // to pay and must survive a power loss, not just a process kill.
        file.sync_data().map_err(persist)
    }
}

/// Bytes up to and including the last newline; everything after it is a
/// torn final record the writer never terminated.
fn valid_prefix_len(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .rposition(|&b| b == b'\n')
        .map(|p| p + 1)
        .unwrap_or(0)
}

/// Parses newline-terminated journal bytes into the ledger's entries,
/// last write winning per job id. Shared by [`JsonlEarningsLedger::open`]
/// (which heals the file first) and [`JsonlEarningsLedger::read_only`]
/// (which touches nothing on disk), so both replay identically.
fn replay(valid: &[u8]) -> Result<Vec<EarningsEntry>, EarningsError> {
    let mut entries: Vec<EarningsEntry> = Vec::new();
    for (i, line) in valid.split(|&b| b == b'\n').enumerate() {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let entry: EarningsEntry = serde_json::from_slice(line)
            .map_err(|e| EarningsError::Persist(format!("line {} is corrupt: {e}", i + 1)))?;
        match entries.iter_mut().find(|e| e.job_id == entry.job_id) {
            Some(existing) => *existing = entry,
            None => entries.push(entry),
        }
    }
    Ok(entries)
}

#[async_trait]
impl EarningsLedger for JsonlEarningsLedger {
    async fn credit(&self, entry: EarningsEntry) -> Result<(), EarningsError> {
        let mut guard = self.entries.lock();
        if guard.iter().any(|e| e.job_id == entry.job_id) {
            return Err(EarningsError::AlreadyCredited(entry.job_id));
        }
        self.append(&entry)?;
        guard.push(entry);
        Ok(())
    }

    async fn mark_paid(
        &self,
        job_id: Uuid,
        tx_signature: Option<String>,
        paid_at_ms: u64,
    ) -> Result<bool, EarningsError> {
        let mut guard = self.entries.lock();
        let entry = guard
            .iter_mut()
            .find(|e| e.job_id == job_id)
            .ok_or(EarningsError::NotFound(job_id))?;
        if entry.status == EarningsStatus::Paid {
            return Ok(false);
        }
        let paid = EarningsEntry {
            status: EarningsStatus::Paid,
            paid_tx_signature: tx_signature,
            paid_at_ms: Some(paid_at_ms),
            ..entry.clone()
        };
        self.append(&paid)?;
        *entry = paid;
        Ok(true)
    }

    async fn unpaid_total_micro_usdc(&self) -> u64 {
        self.entries
            .lock()
            .iter()
            .filter(|e| e.status == EarningsStatus::Unpaid)
            .map(|e| e.amount_micro_usdc)
            .sum()
    }

    async fn recent(&self, limit: usize) -> Vec<EarningsEntry> {
        let guard = self.entries.lock();
        guard.iter().rev().take(limit).cloned().collect()
    }

    async fn is_credited(&self, job_id: Uuid) -> bool {
        self.entries.lock().iter().any(|e| e.job_id == job_id)
    }
}

/// How long a payout confirmation with no matching credit is left alone
/// before it is treated as a dropped credit and backfilled. A live serve
/// loop books a credit within microseconds of the coordinator recording
/// the payout, so a confirmation younger than this is a credit still in
/// flight (or racing this very tick) that `mark_paid` will flip once it
/// lands; only one this old belongs to a credit a crash lost for good.
/// Five default reconcile ticks — far past the race window, far short of
/// leaving a real gap unreported.
const BACKFILL_GRACE_MS: u64 = 5 * 60_000;

/// Applies one fetch of the coordinator's payout-confirmation feed
/// (`GET /federation/operators/:operator/jobs`) to this ledger: every
/// row carrying a payout flips its entry to `Paid` with the reported
/// signature; a completed-but-unpushed row leaves its entry `Unpaid`
/// until a later feed confirms the push. A confirmed payout the ledger
/// has no entry for is normally a credit still landing and is left for
/// the next tick, but once it is older than [`BACKFILL_GRACE_MS`] it is a
/// credit a crash dropped between the result's submission and its local
/// booking — reconstructed here as `Paid` from the coordinator's books,
/// so the operator's earnings stop under-reporting money the chain
/// already moved. Returns how many entries this pass flipped or
/// backfilled — zero on a quiet tick.
pub async fn reconcile_paid_rows(
    ledger: &dyn EarningsLedger,
    rows: &[crate::http_client::OperatorJobRow],
    now_ms: u64,
) -> usize {
    let mut flipped = 0;
    for row in rows {
        let Some(payout) = &row.payout else { continue };
        match ledger
            .mark_paid(
                row.job_id,
                payout.tx_signature.clone(),
                payout.recorded_at_ms,
            )
            .await
        {
            Ok(true) => {
                flipped += 1;
                tracing::info!(
                    job_id = %row.job_id,
                    amount_micro_usdc = payout.amount_micro_usdc,
                    tx_signature = payout.tx_signature.as_deref().unwrap_or("-"),
                    "payout confirmed by the coordinator books"
                );
            }
            Ok(false) => {}
            // A confirmed payout for a job this ledger has no entry for.
            // Within the grace it is a credit still in flight (the common
            // case, and the credit-races-its-confirmation window) — leave
            // it for the next tick. Past the grace the credit was lost to
            // a crash and is never coming: reconstruct the paid row so the
            // operator's earnings match the coordinator's books. The
            // node's own receipt signature died with the credit, so this
            // row can't be re-audited on-chain; the reported tx signature
            // is the operator's handle to verify it.
            Err(EarningsError::NotFound(_)) => {
                if now_ms.saturating_sub(payout.recorded_at_ms) < BACKFILL_GRACE_MS {
                    continue;
                }
                let backfilled = EarningsEntry {
                    job_id: row.job_id,
                    amount_micro_usdc: row.net_micro_usdc,
                    fee_micro_usdc: row.fee_micro_usdc,
                    funding_source: row.funding_source,
                    status: EarningsStatus::Paid,
                    earned_at_ms: row.issued_at_ms,
                    paid_tx_signature: payout.tx_signature.clone(),
                    paid_at_ms: Some(payout.recorded_at_ms),
                    receipt_signature_b58: None,
                };
                match ledger.credit(backfilled).await {
                    Ok(()) => {
                        flipped += 1;
                        tracing::warn!(
                            job_id = %row.job_id,
                            amount_micro_usdc = row.net_micro_usdc,
                            tx_signature = payout.tx_signature.as_deref().unwrap_or("-"),
                            "backfilled a paid job the local ledger had lost — a crash likely \
                             fell between its result and its credit"
                        );
                    }
                    // A credit arrived first (it wins, keeping its receipt
                    // signature) or the write failed: either way the next
                    // tick reconciles it.
                    Err(e) => tracing::debug!(
                        job_id = %row.job_id,
                        error = %e,
                        "backfill skipped; a credit landed or persistence failed"
                    ),
                }
            }
            Err(e) => tracing::debug!(
                job_id = %row.job_id,
                error = %e,
                "payout confirmation could not be applied"
            ),
        }
    }
    flipped
}

/// Why a Paid row failed its on-chain audit. Everything here is a
/// contradiction of the coordinator's books except
/// [`NoReceiptSignature`](EarningsAuditError::NoReceiptSignature),
/// which only says this entry predates receipt journaling and cannot
/// be audited.
#[derive(Debug, thiserror::Error)]
pub enum EarningsAuditError {
    #[error("no receipt signature journaled — this entry predates receipt journaling")]
    NoReceiptSignature,
    #[error("{0}")]
    Chain(String),
    #[error("the chain moved {onchain} base units but this ledger credited {credited}")]
    AmountMismatch { onchain: u64, credited: u64 },
    #[error("the chain paid {recipient}, not this operator's payout wallet {own_wallet}")]
    WrongRecipient {
        recipient: String,
        own_wallet: String,
    },
}

/// Holds one Paid row to the chain's own record: the fetched
/// transaction must carry exactly this entry's receipt-derived memo,
/// have moved exactly the net amount the ledger credited, and — when
/// the operator passes their own payout wallet — have paid THIS
/// operator. Pure; the caller fetches `tx` from an RPC endpoint the
/// operator picked (never the coordinator, whose pick could vouch for
/// its own transfers).
pub fn audit_paid_entry(
    entry: &EarningsEntry,
    tx: &serde_json::Value,
    own_wallet: Option<&str>,
) -> Result<PayoutProof, EarningsAuditError> {
    let Some(signature) = entry.receipt_signature_b58.as_deref() else {
        return Err(EarningsAuditError::NoReceiptSignature);
    };
    let memo = payout_memo_for(entry.job_id, signature);
    let proof = verify_payout_transaction(&memo, tx)
        .map_err(|e| EarningsAuditError::Chain(e.to_string()))?;
    if proof.amount_micro_usdc != entry.amount_micro_usdc {
        return Err(EarningsAuditError::AmountMismatch {
            onchain: proof.amount_micro_usdc,
            credited: entry.amount_micro_usdc,
        });
    }
    if let Some(own_wallet) = own_wallet {
        if proof.recipient_owner_b58 != own_wallet {
            return Err(EarningsAuditError::WrongRecipient {
                recipient: proof.recipient_owner_b58,
                own_wallet: own_wallet.into(),
            });
        }
    }
    Ok(proof)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(job_id: Uuid, amount: u64) -> EarningsEntry {
        EarningsEntry {
            job_id,
            amount_micro_usdc: amount,
            fee_micro_usdc: 0,
            funding_source: FundingSource::Organic,
            status: EarningsStatus::Unpaid,
            earned_at_ms: 1,
            paid_tx_signature: None,
            paid_at_ms: None,
            receipt_signature_b58: Some("receipt-sig".into()),
        }
    }

    #[tokio::test]
    async fn credit_then_unpaid_total_reflects_it() {
        let ledger = InMemoryEarningsLedger::new();
        let job_id = Uuid::new_v4();
        ledger.credit(entry(job_id, 1_000)).await.unwrap();
        assert_eq!(ledger.unpaid_total_micro_usdc().await, 1_000);
    }

    #[tokio::test]
    async fn credit_is_not_idempotent_double_credit_is_an_error() {
        let ledger = InMemoryEarningsLedger::new();
        let job_id = Uuid::new_v4();
        ledger.credit(entry(job_id, 1_000)).await.unwrap();
        let err = ledger.credit(entry(job_id, 1_000)).await.unwrap_err();
        assert!(matches!(err, EarningsError::AlreadyCredited(id) if id == job_id));
        assert_eq!(
            ledger.unpaid_total_micro_usdc().await,
            1_000,
            "a rejected duplicate credit must not double the balance"
        );
    }

    #[tokio::test]
    async fn mark_paid_moves_it_out_of_unpaid_total_and_pins_the_signature() {
        let ledger = InMemoryEarningsLedger::new();
        let job_id = Uuid::new_v4();
        ledger.credit(entry(job_id, 1_000)).await.unwrap();
        let flipped = ledger
            .mark_paid(job_id, Some("devnet-sig".into()), 99)
            .await
            .unwrap();
        assert!(flipped);
        assert_eq!(ledger.unpaid_total_micro_usdc().await, 0);
        let recent = ledger.recent(10).await;
        assert_eq!(recent[0].status, EarningsStatus::Paid);
        assert_eq!(recent[0].paid_tx_signature.as_deref(), Some("devnet-sig"));
        assert_eq!(recent[0].paid_at_ms, Some(99));
    }

    #[tokio::test]
    async fn mark_paid_again_is_a_no_op_not_an_overwrite() {
        let ledger = InMemoryEarningsLedger::new();
        let job_id = Uuid::new_v4();
        ledger.credit(entry(job_id, 1_000)).await.unwrap();
        assert!(ledger
            .mark_paid(job_id, Some("first-sig".into()), 99)
            .await
            .unwrap());
        // The reconcile loop re-reads the same books every tick; the
        // second sighting must neither error nor clobber the record.
        assert!(!ledger
            .mark_paid(job_id, Some("other-sig".into()), 100)
            .await
            .unwrap());
        let recent = ledger.recent(10).await;
        assert_eq!(recent[0].paid_tx_signature.as_deref(), Some("first-sig"));
        assert_eq!(recent[0].paid_at_ms, Some(99));
    }

    #[tokio::test]
    async fn mark_paid_unknown_job_errors() {
        let ledger = InMemoryEarningsLedger::new();
        let err = ledger.mark_paid(Uuid::new_v4(), None, 1).await.unwrap_err();
        assert!(matches!(err, EarningsError::NotFound(_)));
    }

    #[tokio::test]
    async fn jsonl_ledger_survives_a_reopen_with_statuses_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("earnings.jsonl");
        let paid = Uuid::new_v4();
        let unpaid = Uuid::new_v4();

        {
            let ledger = JsonlEarningsLedger::open(&path).unwrap();
            ledger.credit(entry(paid, 1_000)).await.unwrap();
            ledger.credit(entry(unpaid, 250)).await.unwrap();
            assert!(ledger
                .mark_paid(paid, Some("devnet-sig".into()), 7)
                .await
                .unwrap());
            // Already paid: nothing flips, and (checked below via the
            // file's line count) nothing is journaled either.
            assert!(!ledger.mark_paid(paid, None, 8).await.unwrap());
            assert_eq!(ledger.unpaid_total_micro_usdc().await, 250);
        }

        assert_eq!(
            std::fs::read_to_string(&path).unwrap().lines().count(),
            3,
            "two credits plus one paid upsert — the no-op re-mark must not append"
        );

        let ledger = JsonlEarningsLedger::open(&path).unwrap();
        assert_eq!(ledger.unpaid_total_micro_usdc().await, 250);
        let recent = ledger.recent(10).await;
        assert_eq!(recent.len(), 2);
        // Same order semantics as the in-memory ledger: the payout
        // updated the first entry in place, it did not reorder.
        assert_eq!(recent[0].job_id, unpaid);
        assert_eq!(recent[1].job_id, paid);
        assert_eq!(recent[1].status, EarningsStatus::Paid);
        assert_eq!(recent[1].paid_tx_signature.as_deref(), Some("devnet-sig"));
        assert_eq!(recent[1].paid_at_ms, Some(7));

        // And the reopened ledger still refuses a double credit.
        assert!(matches!(
            ledger.credit(entry(paid, 1_000)).await,
            Err(EarningsError::AlreadyCredited(id)) if id == paid
        ));
    }

    #[tokio::test]
    async fn a_torn_final_record_is_truncated_away_and_never_poisons_later_appends() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("earnings.jsonl");
        let first = Uuid::new_v4();
        {
            let ledger = JsonlEarningsLedger::open(&path).unwrap();
            ledger.credit(entry(first, 750)).await.unwrap();
        }

        // Crash mid-append: trailing bytes with no newline.
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(b"{\"job_id\":\"trunca").unwrap();
        }

        // Reopen drops AND truncates the torn tail, so the next credit
        // starts a fresh line instead of gluing onto garbage.
        let second = Uuid::new_v4();
        {
            let ledger = JsonlEarningsLedger::open(&path).unwrap();
            assert_eq!(ledger.unpaid_total_micro_usdc().await, 750);
            ledger.credit(entry(second, 250)).await.unwrap();
        }
        let ledger = JsonlEarningsLedger::open(&path).unwrap();
        assert_eq!(ledger.unpaid_total_micro_usdc().await, 1_000);
    }

    #[tokio::test]
    async fn read_only_replays_the_ledger_without_truncating_a_torn_tail() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("earnings.jsonl");
        let paid_job = Uuid::new_v4();
        {
            let ledger = JsonlEarningsLedger::open(&path).unwrap();
            ledger.credit(entry(paid_job, 750)).await.unwrap();
        }
        // A row a live writer left mid-append. `open` would truncate it,
        // racing that writer; a read beside a live node must not.
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(b"{\"job_id\":\"torn").unwrap();
        }
        let before = std::fs::read(&path).unwrap();

        let view = JsonlEarningsLedger::read_only(&path).unwrap();
        assert_eq!(view.unpaid_total_micro_usdc().await, 750);
        assert_eq!(view.recent(10).await[0].job_id, paid_job);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "a read-only replay must leave the live writer's file byte-for-byte intact"
        );

        // No write handle: a mutation fails loudly rather than silently
        // dropping the operator's claim to pay.
        assert!(matches!(
            view.credit(entry(Uuid::new_v4(), 1)).await,
            Err(EarningsError::Persist(_))
        ));
    }

    #[tokio::test]
    async fn a_corrupt_complete_line_refuses_to_open() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("earnings.jsonl");
        {
            let ledger = JsonlEarningsLedger::open(&path).unwrap();
            ledger.credit(entry(Uuid::new_v4(), 750)).await.unwrap();
        }

        // Newline-terminated garbage is not a torn write — the writer
        // only ever produces complete records, so this is damage.
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(b"{\"job_id\":\"not-earnings\"}\n").unwrap();
        }
        assert!(matches!(
            JsonlEarningsLedger::open(&path),
            Err(EarningsError::Persist(_))
        ));
    }

    #[tokio::test]
    async fn reconcile_flips_exactly_the_rows_with_payouts() {
        use crate::http_client::{OperatorJobRow, PayoutConfirmation};

        let ledger = InMemoryEarningsLedger::new();
        let pushed = Uuid::new_v4();
        let pending = Uuid::new_v4();
        ledger.credit(entry(pushed, 900)).await.unwrap();
        ledger.credit(entry(pending, 400)).await.unwrap();

        let row = |job_id, payout| OperatorJobRow {
            job_id,
            status: "completed".into(),
            refund_reason: None,
            disputed: false,
            dispute_reason: None,
            price_micro_usdc: 1_000,
            fee_micro_usdc: 100,
            net_micro_usdc: 900,
            funding_source: FundingSource::Organic,
            issued_at_ms: 1,
            payout,
        };
        // Fresh confirmations: within the backfill grace, so the unknown
        // job stays skipped rather than reconstructed.
        let now = 100;
        let rows = vec![
            row(
                pushed,
                Some(PayoutConfirmation {
                    amount_micro_usdc: 900,
                    tx_signature: Some("tx-sig".into()),
                    recorded_at_ms: 42,
                }),
            ),
            row(pending, None),
            // The feed can name jobs this ledger never credited (a
            // fresh ledger file) — reconcile skips them, no crash.
            row(
                Uuid::new_v4(),
                Some(PayoutConfirmation {
                    amount_micro_usdc: 1,
                    tx_signature: None,
                    recorded_at_ms: 43,
                }),
            ),
        ];

        assert_eq!(reconcile_paid_rows(&ledger, &rows, now).await, 1);
        assert_eq!(
            ledger.unpaid_total_micro_usdc().await,
            400,
            "the payout-less row must stay unpaid"
        );
        let recent = ledger.recent(10).await;
        let paid = recent.iter().find(|e| e.job_id == pushed).unwrap();
        assert_eq!(paid.status, EarningsStatus::Paid);
        assert_eq!(paid.paid_tx_signature.as_deref(), Some("tx-sig"));
        assert_eq!(paid.paid_at_ms, Some(42));

        // The next tick re-reads the same books: nothing flips again.
        assert_eq!(reconcile_paid_rows(&ledger, &rows, now).await, 0);
    }

    #[tokio::test]
    async fn an_aged_confirmation_for_a_dropped_credit_backfills_a_paid_row() {
        use crate::http_client::{OperatorJobRow, PayoutConfirmation};

        // A crash fell between a result's submission and its local credit,
        // so the coordinator paid a job this ledger never recorded. Once
        // the confirmation is well past the grace, reconcile reconstructs
        // it as Paid from the coordinator's books, and the operator's
        // earnings stop under-reporting money the chain already moved.
        let ledger = InMemoryEarningsLedger::new();
        let job_id = Uuid::new_v4();
        let rows = vec![OperatorJobRow {
            job_id,
            status: "completed".into(),
            refund_reason: None,
            disputed: false,
            dispute_reason: None,
            price_micro_usdc: 1_000,
            fee_micro_usdc: 100,
            net_micro_usdc: 900,
            funding_source: FundingSource::Bootstrap,
            issued_at_ms: 5,
            payout: Some(PayoutConfirmation {
                amount_micro_usdc: 900,
                tx_signature: Some("dropped-sig".into()),
                recorded_at_ms: 1_000,
            }),
        }];
        let now = 1_000 + 10 * 60_000; // far past BACKFILL_GRACE_MS

        assert_eq!(
            reconcile_paid_rows(&ledger, &rows, now).await,
            1,
            "the lost credit is backfilled"
        );
        let recent = ledger.recent(10).await;
        assert_eq!(recent.len(), 1);
        let row = &recent[0];
        assert_eq!(row.job_id, job_id);
        assert_eq!(row.status, EarningsStatus::Paid);
        assert_eq!(row.amount_micro_usdc, 900);
        assert_eq!(row.fee_micro_usdc, 100);
        assert_eq!(row.funding_source, FundingSource::Bootstrap);
        assert_eq!(row.paid_tx_signature.as_deref(), Some("dropped-sig"));
        assert_eq!(row.paid_at_ms, Some(1_000));
        assert_eq!(
            row.receipt_signature_b58, None,
            "the receipt signature died with the credit — unauditable but honest"
        );
        assert_eq!(
            ledger.unpaid_total_micro_usdc().await,
            0,
            "a backfilled row is already Paid, not owed"
        );

        // Idempotent: the next tick re-reads the same books, flips nothing.
        assert_eq!(reconcile_paid_rows(&ledger, &rows, now).await, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_confirmation_racing_its_own_credit_heals_on_the_next_tick() {
        use crate::http_client::{OperatorJobRow, PayoutConfirmation};

        // The serve loop credits and the reconcile loop ticks
        // concurrently in the binary, so a payout confirmation can
        // reach the ledger before the credit it confirms. That tick
        // must skip it (no phantom entry) and the next tick must flip
        // it — the confirmation is never lost, the balance never
        // doubles.
        let job_id = Uuid::new_v4();
        let rows = vec![OperatorJobRow {
            job_id,
            status: "completed".into(),
            refund_reason: None,
            disputed: false,
            dispute_reason: None,
            price_micro_usdc: 1_000,
            fee_micro_usdc: 100,
            net_micro_usdc: 900,
            funding_source: FundingSource::Organic,
            issued_at_ms: 1,
            payout: Some(PayoutConfirmation {
                amount_micro_usdc: 900,
                tx_signature: Some("race-sig".into()),
                recorded_at_ms: 7,
            }),
        }];
        // The confirmation is fresh (recorded 7ms into the epoch, read at
        // 100ms), so it is inside the backfill grace: a racing credit must
        // still own the entry, never a backfilled phantom.
        let now = 100;

        // The deterministic worst case: the confirmation strictly
        // beats the credit.
        let ledger = InMemoryEarningsLedger::new();
        assert_eq!(reconcile_paid_rows(&ledger, &rows, now).await, 0);
        assert!(
            ledger.recent(10).await.is_empty(),
            "a fresh confirmation alone must not mint an entry"
        );
        ledger.credit(entry(job_id, 900)).await.unwrap();
        assert_eq!(
            reconcile_paid_rows(&ledger, &rows, now).await,
            1,
            "the tick after the credit lands must flip it"
        );

        // And every interleaving in between, both spawn orders.
        for i in 0..50u32 {
            let ledger = std::sync::Arc::new(InMemoryEarningsLedger::new());
            let credit_task = {
                let ledger = ledger.clone();
                move || tokio::spawn(async move { ledger.credit(entry(job_id, 900)).await })
            };
            let reconcile_task = {
                let ledger = ledger.clone();
                let rows = rows.clone();
                move || {
                    tokio::spawn(
                        async move { reconcile_paid_rows(ledger.as_ref(), &rows, now).await },
                    )
                }
            };
            let (crediting, reconciling) = if i % 2 == 0 {
                let c = credit_task();
                (c, reconcile_task())
            } else {
                let r = reconcile_task();
                (credit_task(), r)
            };
            crediting.await.unwrap().unwrap();
            let first_pass = reconciling.await.unwrap();

            let heal_pass = reconcile_paid_rows(ledger.as_ref(), &rows, now).await;
            assert_eq!(
                first_pass + heal_pass,
                1,
                "exactly one flip across the racing tick and the heal tick"
            );
            let recent = ledger.recent(10).await;
            assert_eq!(recent.len(), 1, "no phantom entry either way");
            assert_eq!(recent[0].status, EarningsStatus::Paid);
            assert_eq!(recent[0].paid_tx_signature.as_deref(), Some("race-sig"));
            assert_eq!(ledger.unpaid_total_micro_usdc().await, 0);
        }
    }

    #[test]
    fn journal_rows_from_before_receipt_journaling_still_parse() {
        let row: EarningsEntry = serde_json::from_str(
            r#"{"job_id":"a3bb189e-8bf9-3888-9912-ace4e6543002","amount_micro_usdc":900,
                "funding_source":"organic","status":"paid","earned_at_ms":5,
                "paid_tx_signature":"sig","paid_at_ms":9}"#,
        )
        .expect("old-format row");
        assert_eq!(row.receipt_signature_b58, None);
        assert!(matches!(
            audit_paid_entry(&row, &serde_json::Value::Null, None),
            Err(EarningsAuditError::NoReceiptSignature)
        ));
    }

    fn paid_tx(memo: &str, recipient: &str, amount: u64) -> serde_json::Value {
        serde_json::json!({
            "meta": {
                "err": null,
                "preTokenBalances": [
                    { "owner": recipient, "mint": "m1nt", "uiTokenAmount": { "amount": "0" } },
                ],
                "postTokenBalances": [
                    { "owner": recipient, "mint": "m1nt",
                      "uiTokenAmount": { "amount": amount.to_string() } },
                ],
            },
            "transaction": { "message": { "instructions": [
                { "program": "spl-memo", "parsed": memo },
            ] } },
        })
    }

    #[test]
    fn audit_accepts_the_exact_payment_the_books_claim() {
        let mut row = entry(Uuid::new_v4(), 1_000);
        row.status = EarningsStatus::Paid;
        let memo = payout_memo_for(row.job_id, "receipt-sig");
        let tx = paid_tx(&memo, "operator-wallet", 1_000);
        let proof = audit_paid_entry(&row, &tx, Some("operator-wallet")).expect("audit");
        assert_eq!(proof.amount_micro_usdc, 1_000);
        assert_eq!(proof.recipient_owner_b58, "operator-wallet");
    }

    #[test]
    fn audit_contradicts_a_short_payment_a_wrong_recipient_and_foreign_work() {
        let mut row = entry(Uuid::new_v4(), 1_000);
        row.status = EarningsStatus::Paid;
        let memo = payout_memo_for(row.job_id, "receipt-sig");

        let short = paid_tx(&memo, "operator-wallet", 999);
        assert!(matches!(
            audit_paid_entry(&row, &short, None),
            Err(EarningsAuditError::AmountMismatch {
                onchain: 999,
                credited: 1_000
            })
        ));

        let elsewhere = paid_tx(&memo, "someone-else", 1_000);
        assert!(matches!(
            audit_paid_entry(&row, &elsewhere, Some("operator-wallet")),
            Err(EarningsAuditError::WrongRecipient { .. })
        ));
        // Without the operator's wallet configured the recipient is
        // whatever the chain says — the memo still pins the work.
        assert!(audit_paid_entry(&row, &elsewhere, None).is_ok());

        let foreign = paid_tx(
            &payout_memo_for(Uuid::new_v4(), "other-sig"),
            "operator-wallet",
            1_000,
        );
        let err = audit_paid_entry(&row, &foreign, None).unwrap_err();
        assert!(matches!(err, EarningsAuditError::Chain(_)), "got: {err}");
    }
}
