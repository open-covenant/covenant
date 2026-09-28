//! Model-based differential test over the stream relay buffer — the
//! books_model.rs treatment for the one hot-path book that is
//! deliberately NOT journaled. What a buyer's feed-integrity check
//! ultimately trusts is this buffer's arithmetic: chunks form a
//! gapless prefix in seq order, a half-landed push retried verbatim
//! never duplicates a token, the byte ceiling clips instead of
//! growing without bound, and eviction forgets whole streams — never
//! part of one.
//!
//! The unit tests in `stream.rs` pin each behavior once, on
//! whole-batch pushes. This drives random MIXED batches — overlapping
//! retries that cross into new seqs, gaps mid-batch, ceiling trips
//! mid-batch, empty keep-alive pushes, evictions that reset a job's
//! seq expectations — against a trivial model, because the append
//! loop's semantics are per-chunk (skip / refuse / clip / keep) and
//! only interleavings exercise how those arms compose:
//!
//! - a chunk already held is skipped, and the REST of its batch still
//!   lands (a retry after a half-landed push delivers its tail);
//! - a gap refuses the batch from the offending chunk on — what came
//!   before it stays, `done` does not land;
//! - past the ceiling everything drops, but `done` still lands;
//! - every append stamps the idle clock, even an empty one.
//!
//! Deterministic on purpose: the same hand-rolled splitmix64 as the
//! other book models, so a failure names its seed and op index.

use std::collections::BTreeMap;

use covenant_compute_coordinator::stream::MAX_STREAM_BUFFER_BYTES;
use covenant_compute_coordinator::{StreamBook, StreamError, StreamReadout};
use covenant_compute_protocol::StreamChunk;
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

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }
}

#[derive(Default)]
struct StreamModel {
    chunks: Vec<StreamChunk>,
    bytes: usize,
    done: bool,
    truncated: bool,
    updated_at_ms: u64,
}

impl StreamModel {
    /// The append loop's per-chunk arms, restated: skip a held seq,
    /// refuse a gap (keeping what landed before it), clip at the
    /// ceiling, keep the rest. Returns whether the batch completed —
    /// only then does `done` land.
    fn append(&mut self, batch: &[StreamChunk], done: bool, now_ms: u64) -> Option<(u64, u64)> {
        self.updated_at_ms = now_ms;
        for chunk in batch {
            if self.truncated {
                break;
            }
            let expected = self.chunks.len() as u64;
            if chunk.seq < expected {
                continue;
            }
            if chunk.seq > expected {
                return Some((expected, chunk.seq));
            }
            if self.bytes + chunk.text.len() > MAX_STREAM_BUFFER_BYTES {
                self.truncated = true;
                break;
            }
            self.bytes += chunk.text.len();
            self.chunks.push(chunk.clone());
        }
        if done {
            self.done = true;
        }
        None
    }

    fn readout(&self, since_seq: u64) -> StreamReadout {
        let start = (since_seq as usize).min(self.chunks.len());
        StreamReadout {
            chunks: self.chunks[start..].to_vec(),
            next_seq: self.chunks.len() as u64,
            done: self.done,
            truncated: self.truncated,
        }
    }
}

