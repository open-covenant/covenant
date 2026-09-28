//! In-memory ledger for streaming jobs a start/poll surface has in
//! flight: `compute.stream_start` registers a job here and spawns the
//! drain into a background task; `compute.stream_poll` reads chunks by
//! cursor and, once the task concludes, the terminal payload.
//!
//! Same posture as the coordinator's stream book — never journaled,
//! because a chunk is a live preview whose durable form is the
//! receipt-verified final output — with two duties the coordinator
//! doesn't have: the owner check (only the caller that started a job
//! may read its feed) and the spend view (`active_committed` lets a
//! surface count in-flight streaming commitments against its budget or
//! session cap before starting another). A concluded job lingers so a
//! slow poller can still collect the outcome, then evicts; running
//! entries never evict — the drain task concludes them in bounded time
//! (the dispatch deadline plus one grace poll) even when the network
//! path dies.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use uuid::Uuid;

/// How long a concluded job's feed and terminal payload stay readable,
/// mirroring the coordinator's `STREAM_LINGER_MS`.
pub const STREAM_JOB_LINGER: Duration = Duration::from_secs(10 * 60);

/// Hard bound on retained concluded jobs, evicting oldest-first even
/// inside the linger window — the surface stays memory-bounded when a
/// caller starts many jobs and never polls any of them.
pub const MAX_CONCLUDED_JOBS: usize = 256;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum StreamJobsError {
    /// Covers a foreign owner as well as a genuinely unknown or
    /// evicted id — one wording, so the error is not an oracle for
    /// which job ids exist.
    #[error("no streaming job {0} for this caller")]
    UnknownJob(Uuid),
    #[error(
        "{active} streaming jobs already in flight for this caller (cap {cap}); \
         poll one to completion first"
    )]
    ActiveCap { active: usize, cap: usize },
    #[error("streaming job {0} is already tracked")]
    Duplicate(Uuid),
}

struct Entry<T> {
    owner: String,
    committed: u64,
    chunks: Vec<String>,
    outcome: Option<Result<T, String>>,
    concluded_at: Option<Instant>,
}

/// One page of a tracked job's feed: the chunks from the caller's
/// cursor on, the next cursor, and — once the drain task concluded —
/// the terminal payload, readable repeatedly until the entry evicts.
#[derive(Debug)]
pub struct StreamJobPoll<T> {
    pub chunks: Vec<String>,
    pub next_seq: u64,
    pub outcome: Option<Result<T, String>>,
}

pub struct StreamJobs<T> {
    jobs: Mutex<HashMap<Uuid, Entry<T>>>,
    max_active_per_owner: usize,
    linger: Duration,
    max_concluded: usize,
}

impl<T: Clone> StreamJobs<T> {
    pub fn new(max_active_per_owner: usize) -> Self {
        Self::with_limits(max_active_per_owner, STREAM_JOB_LINGER, MAX_CONCLUDED_JOBS)
    }

