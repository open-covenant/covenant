//! Durable redelivery queue for results the coordinator never
//! acknowledged.
//!
//! The node burns real compute before it ever calls `submit_result`; a
//! coordinator restart (a deploy, a crash) at that moment used to cost
//! the operator the whole job — the HTTP client's transport retries
//! span seconds, a restart spans longer, and the signed receipt and
//! output lived only on the call stack. The coordinator is idempotent
//! to redelivery (a duplicate is refused, a crash-orphaned release is
//! filled in and paid), so the node's half of at-least-once delivery
//! is just not forgetting: results whose push failed at transport are
//! queued here and re-pushed by the serve loop until one lands a
//! response — any response. A landed refusal settles the entry too;
//! re-sending a verdict the coordinator already gave is noise, not
//! recovery.
//!
//! Same JSONL idiom as [`crate::earnings::JsonlEarningsLedger`]: every
//! state change appends the full entry, replay keeps the last state
//! per job and drops the settled ones, a torn final line is truncated,
//! and boot compacts the settled history away — a healthy node's
//! outbox holds its undelivered results, not every delivery it ever
//! retried.

use covenant_compute_protocol::{FundingSource, JobResultMessage};
use parking_lot::Mutex;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum OutboxError {
    #[error("persist: {0}")]
    Persist(String),
    #[error("job {0} already queued")]
    AlreadyQueued(Uuid),
}

/// One undelivered result: the exact wire message plus what the credit
/// path needs when a redelivery finally settles — the offer that knew
/// the funding source is long gone by then.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OutboxEntry {
    pub job_id: Uuid,
    pub message: JobResultMessage,
    pub funding_source: FundingSource,
    pub queued_at_ms: u64,
    #[serde(default)]
    pub settled: bool,
}

/// The queue itself. `open` gives the durable, file-backed form the
/// real binary runs with; `in_memory` serves tests and embedders that
/// accept losing queued results with the process.
pub struct ResultOutbox {
    entries: Mutex<Vec<OutboxEntry>>,
    file: Option<Mutex<std::fs::File>>,
}