/// The relay under contention — the raced companion to the sequential
/// model below. Per job, one ordered pusher whose every batch is also
/// re-pushed by a racing verbatim retry task (ending on a stale
/// `done: false` retry racing the closing batch), two cursor readers
/// polling concurrently, and an eviction sweep hammering the book over
/// a set of long-dead streams. The model pins WHAT the per-chunk arms
/// do; this pins that no interleaving of the real lock can tear it:
///
/// - every reader assembles the exact scripted text — no gap, no
///   duplicate, no reorder, no cross-stream bleed;
/// - a cursor never regresses, and a readout always answers exactly
///   `next_seq == cursor + chunks.len()`;
/// - `done` is a latch: once any reader has seen it, no later readout
///   may show it unset, whatever stale retry lands after the close;
/// - the sweep drops the dead streams and never touches a live one.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn racing_pushers_readers_and_eviction_never_tear_a_stream() {
    use std::sync::Arc;

    use covenant_compute_coordinator::stream::STREAM_LINGER_MS;

    const JOBS: usize = 6;
    const CHUNKS: u64 = 48;

    fn scripted(job_ix: usize, seq: u64) -> String {
        format!("j{job_ix}s{seq};")
    }

    let book = Arc::new(StreamBook::new());
    let now_ms = STREAM_LINGER_MS * 3;

    // Streams from a previous era, idle far past the linger — the
    // eviction task's prey while the live pushes race it.
    let dead: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
    for (i, job) in dead.iter().enumerate() {
        book.append(
            *job,
            &[StreamChunk {
                seq: 0,
                text: format!("dead-{i}"),
            }],
            false,
            1,
        )
        .unwrap();
    }

    let jobs: Vec<Uuid> = (0..JOBS).map(|_| Uuid::new_v4()).collect();
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    for (job_ix, job) in jobs.iter().copied().enumerate() {
        // The pusher: ordered batches of 1..=3 chunks, each also handed
        // to a detached verbatim-retry task; the final batch closes the
        // stream, then a stale retry of the first batch — done: false —
        // races everything still in flight.
        let pusher_book = book.clone();
        let mut retries: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        tasks.push(tokio::spawn(async move {
            let mut seq = 0u64;
            let mut first_batch: Option<Vec<StreamChunk>> = None;
            while seq < CHUNKS {
                let len = 1 + (seq % 3);
                let batch: Vec<StreamChunk> = (seq..(seq + len).min(CHUNKS))
                    .map(|s| StreamChunk {
                        seq: s,
                        text: scripted(job_ix, s),
                    })
                    .collect();
                seq += batch.len() as u64;
                let done = seq == CHUNKS;
                pusher_book
                    .append(job, &batch, done, now_ms)
                    .expect("an in-order batch must land");
                first_batch.get_or_insert_with(|| batch.clone());
                let retry_book = pusher_book.clone();
                retries.push(tokio::spawn(async move {
                    retry_book
                        .append(job, &batch, done, now_ms)
                        .expect("a verbatim retry must be skipped, not refused");
                }));
                tokio::task::yield_now().await;
            }
            let stale = first_batch.expect("at least one batch pushed");
            pusher_book
                .append(job, &stale, false, now_ms)
                .expect("a stale retry after the close must be skipped");
            for retry in retries {
                retry.await.unwrap();
            }
        }));

        // Two independent readers per job, polling by cursor.
        for reader_ix in 0..2 {
            let reader_book = book.clone();
            tasks.push(tokio::spawn(async move {
                let mut cursor = 0u64;
                let mut seen_done = false;
                let mut assembled = String::new();
                for _ in 0..200_000u64 {
                    let readout = reader_book.read_from(job, cursor);
                    assert!(
                        readout.next_seq >= cursor,
                        "job {job_ix} reader {reader_ix}: cursor regressed from {cursor} to {}",
                        readout.next_seq
                    );
                    assert_eq!(
                        readout.next_seq,
                        cursor + readout.chunks.len() as u64,
                        "job {job_ix} reader {reader_ix}: readout length and cursor disagree"
                    );
                    assert!(!readout.truncated, "job {job_ix}: nothing here may clip");
                    if seen_done {
                        assert!(
                            readout.done,
                            "job {job_ix} reader {reader_ix}: done unlatched"
                        );
                    }
                    seen_done |= readout.done;
                    for (i, chunk) in readout.chunks.iter().enumerate() {
                        let expected_seq = cursor + i as u64;
                        assert_eq!(
                            chunk.seq, expected_seq,
                            "job {job_ix} reader {reader_ix}: chunk out of order"
                        );
                        assert_eq!(
                            chunk.text,
                            scripted(job_ix, expected_seq),
                            "job {job_ix} reader {reader_ix}: foreign or torn chunk text"
                        );
                        assembled.push_str(&chunk.text);
                    }
                    cursor = readout.next_seq;
                    if seen_done && cursor == CHUNKS {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                let full: String = (0..CHUNKS).map(|s| scripted(job_ix, s)).collect();
                assert_eq!(
                    assembled, full,
                    "job {job_ix} reader {reader_ix}: assembly is not the scripted stream"
                );
            }));
        }
    }

    // The sweep races the live traffic; only the dead may go.
    let sweep_book = book.clone();
    tasks.push(tokio::spawn(async move {
        for _ in 0..64 {
            sweep_book.evict_idle(now_ms, STREAM_LINGER_MS);
            tokio::task::yield_now().await;
        }
    }));

    for task in tasks {
        task.await.unwrap();
    }

    for job in &dead {
        assert_eq!(
            book.read_from(*job, 0),
            StreamReadout::default(),
            "a dead stream survived the racing sweep"
        );
    }
    for (job_ix, job) in jobs.iter().enumerate() {
        let readout = book.read_from(*job, 0);
        assert_eq!(readout.next_seq, CHUNKS);
        assert!(readout.done, "job {job_ix} must conclude done");
        assert!(!readout.truncated);
    }
}

#[test]
fn random_chunk_batches_relay_a_gapless_prefix_and_nothing_else() {
    for seed in 0..20u64 {
        let mut rng = Rng::new(0x57EA ^ (seed << 8));
        let book = StreamBook::new();
        let jobs: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();
        let mut model: BTreeMap<Uuid, StreamModel> = BTreeMap::new();
        // A chunk heavy enough that three of them cross the 4 MiB
        // ceiling, so truncation actually happens mid-sequence.
        let heavy = "x".repeat(MAX_STREAM_BUFFER_BYTES / 3 + 1);

        for op in 0..120u64 {
            let now = op;
            let ctx = format!("seed {seed} op {op}");
            match rng.below(100) {
                // Append a mixed batch: starting anywhere from three
                // seqs behind the prefix (a retry) to one past it (a
                // gap), sometimes empty, sometimes heavy, sometimes
                // closing the stream.
                0..=59 => {
                    let job = *rng.pick(&jobs);
                    let entry = model.entry(job).or_default();
                    let held = entry.chunks.len() as u64;
                    let start = if rng.below(5) == 0 {
                        held + 1
                    } else {
                        held.saturating_sub(rng.below(3))
                    };
                    let len = rng.below(4) as usize;
                    let batch: Vec<StreamChunk> = (0..len)
                        .map(|i| StreamChunk {
                            seq: start + i as u64,
                            text: if rng.below(12) == 0 {
                                heavy.clone()
                            } else {
                                (*rng.pick(&["", "tok ", "delta"])).into()
                            },
                        })
                        .collect();
                    let done = rng.below(6) == 0;
                    let got = book.append(job, &batch, done, now);
                    match entry.append(&batch, done, now) {
                        None => got.unwrap_or_else(|e| panic!("{ctx}: batch refused: {e}")),
                        Some((expected, at)) => match got {
                            Err(StreamError::OutOfOrder {
                                job_id,
                                expected: e,
                                got: g,
                            }) => {
                                assert_eq!(
                                    (job_id, e, g),
                                    (job, expected, at),
                                    "{ctx}: gap named wrong"
                                );
                            }
                            other => panic!("{ctx}: gap not refused: {other:?}"),
                        },
                    }
                }
                // Cursor probe anywhere in and past the prefix.
                60..=79 => {
                    let job = *rng.pick(&jobs);
                    let held = model.get(&job).map(|s| s.chunks.len() as u64).unwrap_or(0);
                    let since = rng.below(held + 3);
                    let got = book.read_from(job, since);
                    let want = model
                        .get(&job)
                        .map(|s| s.readout(since))
                        .unwrap_or_default();
                    assert_eq!(got, want, "{ctx}: cursor read diverges");
                }
                // Eviction forgets exactly the streams idle past the
                // linger — and a later append starts them over at seq
                // zero.
                80..=89 => {
                    let linger = *rng.pick(&[0, 5, 20]);
                    let evicted = book.evict_idle(now, linger);
                    let before = model.len();
                    model.retain(|_, s| now.saturating_sub(s.updated_at_ms) <= linger);
                    assert_eq!(
                        evicted,
                        before - model.len(),
                        "{ctx}: eviction count diverges"
                    );
                }
                // A job nobody ever streamed reads as an empty live
                // stream, and stays that way.
                _ => {
                    let readout = book.read_from(Uuid::new_v4(), rng.below(4));
                    assert_eq!(
                        readout,
                        StreamReadout::default(),
                        "{ctx}: stranger not empty"
                    );
                }
            }

            // The full differential: every pooled job's whole state,
            // byte ceiling included, recomputed by the model.
            for job in &jobs {
                let got = book.read_from(*job, 0);
                let want = model.get(job).map(|s| s.readout(0)).unwrap_or_default();
                assert_eq!(got, want, "{ctx}: stream {job} diverges");
                if let Some(s) = model.get(job) {
                    assert!(
                        s.bytes <= MAX_STREAM_BUFFER_BYTES,
                        "{ctx}: buffered bytes past the ceiling"
                    );
                }
            }
        }
    }
}