    pub fn with_limits(
        max_active_per_owner: usize,
        linger: Duration,
        max_concluded: usize,
    ) -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
            max_active_per_owner,
            linger,
            max_concluded,
        }
    }

    /// Registers a job under its owner before submission, enforcing the
    /// per-owner cap atomically — two concurrent starts cannot both
    /// slip under it. `committed` is the spend this job has offered, in
    /// whatever unit the surface budgets in.
    pub fn try_start(
        &self,
        owner: &str,
        job_id: Uuid,
        committed: u64,
    ) -> Result<(), StreamJobsError> {
        let mut jobs = self.jobs.lock();
        Self::evict(&mut jobs, self.linger, self.max_concluded);
        if jobs.contains_key(&job_id) {
            return Err(StreamJobsError::Duplicate(job_id));
        }
        let active = jobs
            .values()
            .filter(|e| e.owner == owner && e.outcome.is_none())
            .count();
        if active >= self.max_active_per_owner {
            return Err(StreamJobsError::ActiveCap {
                active,
                cap: self.max_active_per_owner,
            });
        }
        jobs.insert(
            job_id,
            Entry {
                owner: owner.to_string(),
                committed,
                chunks: Vec::new(),
                outcome: None,
                concluded_at: None,
            },
        );
        Ok(())
    }

    /// Rolls a registration back — the submit after [`Self::try_start`]
    /// failed, so the job never reached the network.
    pub fn remove(&self, job_id: Uuid) {
        self.jobs.lock().remove(&job_id);
    }

    /// Sum of running jobs' commitments for one owner, for pre-checking
    /// the next start against a budget that can't see in-flight work.
    pub fn active_committed(&self, owner: &str) -> u64 {
        self.jobs
            .lock()
            .values()
            .filter(|e| e.owner == owner && e.outcome.is_none())
            .map(|e| e.committed)
            .sum()
    }

    /// Appends one delta from the drain task's `on_chunk` callback.
    /// A no-op for an unknown id: running entries never evict, so that
    /// only means the job already concluded and was removed.
    pub fn append_chunk(&self, job_id: Uuid, text: &str) {
        if let Some(entry) = self.jobs.lock().get_mut(&job_id) {
            entry.chunks.push(text.to_string());
        }
    }

    /// Lands the drain task's terminal payload: the surface's final
    /// result on success, the error message a synchronous call would
    /// have returned otherwise. Readable via [`Self::poll`] until the
    /// linger evicts the entry.
    pub fn conclude(&self, job_id: Uuid, outcome: Result<T, String>) {
        if let Some(entry) = self.jobs.lock().get_mut(&job_id) {
            entry.outcome = Some(outcome);
            entry.concluded_at = Some(Instant::now());
        }
    }

    /// Reads chunks from `since` on, plus the terminal payload once the
    /// job concluded. The owner is checked first: a foreign caller gets
    /// the same [`StreamJobsError::UnknownJob`] a bogus id gets.
    pub fn poll(
        &self,
        owner: &str,
        job_id: Uuid,
        since: u64,
    ) -> Result<StreamJobPoll<T>, StreamJobsError> {
        let mut jobs = self.jobs.lock();
        Self::evict(&mut jobs, self.linger, self.max_concluded);
        let entry = jobs
            .get(&job_id)
            .filter(|e| e.owner == owner)
            .ok_or(StreamJobsError::UnknownJob(job_id))?;
        let start = (since as usize).min(entry.chunks.len());
        Ok(StreamJobPoll {
            chunks: entry.chunks[start..].to_vec(),
            next_seq: entry.chunks.len() as u64,
            outcome: entry.outcome.clone(),
        })
    }

    fn evict(jobs: &mut HashMap<Uuid, Entry<T>>, linger: Duration, max_concluded: usize) {
        jobs.retain(|_, e| match e.concluded_at {
            Some(at) => at.elapsed() <= linger,
            None => true,
        });
        let mut concluded: Vec<(Uuid, Instant)> = jobs
            .iter()
            .filter_map(|(id, e)| e.concluded_at.map(|at| (*id, at)))
            .collect();
        if concluded.len() > max_concluded {
            concluded.sort_by_key(|(_, at)| *at);
            for (id, _) in &concluded[..concluded.len() - max_concluded] {
                jobs.remove(id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker(cap: usize) -> StreamJobs<String> {
        StreamJobs::new(cap)
    }

    #[test]
    fn cursor_returns_only_unseen_chunks_and_never_overruns() {
        let jobs = tracker(4);
        let id = Uuid::new_v4();
        jobs.try_start("alice", id, 100).unwrap();
        for text in ["a", "b", "c"] {
            jobs.append_chunk(id, text);
        }
        let page = jobs.poll("alice", id, 0).unwrap();
        assert_eq!(page.chunks, vec!["a", "b", "c"]);
        assert_eq!(page.next_seq, 3);
        assert!(page.outcome.is_none());
        let page = jobs.poll("alice", id, 2).unwrap();
        assert_eq!(page.chunks, vec!["c"]);
        let past_end = jobs.poll("alice", id, 99).unwrap();
        assert!(past_end.chunks.is_empty());
        assert_eq!(past_end.next_seq, 3);
    }

    #[test]
    fn a_foreign_owner_and_a_bogus_id_read_identically() {
        let jobs = tracker(4);
        let id = Uuid::new_v4();
        jobs.try_start("alice", id, 100).unwrap();
        let foreign = jobs.poll("mallory", id, 0).unwrap_err();
        let bogus_id = Uuid::new_v4();
        let bogus = jobs.poll("alice", bogus_id, 0).unwrap_err();
        assert_eq!(foreign, StreamJobsError::UnknownJob(id));
        assert_eq!(bogus, StreamJobsError::UnknownJob(bogus_id));
        assert_eq!(
            foreign.to_string().replace(&id.to_string(), "<id>"),
            bogus.to_string().replace(&bogus_id.to_string(), "<id>"),
            "wording must not reveal whether the id exists"
        );
    }

    #[test]
    fn the_active_cap_counts_running_jobs_only() {
        let jobs = tracker(2);
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        jobs.try_start("alice", a, 1).unwrap();
        jobs.try_start("alice", b, 1).unwrap();
        let err = jobs.try_start("alice", c, 1).unwrap_err();
        assert_eq!(err, StreamJobsError::ActiveCap { active: 2, cap: 2 });
        // A different owner is unaffected.
        jobs.try_start("bob", Uuid::new_v4(), 1).unwrap();
        // Conclusion frees the slot.
        jobs.conclude(a, Ok("done".into()));
        jobs.try_start("alice", c, 1).unwrap();
    }

    #[test]
    fn active_committed_sums_running_commitments_only() {
        let jobs = tracker(4);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        jobs.try_start("alice", a, 300).unwrap();
        jobs.try_start("alice", b, 500).unwrap();
        jobs.try_start("bob", Uuid::new_v4(), 900).unwrap();
        assert_eq!(jobs.active_committed("alice"), 800);
        jobs.conclude(b, Err("failed".into()));
        assert_eq!(jobs.active_committed("alice"), 300);
    }

    #[test]
    fn a_duplicate_job_id_is_refused() {
        let jobs = tracker(4);
        let id = Uuid::new_v4();
        jobs.try_start("alice", id, 1).unwrap();
        assert_eq!(
            jobs.try_start("alice", id, 1).unwrap_err(),
            StreamJobsError::Duplicate(id)
        );
    }

    #[test]
    fn remove_rolls_a_registration_back_entirely() {
        let jobs = tracker(1);
        let id = Uuid::new_v4();
        jobs.try_start("alice", id, 700).unwrap();
        jobs.remove(id);
        assert_eq!(jobs.active_committed("alice"), 0);
        assert_eq!(
            jobs.poll("alice", id, 0).unwrap_err(),
            StreamJobsError::UnknownJob(id)
        );
        // The freed slot is usable again.
        jobs.try_start("alice", Uuid::new_v4(), 1).unwrap();
    }

    #[test]
    fn a_concluded_outcome_reads_repeatedly_within_the_linger() {
        let jobs = tracker(4);
        let id = Uuid::new_v4();
        jobs.try_start("alice", id, 1).unwrap();
        jobs.append_chunk(id, "hi");
        jobs.conclude(id, Ok("final".into()));
        for _ in 0..2 {
            let page = jobs.poll("alice", id, 0).unwrap();
            assert_eq!(page.chunks, vec!["hi"]);
            assert_eq!(page.outcome, Some(Ok("final".into())));
        }
    }

    #[test]
    fn a_concluded_job_evicts_after_the_linger() {
        let jobs = StreamJobs::<String>::with_limits(4, Duration::ZERO, 256);
        let id = Uuid::new_v4();
        jobs.try_start("alice", id, 1).unwrap();
        jobs.conclude(id, Ok("final".into()));
        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(
            jobs.poll("alice", id, 0).unwrap_err(),
            StreamJobsError::UnknownJob(id)
        );
    }

    #[test]
    fn running_jobs_survive_eviction_even_with_zero_linger() {
        let jobs = StreamJobs::<String>::with_limits(4, Duration::ZERO, 256);
        let id = Uuid::new_v4();
        jobs.try_start("alice", id, 1).unwrap();
        std::thread::sleep(Duration::from_millis(2));
        jobs.append_chunk(id, "still here");
        assert_eq!(
            jobs.poll("alice", id, 0).unwrap().chunks,
            vec!["still here"]
        );
    }

    #[test]
    fn concluded_jobs_beyond_the_hard_cap_evict_oldest_first() {
        let jobs = StreamJobs::<String>::with_limits(10, Duration::from_secs(600), 2);
        let ids: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();
        for id in &ids {
            jobs.try_start("alice", *id, 1).unwrap();
            jobs.conclude(*id, Ok("done".into()));
            std::thread::sleep(Duration::from_millis(2));
        }
        // Any access runs eviction; the oldest concluded entry is gone.
        assert_eq!(
            jobs.poll("alice", ids[0], 0).unwrap_err(),
            StreamJobsError::UnknownJob(ids[0])
        );
        assert!(jobs.poll("alice", ids[1], 0).is_ok());
        assert!(jobs.poll("alice", ids[2], 0).is_ok());
    }

    #[test]
    fn appends_to_unknown_jobs_are_dropped_silently() {
        let jobs = tracker(4);
        jobs.append_chunk(Uuid::new_v4(), "orphan");
        jobs.conclude(Uuid::new_v4(), Ok("orphan".into()));
    }
}
