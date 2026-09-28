//! Model-based differential test over the buyer's streaming-job
//! ledger — the start/poll surface's sibling of purchases_model.rs.
//! The unit tests in `stream_jobs.rs` pin single scenarios; this
//! drives seeded-random interleavings of starts, appends, conclusions
//! (re-conclusions included), rollbacks and cursor polls under a tight
//! per-owner active cap and a small hard cap on retained concluded
//! jobs, checking after every step that the real ledger agrees with a
//! trivial reference model on every page, every error, and every
//! owner's committed spend:
//!
//! - the active cap counts RUNNING jobs only, per owner, and a start
//!   refused for the cap names the exact count it saw;
//! - a job id is tracked at most once — a duplicate start is refused
//!   whether the holder is running or lingering concluded, but an
//!   evicted id may honestly register again;
//! - a foreign owner's poll and a bogus id are the same error;
//! - eviction under the hard cap drops concluded jobs oldest-first —
//!   a re-conclusion refreshes its age — and never touches a running
//!   job, whose chunks keep accumulating and keep serving any cursor;
//! - `active_committed` is exactly the sum of running commitments.
//!
//! Eviction here is driven purely by the hard cap (the linger is far
//! longer than the test), so every step is deterministic: the same
//! hand-rolled splitmix64 as the other book models, and a failure
//! names its seed and op index.

use std::collections::HashMap;
use std::time::Duration;

use covenant_compute_buyer::{StreamJobs, StreamJobsError};
use uuid::Uuid;

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn coin(&mut self) -> bool {
        self.below(2) == 0
    }
}

const CAP: usize = 2;
const MAX_CONCLUDED: usize = 3;

struct ModelEntry {
    owner: usize,
    committed: u64,
    chunks: Vec<String>,
    outcome: Option<Result<String, String>>,
}

/// The reference ledger: a plain map plus the conclusion order the
/// hard cap evicts by. Only [`StreamJobs::try_start`] and
/// [`StreamJobs::poll`] run eviction, so the model evicts at exactly
/// those points and nowhere else.
struct Model {
    entries: HashMap<Uuid, ModelEntry>,
    concluded_order: Vec<Uuid>,
}

impl Model {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            concluded_order: Vec::new(),
        }
    }

    fn evict(&mut self) {
        while self.concluded_order.len() > MAX_CONCLUDED {
            let oldest = self.concluded_order.remove(0);
            self.entries.remove(&oldest);
        }
    }

    fn active(&self, owner: usize) -> usize {
        self.entries
            .values()
            .filter(|e| e.owner == owner && e.outcome.is_none())
            .count()
    }

    fn committed(&self, owner: usize) -> u64 {
        self.entries
            .values()
            .filter(|e| e.owner == owner && e.outcome.is_none())
            .map(|e| e.committed)
            .sum()
    }
}