impl ResultOutbox {
    pub fn in_memory() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            file: None,
        }
    }

    pub fn open(path: &std::path::Path) -> Result<Self, OutboxError> {
        let persist = |e: std::io::Error| OutboxError::Persist(e.to_string());

        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(persist(e)),
        };
        let valid_len = valid_prefix_len(&bytes);
        if valid_len < bytes.len() {
            tracing::warn!(
                dropped_bytes = bytes.len() - valid_len,
                "truncating torn final outbox record"
            );
            let f = std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .map_err(persist)?;
            f.set_len(valid_len as u64).map_err(persist)?;
        }

        let (entries, lines) = replay(&bytes[..valid_len])?;

        // Boot compaction. Every recovered delivery leaves a queue
        // line and a tombstone behind, so the file grows with outages
        // survived, not results still owed. Rewrite it to the replayed
        // state — temp file, fsync, atomic rename, the coordinator
        // journal's idiom — so a crash mid-compaction leaves the
        // original queue intact.
        if lines > entries.len() {
            use std::io::Write;
            let mut buf = Vec::with_capacity(valid_len / 2);
            for entry in &entries {
                let line =
                    serde_json::to_vec(entry).map_err(|e| OutboxError::Persist(e.to_string()))?;
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
                "compacted result outbox"
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

    /// Replays the queue into memory WITHOUT touching the file — no
    /// torn-tail truncation, no boot compaction, no append handle. This
    /// is what a `status`/`earnings` read must use: an operator runs
    /// those against the same home a live `serve` process is appending
    /// to, and [`ResultOutbox::open`]'s healing writes would race that
    /// node — truncating its in-flight append or renaming the queue out
    /// from under its handle. The result never writes (`file: None`), so
    /// it is safe to run beside the node; the serve process stays the
    /// one writer that heals and compacts.
    pub fn read_only(path: &std::path::Path) -> Result<Self, OutboxError> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(OutboxError::Persist(e.to_string())),
        };
        let (entries, _) = replay(&bytes[..valid_prefix_len(&bytes)])?;
        Ok(Self {
            entries: Mutex::new(entries),
            file: None,
        })
    }

    fn append(&self, entry: &OutboxEntry) -> Result<(), OutboxError> {
        use std::io::Write;
        let Some(file) = &self.file else {
            return Ok(());
        };
        let persist = |e: std::io::Error| OutboxError::Persist(e.to_string());
        let mut line =
            serde_json::to_vec(entry).map_err(|e| OutboxError::Persist(e.to_string()))?;
        line.push(b'\n');
        let mut file = file.lock();
        file.write_all(&line).map_err(persist)?;
        // fdatasync, not flush: a queued result is the only proof of
        // pay-worthy work once the transport failed — it must survive a
        // power loss, not just a process kill.
        file.sync_data().map_err(persist)
    }

    /// Queues an undelivered result. Durable before it returns — an
    /// entry this cannot persist is not queued, so the caller still
    /// holds the failure.
    pub fn enqueue(&self, entry: OutboxEntry) -> Result<(), OutboxError> {
        let mut guard = self.entries.lock();
        if guard.iter().any(|e| e.job_id == entry.job_id) {
            return Err(OutboxError::AlreadyQueued(entry.job_id));
        }
        self.append(&entry)?;
        guard.push(entry);
        Ok(())
    }

    /// Every queued result, oldest first — the drain's worklist.
    pub fn pending(&self) -> Vec<OutboxEntry> {
        self.entries.lock().clone()
    }

    /// Marks a queued result delivered (or refused — either way, the
    /// coordinator answered) and tombstones it in the journal. Unknown
    /// job ids are fine: a drain racing a settle must not fail.
    pub fn settle(&self, job_id: Uuid) -> Result<(), OutboxError> {
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
/// (undelivered) entries, last write winning per job id, and returns
/// them with the total line count the compactor uses to decide whether
/// the file has settled history to reclaim. Shared by [`ResultOutbox::open`]
/// (which heals and compacts the file first) and
/// [`ResultOutbox::read_only`] (which touches nothing on disk), so both
/// replay identically.
fn replay(valid: &[u8]) -> Result<(Vec<OutboxEntry>, usize), OutboxError> {
    let mut entries: Vec<OutboxEntry> = Vec::new();
    let mut lines = 0usize;
    for (i, line) in valid.split(|&b| b == b'\n').enumerate() {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        lines += 1;
        let entry: OutboxEntry = serde_json::from_slice(line)
            .map_err(|e| OutboxError::Persist(format!("line {} is corrupt: {e}", i + 1)))?;
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
    use covenant_a2a::A2ATaskStatus;
    use covenant_compute_protocol::{JobMeter, SignedWorkReceipt, WorkReceiptPayload};
    use covenant_identity::LocalIdentity;

    fn entry_for(job_id: Uuid) -> OutboxEntry {
        let operator = LocalIdentity::generate("operator@outbox");
        let receipt = SignedWorkReceipt::sign(
            WorkReceiptPayload {
                job_id,
                operator: operator.agent_id(),
                job_hash_hex: "aa".repeat(32),
                result_hash_hex: "bb".repeat(32),
                meter: JobMeter {
                    wall_ms: 1,
                    tokens_in: None,
                    tokens_out: None,
                    gpu_seconds: None,
                    finish_reason: None,
                },
                price_micro_usdc: 100,
                status: A2ATaskStatus::Ok,
                executed_at_ms: 1,
                node_audit_root_hex: "cc".repeat(32),
            },
            &operator,
        )
        .unwrap();
        OutboxEntry {
            job_id,
            message: JobResultMessage {
                receipt,
                output: vec![covenant_mcp::Content::text("queued work")],
            },
            funding_source: FundingSource::Organic,
            queued_at_ms: 1,
            settled: false,
        }
    }

    #[test]
    fn a_queued_result_survives_reopen_and_a_settled_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let kept = Uuid::new_v4();
        let done = Uuid::new_v4();

        let outbox = ResultOutbox::open(&path).unwrap();
        outbox.enqueue(entry_for(kept)).unwrap();
        outbox.enqueue(entry_for(done)).unwrap();
        outbox.settle(done).unwrap();
        assert_eq!(outbox.pending().len(), 1);

        let reopened = ResultOutbox::open(&path).unwrap();
        let pending = reopened.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].job_id, kept);
        pending[0].message.receipt.verify().unwrap();
    }

    #[test]
    fn boot_compacts_settled_history_to_the_undelivered_results() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let owed = Uuid::new_v4();

        let outbox = ResultOutbox::open(&path).unwrap();
        outbox.enqueue(entry_for(owed)).unwrap();
        for _ in 0..3 {
            let delivered = Uuid::new_v4();
            outbox.enqueue(entry_for(delivered)).unwrap();
            outbox.settle(delivered).unwrap();
        }
        let lines = |p: &std::path::Path| {
            std::fs::read_to_string(p)
                .unwrap()
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count()
        };
        assert_eq!(lines(&path), 7, "grows with outages survived before boot");

        let reopened = ResultOutbox::open(&path).unwrap();
        assert_eq!(lines(&path), 1, "one line per result still owed after boot");
        let pending = reopened.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].job_id, owed);
        pending[0].message.receipt.verify().unwrap();
    }

    #[test]
    fn read_only_replays_the_undelivered_results_without_touching_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let owed = Uuid::new_v4();

        // A queue with settled history: compaction-eligible, and with a
        // torn final record a live writer left mid-append. `open` would
        // heal both; a read beside a live node must heal neither.
        let outbox = ResultOutbox::open(&path).unwrap();
        outbox.enqueue(entry_for(owed)).unwrap();
        for _ in 0..2 {
            let delivered = Uuid::new_v4();
            outbox.enqueue(entry_for(delivered)).unwrap();
            outbox.settle(delivered).unwrap();
        }
        drop(outbox);
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(b"{\"job_id\":\"torn").unwrap();
        }
        let before = std::fs::read(&path).unwrap();

        let view = ResultOutbox::read_only(&path).unwrap();
        let pending = view.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].job_id, owed);
        pending[0].message.receipt.verify().unwrap();

        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "a read-only replay must leave the live writer's file byte-for-byte intact"
        );
    }

    #[test]
    fn open_truncates_a_torn_tail_so_the_next_enqueue_starts_clean() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let first = Uuid::new_v4();
        {
            let outbox = ResultOutbox::open(&path).unwrap();
            outbox.enqueue(entry_for(first)).unwrap();
        }

        // A node that died mid-append leaves a result with no closing
        // newline. Reopen truncates it, so the next enqueue writes a fresh
        // line instead of gluing onto the garbage — which the corrupt-line
        // guard would then refuse to load, stranding every still-owed
        // result and the pay it stands for.
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(b"{\"job_id\":\"torn").unwrap();
        }

        let second = Uuid::new_v4();
        {
            let outbox = ResultOutbox::open(&path).unwrap();
            assert_eq!(outbox.pending().len(), 1);
            outbox.enqueue(entry_for(second)).unwrap();
        }

        let pending = ResultOutbox::open(&path).unwrap().pending();
        assert_eq!(pending.len(), 2);
        assert!(pending.iter().any(|e| e.job_id == first));
        assert!(pending.iter().any(|e| e.job_id == second));
    }

    #[test]
    fn a_corrupt_record_fails_the_open_rather_than_dropping_a_result() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");

        // A newline-terminated garbage line in the body is damage, not a
        // torn tail. Skipping it would drop an undelivered result — the
        // only proof of pay-worthy work once the transport failed — so
        // the open fails hard rather than losing it.
        let mut buf = serde_json::to_vec(&entry_for(Uuid::new_v4())).unwrap();
        buf.push(b'\n');
        buf.extend_from_slice(b"{not an outbox entry}\n");
        std::fs::write(&path, &buf).unwrap();

        let err = match ResultOutbox::open(&path) {
            Err(e) => e,
            Ok(_) => panic!("a corrupt record must fail the open, not open silently"),
        };
        assert!(
            matches!(err, OutboxError::Persist(ref m) if m.contains("line 2") && m.contains("corrupt")),
            "got: {err:?}"
        );
    }

    #[test]
    fn a_job_cannot_queue_twice_and_settling_a_stranger_is_a_no_op() {
        let outbox = ResultOutbox::in_memory();
        let job_id = Uuid::new_v4();
        outbox.enqueue(entry_for(job_id)).unwrap();
        assert!(matches!(
            outbox.enqueue(entry_for(job_id)),
            Err(OutboxError::AlreadyQueued(id)) if id == job_id
        ));
        outbox.settle(Uuid::new_v4()).unwrap();
        assert_eq!(outbox.pending().len(), 1);
    }
}
