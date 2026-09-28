//! Durable book of idempotent compute purchases — the demand side's
//! twin of the node's accepted-jobs book.
//!
//! A buyer process that dies mid-purchase leaves the agent that asked
//! with nothing but the urge to retry, and a naive retry signs a fresh
//! envelope: a second job, a second hold, a second payment for one
//! intent. The book closes that window. A purchase made under an
//! idempotency key journals its signed envelope here *before* the
//! first submission, so every retry — same process or the next one —
//! re-drives the exact same bytes into the coordinator's duplicate
//! detection instead of minting a new job.
//!
//! Entry lifecycle: `record` (envelope journaled, purchase in flight)
//! → either `settle` (the result was returned and the spend booked;
//! the entry stays, replays answer from it without paying again) or
//! `void` (the job concluded unpaid — refunded, rejected, failed — so
//! the key frees and a later call may honestly re-buy). Keys are
//! payer-scoped by the caller; the book never interprets them.
//!
//! Same JSONL idiom as the node's ledgers: every state change appends
//! the full entry, replay keeps the last state per key and drops the
//! voided ones, a torn final line is truncated, and boot compacts the
//! accumulated history back to one line per live key.

use covenant_compute_protocol::{JobKind, SignedJobEnvelope};
use parking_lot::Mutex;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum PurchaseError {
    #[error("persist: {0}")]
    Persist(String),
    #[error("a purchase under key {0} is already in flight")]
    InFlight(String),
}

/// One idempotent purchase: the signed envelope is the whole work
/// order, and re-submitting it verbatim is what makes retries safe.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PurchaseEntry {
    pub key: String,
    pub envelope: SignedJobEnvelope,
    pub opened_at_ms: u64,
    /// The spend-side receipt id, set by `settle` once the result has
    /// been returned and the debit booked — a replay from then on
    /// serves the recorded purchase instead of paying again.
    #[serde(default)]
    pub receipt_id: Option<Uuid>,
    #[serde(default)]
    pub voided: bool,
}

impl PurchaseEntry {
    pub fn settled(&self) -> bool {
        self.receipt_id.is_some()
    }

    /// Names the first explicit argument that contradicts this
    /// journaled purchase, if any. A reused key means "this purchase":
    /// omitted arguments inherit it (a re-drive submits the journaled
    /// envelope verbatim anyway), but an explicit argument that says
    /// something else must refuse loudly — silently answering the old
    /// purchase would hand an agent a stale result for a question it
    /// never asked, billed as if it had.
    #[allow(clippy::too_many_arguments)]
    pub fn conflicting_argument(
        &self,
        kind: JobKind,
        input: &[covenant_mcp::Content],
        model: Option<&str>,
        gpu_class: Option<&str>,
        min_vram_gb: Option<u32>,
        min_reputation_bps: Option<u32>,
        price_micro_usdc: Option<u64>,
        deadline_ms: Option<u64>,
    ) -> Option<&'static str> {
        let p = &self.envelope.payload;
        if p.kind != kind {
            return Some("kind");
        }
        if p.input != input {
            return Some("input");
        }
        if model.is_some() && p.capability_requirement.model_id.as_deref() != model {
            return Some("model");
        }
        // Compare the trimmed form the envelope stores (see the buyer
        // crate's `build_envelope`), so a re-drive that only re-spaces
        // the flag inherits the journaled purchase instead of conflicting.
        if let Some(class) = gpu_class {
            if p.capability_requirement.gpu_class.as_deref() != Some(class.trim()) {
                return Some("gpu_class");
            }
        }
        if min_vram_gb.is_some() && p.capability_requirement.min_vram_gb != min_vram_gb {
            return Some("min_vram_gb");
        }
        if min_reputation_bps.is_some()
            && p.capability_requirement.min_reputation_bps != min_reputation_bps
        {
            return Some("min_reputation_bps");
        }
        if price_micro_usdc.is_some_and(|v| v != p.price_micro_usdc) {
            return Some("price_micro_usdc");
        }
        if deadline_ms.is_some_and(|v| v != p.deadline_ms) {
            return Some("deadline_ms");
        }
        None
    }
}