#[test]
fn random_stream_job_interleavings_match_a_reference_ledger() {
    let owners = ["alice", "bob", "cara"];
    for seed in 0..12u64 {
        let mut rng = Rng::new(0x57E4 ^ (seed << 8));
        let ids: Vec<Uuid> = (0..8).map(|_| Uuid::new_v4()).collect();
        let jobs = StreamJobs::<String>::with_limits(CAP, Duration::from_secs(600), MAX_CONCLUDED);
        let mut model = Model::new();

        for op in 0..140u64 {
            let ctx = format!("seed {seed} op {op}");
            let id = ids[rng.below(ids.len() as u64) as usize];
            let owner = rng.below(owners.len() as u64) as usize;
            match rng.below(100) {
                // Start: refused as a duplicate while ANY holder — the
                // same owner's, a foreign one, running or lingering —
                // still has the id; refused for the cap with the exact
                // running count; otherwise registered.
                0..=29 => {
                    let committed = rng.below(1_000) + 1;
                    model.evict();
                    let expected = if model.entries.contains_key(&id) {
                        Err(StreamJobsError::Duplicate(id))
                    } else {
                        let active = model.active(owner);
                        if active >= CAP {
                            Err(StreamJobsError::ActiveCap { active, cap: CAP })
                        } else {
                            Ok(())
                        }
                    };
                    let got = jobs.try_start(owners[owner], id, committed);
                    assert_eq!(got, expected, "{ctx}: try_start diverges");
                    if expected.is_ok() {
                        model.entries.insert(
                            id,
                            ModelEntry {
                                owner,
                                committed,
                                chunks: Vec::new(),
                                outcome: None,
                            },
                        );
                    }
                }
                // Append: lands on whoever holds the id — running or
                // concluded-and-lingering — and is dropped silently
                // for an evicted or never-started one.
                30..=54 => {
                    let text = format!("c{op}");
                    jobs.append_chunk(id, &text);
                    if let Some(entry) = model.entries.get_mut(&id) {
                        entry.chunks.push(text);
                    }
                }
                // Conclude (or re-conclude): the terminal payload
                // overwrites, and the entry's eviction age refreshes —
                // it becomes the NEWEST concluded job.
                55..=69 => {
                    let outcome = if rng.coin() {
                        Ok(format!("out-{op}"))
                    } else {
                        Err(format!("err-{op}"))
                    };
                    jobs.conclude(id, outcome.clone());
                    if let Some(entry) = model.entries.get_mut(&id) {
                        entry.outcome = Some(outcome);
                        model.concluded_order.retain(|x| *x != id);
                        model.concluded_order.push(id);
                    }
                }
                // Rollback: unregisters unconditionally, running or
                // concluded, freeing the slot and the id.
                70..=74 => {
                    jobs.remove(id);
                    model.entries.remove(&id);
                    model.concluded_order.retain(|x| *x != id);
                }
                // Poll: a random cursor against a random owner. A
                // foreign owner and a bogus id must be the SAME error.
                75..=95 => {
                    let since = match rng.below(3) {
                        0 => 0,
                        1 => rng.below(4),
                        _ => 99,
                    };
                    model.evict();
                    let got = jobs.poll(owners[owner], id, since);
                    match model.entries.get(&id).filter(|e| e.owner == owner) {
                        None => assert_eq!(
                            got.unwrap_err(),
                            StreamJobsError::UnknownJob(id),
                            "{ctx}: poll must refuse a foreign or unknown id"
                        ),
                        Some(entry) => {
                            let page = got.unwrap_or_else(|e| panic!("{ctx}: poll refused: {e}"));
                            let start = (since as usize).min(entry.chunks.len());
                            assert_eq!(
                                page.chunks,
                                entry.chunks[start..].to_vec(),
                                "{ctx}: page diverges"
                            );
                            assert_eq!(
                                page.next_seq,
                                entry.chunks.len() as u64,
                                "{ctx}: cursor diverges"
                            );
                            assert_eq!(page.outcome, entry.outcome, "{ctx}: outcome diverges");
                        }
                    }
                }
                // Full sweep: every owner reads every id from zero —
                // the ledger and the model must agree cell for cell.
                _ => {
                    model.evict();
                    for (owner, name) in owners.iter().enumerate() {
                        for id in &ids {
                            let got = jobs.poll(name, *id, 0);
                            match model.entries.get(id).filter(|e| e.owner == owner) {
                                None => assert_eq!(
                                    got.unwrap_err(),
                                    StreamJobsError::UnknownJob(*id),
                                    "{ctx}: sweep saw a page it must not"
                                ),
                                Some(entry) => {
                                    let page = got.unwrap_or_else(|e| {
                                        panic!("{ctx}: sweep refused {name}/{id}: {e}")
                                    });
                                    assert_eq!(page.chunks, entry.chunks, "{ctx}: sweep page");
                                    assert_eq!(page.outcome, entry.outcome, "{ctx}: sweep outcome");
                                }
                            }
                        }
                    }
                }
            }

            // Standing laws, after every step: committed spend is the
            // sum of running commitments, and the cap was never
            // breached. (Concluded jobs may pile past the hard cap
            // between evictions — only a start or a poll runs one.)
            for (owner, name) in owners.iter().enumerate() {
                assert_eq!(
                    jobs.active_committed(name),
                    model.committed(owner),
                    "{ctx}: committed spend diverges for {name}"
                );
                assert!(
                    model.active(owner) <= CAP,
                    "{ctx}: the cap was breached for {name}"
                );
            }
        }
    }
}
