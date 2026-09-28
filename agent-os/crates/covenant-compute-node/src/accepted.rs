//! Durable book of accepted-but-unfinished jobs — the accept side's
//! twin of [`crate::outbox::ResultOutbox`].
//!
//! Once the node tells the coordinator "accepted", the job is this
//! operator's until the deadline: the coordinator dispatches it to no
//! one else, and a node process that dies mid-execution used to forget
//! the job entirely — the buyer was made whole by the deadline sweep,
//! but the operator ate a reputation fault and lost the pay for work a
//! quick restart could have served. The book is the node's memory
//! across that window: every accepted job is written here before
//! execution starts and tombstoned when its result is disposed of —
//! acknowledged, refused, or handed to the outbox. At boot, whatever
//! is still booked is a job a previous life accepted and never
//! finished; [`crate::node::Node::recover_accepted`] re-serves it if
//! the deadline still allows.
//!
//! `lives` counts the process lives that attempted execution, bumped
//! durably before each recovery run — a job whose execution kills the
//! node would otherwise crash-loop it at every boot.
//!
//! Same JSONL idiom as the outbox and the earnings ledger: every state
//! change appends the full entry, replay keeps the last state per job
//! and drops the settled ones, a torn final line is truncated, and
//! boot compacts the settled history away — a healthy node's book
//! holds its in-flight jobs, not every job it ever served.

use covenant_compute_protocol::{EscrowHoldAttestation, SignedJobEnvelope};
use parking_lot::Mutex;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum AcceptedError {
    #[error("persist: {0}")]
    Persist(String),
    #[error("job {0} already booked")]
    AlreadyBooked(Uuid),
}

/// One accepted-and-unfinished job: everything the execution path
/// needs to run it again — the signed envelope is the work order, the
/// escrow attestation carries the funding source the credit path
/// books under.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AcceptedEntry {
    pub job_id: Uuid,
    pub envelope: SignedJobEnvelope,
    pub escrow_hold: EscrowHoldAttestation,
    pub accepted_at_ms: u64,
    /// Process lives that have attempted execution; 1 is the life that
    /// accepted the job.
    pub lives: u32,
    #[serde(default)]
    pub settled: bool,
}

/// The book itself. `open` gives the durable, file-backed form the
/// real binary runs with; `in_memory` serves tests and embedders that
/// accept forgetting accepted jobs with the process.
pub struct AcceptedBook {
    entries: Mutex<Vec<AcceptedEntry>>,
    file: Option<Mutex<std::fs::File>>,
}