/// The book itself. `open` gives the durable, file-backed form a real
/// daemon runs with; `in_memory` serves tests and embedders that
/// accept losing idempotence with the process.
pub struct PurchaseBook {
    entries: Mutex<Vec<PurchaseEntry>>,
    file: Option<Mutex<std::fs::File>>,
}

impl PurchaseBook {
    pub fn in_memory() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            file: None,
        }
    }

    pub fn open(path: &std::path::Path) -> Result<Self, PurchaseError> {
        let persist = |e: std::io::Error| PurchaseError::Persist(e.to_string());

        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(persist(e)),
        };
        let valid_len = bytes
            .iter()
            .rposition(|&b| b == b'\n')
            .map(|p| p + 1)
            .unwrap_or(0);
        if valid_len < bytes.len() {
            tracing::warn!(
                dropped_bytes = bytes.len() - valid_len,
                "truncating torn final purchase-book record"
            );
            let f = std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .map_err(persist)?;
            f.set_len(valid_len as u64).map_err(persist)?;
        }

        let mut entries: Vec<PurchaseEntry> = Vec::new();
        let mut lines = 0usize;
        for (i, line) in bytes[..valid_len].split(|&b| b == b'\n').enumerate() {
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            lines += 1;
            let entry: PurchaseEntry = serde_json::from_slice(line)
                .map_err(|e| PurchaseError::Persist(format!("line {} is corrupt: {e}", i + 1)))?;
            match entries.iter_mut().find(|e| e.key == entry.key) {
                Some(existing) => *existing = entry,
                None => entries.push(entry),
            }
        }
        entries.retain(|e| !e.voided);

        // Boot compaction. Every settle and void appends a full entry,
        // so a long-lived book carries one line per state change and
        // voided keys linger as dead weight. When history has piled up,
        // rewrite the file to one line per live key — the replayed
        // state above — via temp file, fsync, atomic rename, same as
        // the coordinator journal: a crash mid-compaction leaves the
        // original book intact.
        if lines > entries.len() {
            use std::io::Write;
            let mut buf = Vec::with_capacity(valid_len / 2);
            for entry in &entries {
                let line =
                    serde_json::to_vec(entry).map_err(|e| PurchaseError::Persist(e.to_string()))?;
                buf.extend_from_slice(&line);
                buf.push(b'\n');
            }
            let tmp_path = path.with_extension("jsonl.compacting");
            let mut tmp = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp_path)
                .map_err(persist)?;
            tmp.write_all(&buf).map_err(persist)?;
            tmp.sync_all().map_err(persist)?;
            std::fs::rename(&tmp_path, path).map_err(persist)?;
            tracing::info!(
                lines_before = lines,
                entries_after = entries.len(),
                "compacted purchase book"
            );
        }

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

    fn append(&self, entry: &PurchaseEntry) -> Result<(), PurchaseError> {
        use std::io::Write;
        let Some(file) = &self.file else {
            return Ok(());
        };
        let persist = |e: std::io::Error| PurchaseError::Persist(e.to_string());
        let mut line =
            serde_json::to_vec(entry).map_err(|e| PurchaseError::Persist(e.to_string()))?;
        line.push(b'\n');
        let mut file = file.lock();
        file.write_all(&line).map_err(persist)?;
        // fdatasync, not flush: the book is what stands between a
        // crashed buyer and paying twice — it must survive a power
        // loss, not just a kill.
        file.sync_data().map_err(persist)
    }

    pub fn lookup(&self, key: &str) -> Option<PurchaseEntry> {
        self.entries.lock().iter().find(|e| e.key == key).cloned()
    }

    /// Books a fresh purchase. Durable before it returns, and refused
    /// while any entry — in flight or settled — already holds the key:
    /// the caller must consult `lookup` first, and two racing calls
    /// under one key must collapse to one purchase.
    pub fn record(&self, entry: PurchaseEntry) -> Result<(), PurchaseError> {
        let mut guard = self.entries.lock();
        if guard.iter().any(|e| e.key == entry.key) {
            return Err(PurchaseError::InFlight(entry.key));
        }
        self.append(&entry)?;
        guard.push(entry);
        Ok(())
    }

    /// Marks a purchase paid-and-returned. The entry stays: from here
    /// on the key answers replays. Unknown keys are a no-op.
    pub fn settle(&self, key: &str, receipt_id: Uuid) -> Result<(), PurchaseError> {
        let mut guard = self.entries.lock();
        let Some(entry) = guard.iter_mut().find(|e| e.key == key) else {
            return Ok(());
        };
        entry.receipt_id = Some(receipt_id);
        let entry = entry.clone();
        self.append(&entry)
    }

    /// Frees a key whose job concluded unpaid — refunded, rejected, or
    /// failed. The money never moved, so a later call under the same
    /// key may honestly buy again. Unknown keys are a no-op.
    pub fn void(&self, key: &str) -> Result<(), PurchaseError> {
        let mut guard = self.entries.lock();
        let Some(pos) = guard.iter().position(|e| e.key == key) else {
            return Ok(());
        };
        let mut entry = guard[pos].clone();
        entry.voided = true;
        self.append(&entry)?;
        guard.remove(pos);
        Ok(())
    }

    /// [`PurchaseBook::void`] addressed by the job instead of the key —
    /// what a cancel has in hand. A cancelled job concluded unpaid, so
    /// whatever key bought it may honestly buy again; `false` when no
    /// live entry names the job (a keyless purchase, or one already
    /// settled and served — a settled entry stays, because its money
    /// moved and its key must keep answering the replay).
    pub fn void_by_job(&self, job_id: Uuid) -> Result<bool, PurchaseError> {
        let key = {
            let guard = self.entries.lock();
            guard
                .iter()
                .find(|e| e.envelope.payload.job_id == job_id && !e.settled())
                .map(|e| e.key.clone())
        };
        match key {
            Some(key) => self.void(&key).map(|()| true),
            None => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
    use covenant_compute_protocol::{CapabilityRequirement, JobEnvelopePayload, JobKind};
    use covenant_identity::LocalIdentity;
    use covenant_mcp::Content;

    fn entry_for(key: &str) -> PurchaseEntry {
        let buyer = LocalIdentity::generate("buyer@purchases");
        let job_id = Uuid::new_v4();
        let envelope = SignedJobEnvelope::sign(
            JobEnvelopePayload {
                job_id,
                buyer: buyer.agent_id(),
                kind: JobKind::InferenceCall,
                capability_requirement: CapabilityRequirement {
                    gpu_class: None,
                    min_vram_gb: None,
                    model_id: None,
                    kind: JobKind::InferenceCall,
                    max_duration_secs: 30,
                    min_reputation_bps: None,
                },
                input: vec![Content::text("buy once")],
                price_micro_usdc: 5_000,
                deadline_ms: 30_000,
                idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, key),
                issued_at_ms: 1,
                referral_code: None,
                stream: false,
            },
            &buyer,
        )
        .unwrap();
        PurchaseEntry {
            key: key.to_string(),
            envelope,
            opened_at_ms: 1,
            receipt_id: None,
            voided: false,
        }
    }

    #[test]
    fn a_recorded_purchase_survives_reopen_with_the_same_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("purchases.jsonl");

        let book = PurchaseBook::open(&path).unwrap();
        let entry = entry_for("payer:one");
        let job_id = entry.envelope.payload.job_id;
        book.record(entry).unwrap();

        let reopened = PurchaseBook::open(&path).unwrap();
        let restored = reopened.lookup("payer:one").expect("entry survived");
        assert_eq!(restored.envelope.payload.job_id, job_id);
        assert!(!restored.settled());
        restored.envelope.verify().unwrap();
    }

    #[test]
    fn a_settled_purchase_keeps_answering_and_a_voided_one_frees_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("purchases.jsonl");
        let receipt_id = Uuid::new_v4();

        let book = PurchaseBook::open(&path).unwrap();
        book.record(entry_for("payer:paid")).unwrap();
        book.settle("payer:paid", receipt_id).unwrap();
        book.record(entry_for("payer:unpaid")).unwrap();
        book.void("payer:unpaid").unwrap();

        let reopened = PurchaseBook::open(&path).unwrap();
        let paid = reopened.lookup("payer:paid").expect("settled entry stays");
        assert_eq!(paid.receipt_id, Some(receipt_id));
        assert!(
            reopened.lookup("payer:unpaid").is_none(),
            "a voided key is free to buy again"
        );
        assert!(reopened.record(entry_for("payer:unpaid")).is_ok());
    }

    #[test]
    fn void_by_job_frees_the_cancelled_jobs_key_but_never_a_settled_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("purchases.jsonl");
        let book = PurchaseBook::open(&path).unwrap();

        let cancelled = entry_for("payer:cancelled");
        let cancelled_job = cancelled.envelope.payload.job_id;
        book.record(cancelled).unwrap();

        let served = entry_for("payer:served");
        let served_job = served.envelope.payload.job_id;
        book.record(served).unwrap();
        book.settle("payer:served", Uuid::new_v4()).unwrap();

        // The cancelled job's key frees — durably.
        assert!(book.void_by_job(cancelled_job).unwrap());
        let reopened = PurchaseBook::open(&path).unwrap();
        assert!(reopened.lookup("payer:cancelled").is_none());
        assert!(reopened.record(entry_for("payer:cancelled")).is_ok());

        // A settled purchase's money moved: its key must keep answering
        // the replay, whatever job id a caller waves at it.
        assert!(!reopened.void_by_job(served_job).unwrap());
        assert!(reopened.lookup("payer:served").is_some());

        // A job no live entry names is a clean no-op.
        assert!(!reopened.void_by_job(Uuid::new_v4()).unwrap());
    }

    #[test]
    fn only_explicit_contradictions_conflict_with_a_journaled_purchase() {
        let entry = entry_for("payer:pinned");
        let input = entry.envelope.payload.input.clone();

        // (model, gpu_class, min_vram_gb, min_reputation_bps, price, deadline).
        let check = |model, gpu_class, min_vram_gb, min_reputation_bps, price, deadline| {
            entry.conflicting_argument(
                JobKind::InferenceCall,
                &input,
                model,
                gpu_class,
                min_vram_gb,
                min_reputation_bps,
                price,
                deadline,
            )
        };

        // Omitted arguments inherit the purchase of record.
        assert_eq!(check(None, None, None, None, None, None), None);
        // Explicit arguments that agree are no conflict either.
        assert_eq!(
            check(None, None, None, None, Some(5_000), Some(30_000)),
            None
        );
        // The question itself is always explicit.
        assert_eq!(
            entry.conflicting_argument(
                JobKind::InferenceCall,
                &[Content::text("a different question")],
                None,
                None,
                None,
                None,
                None,
                None
            ),
            Some("input")
        );
        assert_eq!(
            entry.conflicting_argument(
                JobKind::BatchJob,
                &input,
                None,
                None,
                None,
                None,
                None,
                None
            ),
            Some("kind")
        );
        assert_eq!(
            check(Some("some-model"), None, None, None, None, None),
            Some("model")
        );
        // The journaled purchase named no hardware or trust floor; asking
        // for a specific class, VRAM or reputation floor now must refuse,
        // not silently serve it.
        assert_eq!(
            check(None, Some("h100"), None, None, None, None),
            Some("gpu_class")
        );
        assert_eq!(
            check(None, None, Some(24), None, None, None),
            Some("min_vram_gb")
        );
        assert_eq!(
            check(None, None, None, Some(8_000), None, None),
            Some("min_reputation_bps")
        );
        assert_eq!(
            check(None, None, None, None, Some(9_999), None),
            Some("price_micro_usdc")
        );
        assert_eq!(
            check(None, None, None, None, None, Some(1)),
            Some("deadline_ms")
        );
    }

    #[test]
    fn boot_compacts_the_book_to_one_line_per_live_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("purchases.jsonl");
        let receipt_id = Uuid::new_v4();

        let book = PurchaseBook::open(&path).unwrap();
        book.record(entry_for("payer:settled")).unwrap();
        book.settle("payer:settled", receipt_id).unwrap();
        book.record(entry_for("payer:open")).unwrap();
        book.record(entry_for("payer:gone")).unwrap();
        book.void("payer:gone").unwrap();
        let lines = |p: &std::path::Path| {
            std::fs::read_to_string(p)
                .unwrap()
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count()
        };
        assert_eq!(lines(&path), 5, "one line per state change before boot");

        let reopened = PurchaseBook::open(&path).unwrap();
        assert_eq!(lines(&path), 2, "one line per live key after boot");
        let settled = reopened.lookup("payer:settled").expect("settled stays");
        assert_eq!(settled.receipt_id, Some(receipt_id));
        settled.envelope.verify().unwrap();
        assert!(reopened
            .lookup("payer:open")
            .expect("in flight stays")
            .receipt_id
            .is_none());
        assert!(reopened.lookup("payer:gone").is_none());

        // Appends land in the compacted file, and a boot with no
        // history to fold leaves the file alone.
        reopened.record(entry_for("payer:fresh")).unwrap();
        let again = PurchaseBook::open(&path).unwrap();
        assert_eq!(lines(&path), 3);
        assert!(again.lookup("payer:fresh").is_some());
        assert_eq!(
            again.lookup("payer:settled").unwrap().receipt_id,
            Some(receipt_id)
        );
    }

    #[test]
    fn a_key_cannot_be_recorded_twice_while_held() {
        let book = PurchaseBook::in_memory();
        book.record(entry_for("payer:dup")).unwrap();
        assert!(matches!(
            book.record(entry_for("payer:dup")),
            Err(PurchaseError::InFlight(k)) if k == "payer:dup"
        ));
        book.settle("payer:dup", Uuid::new_v4()).unwrap();
        assert!(
            matches!(
                book.record(entry_for("payer:dup")),
                Err(PurchaseError::InFlight(_))
            ),
            "a settled key still answers replays; it never re-records"
        );
    }

    #[test]
    fn a_torn_final_append_is_dropped_and_the_fsynced_history_survives() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("purchases.jsonl");

        // One clean, fsynced line, then a buyer that died mid-append:
        // half a record with no closing newline.
        let mut good = serde_json::to_vec(&entry_for("payer:survivor")).unwrap();
        good.push(b'\n');
        let half = serde_json::to_vec(&entry_for("payer:torn")).unwrap();
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(&good).unwrap();
            f.write_all(&half[..half.len() / 2]).unwrap();
        }

        let book = PurchaseBook::open(&path).unwrap();
        assert!(
            book.lookup("payer:survivor").is_some(),
            "the completed line before the crash still stands"
        );
        assert!(book.lookup("payer:torn").is_none());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            good,
            "the torn tail is truncated off the file, not left to trip the next boot"
        );
    }

    #[test]
    fn a_corrupt_record_fails_the_open_rather_than_dropping_a_purchase() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("purchases.jsonl");

        // A corrupt line in the body of the book (newline-terminated, so
        // not a torn tail) is a hard error: silently skipping it could
        // drop a settled purchase and let a paid job be bought again.
        let mut buf = serde_json::to_vec(&entry_for("payer:one")).unwrap();
        buf.push(b'\n');
        buf.extend_from_slice(b"{not a purchase entry}\n");
        std::fs::write(&path, &buf).unwrap();

        let err = match PurchaseBook::open(&path) {
            Err(e) => e,
            Ok(_) => panic!("a corrupt record must fail the open, not open silently"),
        };
        assert!(
            matches!(err, PurchaseError::Persist(ref m) if m.contains("line 2") && m.contains("corrupt")),
            "got: {err:?}"
        );
    }
}