impl AcceptedBook {
    pub fn in_memory() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            file: None,
        }
    }

    pub fn open(path: &std::path::Path) -> Result<Self, AcceptedError> {
        let persist = |e: std::io::Error| AcceptedError::Persist(e.to_string());

        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(persist(e)),
        };
        let valid_len = valid_prefix_len(&bytes);
        if valid_len < bytes.len() {
            tracing::warn!(
                dropped_bytes = bytes.len() - valid_len,
                "truncating torn final accepted-book record"
            );
            let f = std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .map_err(persist)?;
            f.set_len(valid_len as u64).map_err(persist)?;
        }

        let (entries, lines) = replay(&bytes[..valid_len])?;

        // Boot compaction. Every served job leaves a booking and a
        // tombstone behind, so the file grows with jobs served, not
        // jobs in flight. Rewrite it to the replayed state — temp
        // file, fsync, atomic rename, the coordinator journal's idiom —
        // so a crash mid-compaction leaves the original book intact.
        if lines > entries.len() {
            use std::io::Write;
            let mut buf = Vec::with_capacity(valid_len / 2);
            for entry in &entries {
                let line =
                    serde_json::to_vec(entry).map_err(|e| AcceptedError::Persist(e.to_string()))?;
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
                "compacted accepted book"
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

    /// Replays the book into memory WITHOUT touching the file — no
    /// torn-tail truncation, no boot compaction, no append handle. This
    /// is what a `status`/`earnings` read must use: an operator runs
    /// those against the same home a live `serve` process is appending
    /// to, and [`AcceptedBook::open`]'s healing writes would race that
    /// node — truncating its in-flight append or renaming the book out
    /// from under its handle. The result never writes (`file: None`), so
    /// it is safe to run beside the node; the serve process stays the
    /// one writer that heals and compacts.
    pub fn read_only(path: &std::path::Path) -> Result<Self, AcceptedError> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(AcceptedError::Persist(e.to_string())),
        };
        let (entries, _) = replay(&bytes[..valid_prefix_len(&bytes)])?;
        Ok(Self {
            entries: Mutex::new(entries),
            file: None,
        })
    }

    fn append(&self, entry: &AcceptedEntry) -> Result<(), AcceptedError> {
        use std::io::Write;
        let Some(file) = &self.file else {
            return Ok(());
        };
        let persist = |e: std::io::Error| AcceptedError::Persist(e.to_string());
        let mut line =
            serde_json::to_vec(entry).map_err(|e| AcceptedError::Persist(e.to_string()))?;
        line.push(b'\n');
        let mut file = file.lock();
        file.write_all(&line).map_err(persist)?;
        // fdatasync, not flush: the book is the only memory of an
        // accepted job once the process dies — it must survive a power
        // loss, not just a kill.
        file.sync_data().map_err(persist)
    }

    /// Books an accepted job. Durable before it returns.
    pub fn book(&self, entry: AcceptedEntry) -> Result<(), AcceptedError> {
        let mut guard = self.entries.lock();
        if guard.iter().any(|e| e.job_id == entry.job_id) {
            return Err(AcceptedError::AlreadyBooked(entry.job_id));
        }
        self.append(&entry)?;
        guard.push(entry);
        Ok(())
    }

    /// Every unfinished job, oldest first — recovery's worklist.
    pub fn pending(&self) -> Vec<AcceptedEntry> {
        self.entries.lock().clone()
    }

    /// Bumps the entry's life count, durably, before a recovery run
    /// executes it — the crash-loop guard. Unknown job ids are a no-op.
    pub fn record_life(&self, job_id: Uuid) -> Result<(), AcceptedError> {
        let mut guard = self.entries.lock();
        let Some(entry) = guard.iter_mut().find(|e| e.job_id == job_id) else {
            return Ok(());
        };
        entry.lives += 1;
        let entry = entry.clone();
        self.append(&entry)
    }

    /// Marks a job's result disposed of — acknowledged, refused, or
    /// queued in the outbox — and tombstones it in the book. Unknown
    /// job ids are fine: nothing to finish is the goal state.
    pub fn settle(&self, job_id: Uuid) -> Result<(), AcceptedError> {
        let mut guard = self.entries.lock();
        let Some(pos) = guard.iter().position(|e| e.job_id == job_id) else {
            return Ok(());
        };
        let mut entry = guard[pos].clone();
        entry.settled = true;
        self.append(&entry)?;
        guard.remove(pos);
        Ok(())
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

/// Parses newline-terminated journal bytes into the surviving
/// (unfinished) bookings, last write winning per job id, and returns
/// them with the total line count the compactor uses to decide whether
/// the file has settled history to reclaim. Shared by [`AcceptedBook::open`]
/// (which heals and compacts the file first) and
/// [`AcceptedBook::read_only`] (which touches nothing on disk), so both
/// replay identically.
fn replay(valid: &[u8]) -> Result<(Vec<AcceptedEntry>, usize), AcceptedError> {
    let mut entries: Vec<AcceptedEntry> = Vec::new();
    let mut lines = 0usize;
    for (i, line) in valid.split(|&b| b == b'\n').enumerate() {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        lines += 1;
        let entry: AcceptedEntry = serde_json::from_slice(line)
            .map_err(|e| AcceptedError::Persist(format!("line {} is corrupt: {e}", i + 1)))?;
        match entries.iter_mut().find(|e| e.job_id == entry.job_id) {
            Some(existing) => *existing = entry,
            None => entries.push(entry),
        }
    }
    entries.retain(|e| !e.settled);
    Ok((entries, lines))
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
    use covenant_compute_protocol::{
        CapabilityRequirement, FundingSource, JobEnvelopePayload, JobKind,
    };
    use covenant_identity::LocalIdentity;
    use covenant_mcp::Content;

    fn entry_for(job_id: Uuid) -> AcceptedEntry {
        let buyer = LocalIdentity::generate("buyer@accepted");
        let coordinator = LocalIdentity::generate("coordinator@accepted");
        let envelope = SignedJobEnvelope::sign(
            JobEnvelopePayload {
                job_id,
                buyer: buyer.agent_id(),
                kind: JobKind::BatchJob,
                capability_requirement: CapabilityRequirement {
                    gpu_class: None,
                    min_vram_gb: None,
                    model_id: None,
                    kind: JobKind::BatchJob,
                    max_duration_secs: 30,
                    min_reputation_bps: None,
                },
                input: vec![Content::text("work worth remembering")],
                price_micro_usdc: 5_000,
                deadline_ms: 30_000,
                idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "accepted-book"),
                issued_at_ms: 1,
                referral_code: None,
                stream: false,
            },
            &buyer,
        )
        .unwrap();
        let escrow_hold =
            EscrowHoldAttestation::sign(job_id, 5_000, FundingSource::Organic, 1, &coordinator)
                .unwrap();
        AcceptedEntry {
            job_id,
            envelope,
            escrow_hold,
            accepted_at_ms: 1,
            lives: 1,
            settled: false,
        }
    }

    #[test]
    fn a_booked_job_survives_reopen_and_a_settled_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accepted.jsonl");
        let kept = Uuid::new_v4();
        let done = Uuid::new_v4();

        let book = AcceptedBook::open(&path).unwrap();
        book.book(entry_for(kept)).unwrap();
        book.book(entry_for(done)).unwrap();
        book.settle(done).unwrap();
        assert_eq!(book.pending().len(), 1);

        let reopened = AcceptedBook::open(&path).unwrap();
        let pending = reopened.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].job_id, kept);
        pending[0].envelope.verify().unwrap();
    }

    #[test]
    fn read_only_replays_the_unfinished_jobs_without_touching_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accepted.jsonl");
        let unfinished = Uuid::new_v4();

        // A book with settled history (compaction-eligible) plus a torn
        // final record a live writer left mid-append. `open` heals both;
        // a read beside a live node must heal neither.
        let book = AcceptedBook::open(&path).unwrap();
        book.book(entry_for(unfinished)).unwrap();
        for _ in 0..2 {
            let served = Uuid::new_v4();
            book.book(entry_for(served)).unwrap();
            book.settle(served).unwrap();
        }
        drop(book);
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(b"{\"job_id\":\"torn").unwrap();
        }
        let before = std::fs::read(&path).unwrap();

        let view = AcceptedBook::read_only(&path).unwrap();
        let pending = view.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].job_id, unfinished);
        pending[0].envelope.verify().unwrap();

        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "a read-only replay must leave the live writer's file byte-for-byte intact"
        );
    }

    #[test]
    fn open_truncates_a_torn_tail_so_the_next_booking_starts_clean() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accepted.jsonl");
        let first = Uuid::new_v4();
        {
            let book = AcceptedBook::open(&path).unwrap();
            book.book(entry_for(first)).unwrap();
        }

        // A node that died mid-append leaves a record with no closing
        // newline. Reopen truncates it, so the next booking writes a
        // fresh line instead of gluing onto the garbage — which would
        // corrupt the book and make every later reopen refuse to load,
        // stranding the operator's claim on the escrowed pay.
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(b"{\"job_id\":\"torn").unwrap();
        }

        let second = Uuid::new_v4();
        {
            let book = AcceptedBook::open(&path).unwrap();
            assert_eq!(book.pending().len(), 1);
            book.book(entry_for(second)).unwrap();
        }

        let pending = AcceptedBook::open(&path).unwrap().pending();
        assert_eq!(pending.len(), 2);
        assert!(pending.iter().any(|e| e.job_id == first));
        assert!(pending.iter().any(|e| e.job_id == second));
    }

    #[test]
    fn a_corrupt_record_fails_the_open_rather_than_dropping_an_accepted_job() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accepted.jsonl");

        // A newline-terminated garbage line in the body of the book is
        // not a torn tail — the writer only ever emits complete records.
        // Skipping it would drop the proof a job was accepted and strand
        // the operator's claim on the escrowed pay, so the open fails hard.
        let mut buf = serde_json::to_vec(&entry_for(Uuid::new_v4())).unwrap();
        buf.push(b'\n');
        buf.extend_from_slice(b"{not an accepted entry}\n");
        std::fs::write(&path, &buf).unwrap();

        let err = match AcceptedBook::open(&path) {
            Err(e) => e,
            Ok(_) => panic!("a corrupt record must fail the open, not open silently"),
        };
        assert!(
            matches!(err, AcceptedError::Persist(ref m) if m.contains("line 2") && m.contains("corrupt")),
            "got: {err:?}"
        );
    }

    #[test]
    fn a_job_cannot_book_twice_and_settling_a_stranger_is_a_no_op() {
        let book = AcceptedBook::in_memory();
        let job_id = Uuid::new_v4();
        book.book(entry_for(job_id)).unwrap();
        assert!(matches!(
            book.book(entry_for(job_id)),
            Err(AcceptedError::AlreadyBooked(id)) if id == job_id
        ));
        book.settle(Uuid::new_v4()).unwrap();
        assert_eq!(book.pending().len(), 1);
    }

    #[test]
    fn boot_compacts_settled_history_to_the_in_flight_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accepted.jsonl");
        let unfinished = Uuid::new_v4();

        let book = AcceptedBook::open(&path).unwrap();
        book.book(entry_for(unfinished)).unwrap();
        book.record_life(unfinished).unwrap();
        for _ in 0..3 {
            let served = Uuid::new_v4();
            book.book(entry_for(served)).unwrap();
            book.settle(served).unwrap();
        }
        let lines = |p: &std::path::Path| {
            std::fs::read_to_string(p)
                .unwrap()
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count()
        };
        assert_eq!(lines(&path), 8, "grows with jobs served before boot");

        let reopened = AcceptedBook::open(&path).unwrap();
        assert_eq!(lines(&path), 1, "one line per in-flight job after boot");
        let pending = reopened.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].job_id, unfinished);
        assert_eq!(pending[0].lives, 2, "compaction keeps the latest state");
        pending[0].envelope.verify().unwrap();

        // Appends land in the compacted file, and a boot with nothing
        // to fold leaves the file alone.
        reopened.record_life(unfinished).unwrap();
        let again = AcceptedBook::open(&path).unwrap();
        assert_eq!(again.pending()[0].lives, 3);
    }

    #[test]
    fn a_recorded_life_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accepted.jsonl");
        let job_id = Uuid::new_v4();

        let book = AcceptedBook::open(&path).unwrap();
        book.book(entry_for(job_id)).unwrap();
        book.record_life(job_id).unwrap();
        book.record_life(Uuid::new_v4()).unwrap();

        let reopened = AcceptedBook::open(&path).unwrap();
        assert_eq!(reopened.pending()[0].lives, 2);
    }
}
