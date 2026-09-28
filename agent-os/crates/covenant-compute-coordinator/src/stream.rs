//! In-memory relay buffer for streaming jobs' incremental output (the
//! envelope's `stream` flag): the assigned operator appends
//! seq-numbered chunk batches, the buyer drains them by cursor.
//!
//! Deliberately NOT journaled, unlike every money-bearing book here: a
//! chunk is a live preview whose durable form is the job record's
//! verified final output, so replaying half a stream after a restart
//! would cost a journal write per token batch and buy nothing the
//! receipt poll doesn't already serve. A coordinator restart mid-job
//! degrades a streaming buyer to exactly the non-streaming experience.

use std::collections::HashMap;

use covenant_compute_protocol::StreamChunk;
use parking_lot::Mutex;
use uuid::Uuid;

/// Per-job ceiling on buffered stream text, mirroring the node
/// executors' 4 MiB output caps. Past it the stream is marked
/// truncated and further chunks are dropped: unlike an executor, the
/// relay CAN clip safely, because the job's final output still arrives
/// whole through the receipt path and the readout says it was clipped.
pub const MAX_STREAM_BUFFER_BYTES: usize = 4 * 1024 * 1024;

/// How long a stream outlives its last append before
/// [`StreamBook::evict_idle`] may drop it — long enough for a buyer to
/// drain the tail after conclusion, short enough that abandoned
/// streams don't accrete in a long-lived coordinator.
pub const STREAM_LINGER_MS: u64 = 10 * 60 * 1000;

#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error(
        "stream for job {job_id} expected seq {expected}, got {got}: \
         chunks append in order so a reader's prefix can never hide a gap"
    )]
    OutOfOrder {
        job_id: Uuid,
        expected: u64,
        got: u64,
    },
}

#[derive(Debug, Default)]
struct JobStream {
    chunks: Vec<StreamChunk>,
    bytes: usize,
    done: bool,
    truncated: bool,
    updated_at_ms: u64,
}

/// A cursor read's result: every chunk at-and-after the cursor, the
/// next cursor value, and the stream's end state.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StreamReadout {
    pub chunks: Vec<StreamChunk>,
    pub next_seq: u64,
    pub done: bool,
    pub truncated: bool,
}

#[derive(Default)]
pub struct StreamBook {
    streams: Mutex<HashMap<Uuid, JobStream>>,
}

impl StreamBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a push's chunks. Seqs the buffer already holds are
    /// skipped, so a batch retried verbatim after a half-landed push
    /// is safe; a seq PAST the expected next one is refused — storing
    /// it would fabricate a gapless prefix the node never sent. Once
    /// the byte ceiling trips, the stream stays truncated and every
    /// further chunk is dropped.
    pub fn append(
        &self,
        job_id: Uuid,
        chunks: &[StreamChunk],
        done: bool,
        now_ms: u64,
    ) -> Result<(), StreamError> {
        let mut guard = self.streams.lock();
        let stream = guard.entry(job_id).or_default();
        stream.updated_at_ms = now_ms;
        for chunk in chunks {
            if stream.truncated {
                break;
            }
            let expected = stream.chunks.len() as u64;
            if chunk.seq < expected {
                continue;
            }
            if chunk.seq > expected {
                return Err(StreamError::OutOfOrder {
                    job_id,
                    expected,
                    got: chunk.seq,
                });
            }
            if stream.bytes + chunk.text.len() > MAX_STREAM_BUFFER_BYTES {
                stream.truncated = true;
                break;
            }
            stream.bytes += chunk.text.len();
            stream.chunks.push(chunk.clone());
        }
        if done {
            stream.done = true;
        }
        Ok(())
    }

    /// Chunks from `since_seq` on. A job with no stream yet reads as an
    /// empty, not-done readout — indistinguishable from "started but
    /// nothing produced", which is exactly what a poller should see.
    pub fn read_from(&self, job_id: Uuid, since_seq: u64) -> StreamReadout {
        let guard = self.streams.lock();
        let Some(stream) = guard.get(&job_id) else {
            return StreamReadout::default();
        };
        let start = (since_seq as usize).min(stream.chunks.len());
        StreamReadout {
            chunks: stream.chunks[start..].to_vec(),
            next_seq: stream.chunks.len() as u64,
            done: stream.done,
            truncated: stream.truncated,
        }
    }

    /// Drops streams idle past `linger_ms`; returns how many went.
    pub fn evict_idle(&self, now_ms: u64, linger_ms: u64) -> usize {
        let mut guard = self.streams.lock();
        let before = guard.len();
        guard.retain(|_, s| now_ms.saturating_sub(s.updated_at_ms) <= linger_ms);
        before - guard.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(seq: u64, text: &str) -> StreamChunk {
        StreamChunk {
            seq,
            text: text.into(),
        }
    }

    #[test]
    fn appends_read_back_from_any_cursor() {
        let book = StreamBook::new();
        let job = Uuid::new_v4();
        book.append(job, &[chunk(0, "a"), chunk(1, "b")], false, 1)
            .unwrap();
        book.append(job, &[chunk(2, "c")], true, 2).unwrap();

        let all = book.read_from(job, 0);
        assert_eq!(all.chunks.len(), 3);
        assert_eq!(all.next_seq, 3);
        assert!(all.done);
        assert!(!all.truncated);

        let tail = book.read_from(job, 2);
        assert_eq!(tail.chunks, vec![chunk(2, "c")]);

        let past_end = book.read_from(job, 10);
        assert!(past_end.chunks.is_empty());
        assert_eq!(past_end.next_seq, 3);
    }

    #[test]
    fn a_verbatim_retry_is_ignored_not_duplicated() {
        let book = StreamBook::new();
        let job = Uuid::new_v4();
        let batch = [chunk(0, "a"), chunk(1, "b")];
        book.append(job, &batch, false, 1).unwrap();
        book.append(job, &batch, false, 2).unwrap();
        let all = book.read_from(job, 0);
        assert_eq!(all.chunks.len(), 2);
    }

    #[test]
    fn a_gap_is_refused() {
        let book = StreamBook::new();
        let job = Uuid::new_v4();
        book.append(job, &[chunk(0, "a")], false, 1).unwrap();
        let err = book
            .append(job, &[chunk(2, "c")], false, 2)
            .expect_err("seq 1 is missing");
        assert!(matches!(
            err,
            StreamError::OutOfOrder {
                expected: 1,
                got: 2,
                ..
            }
        ));
    }

    #[test]
    fn an_unknown_job_reads_as_an_empty_live_stream() {
        let book = StreamBook::new();
        let readout = book.read_from(Uuid::new_v4(), 0);
        assert_eq!(readout, StreamReadout::default());
        assert!(!readout.done);
    }

    #[test]
    fn past_the_byte_ceiling_the_stream_marks_truncated_and_drops() {
        let book = StreamBook::new();
        let job = Uuid::new_v4();
        let big = "x".repeat(MAX_STREAM_BUFFER_BYTES - 1);
        book.append(job, &[chunk(0, &big)], false, 1).unwrap();
        book.append(job, &[chunk(1, "overflow")], false, 2).unwrap();
        let readout = book.read_from(job, 0);
        assert!(readout.truncated);
        assert_eq!(readout.chunks.len(), 1, "the overflowing chunk dropped");

        // Whatever arrives after truncation is dropped without fuss —
        // including seqs that would otherwise read as gaps.
        book.append(job, &[chunk(5, "late")], true, 3).unwrap();
        let readout = book.read_from(job, 0);
        assert_eq!(readout.next_seq, 1);
        assert!(readout.done, "done still lands on a truncated stream");
    }

    #[test]
    fn evict_idle_drops_only_streams_past_the_linger() {
        let book = StreamBook::new();
        let old = Uuid::new_v4();
        let fresh = Uuid::new_v4();
        book.append(old, &[chunk(0, "a")], true, 1_000).unwrap();
        book.append(fresh, &[chunk(0, "b")], false, 90_000).unwrap();

        let evicted = book.evict_idle(100_000, 50_000);
        assert_eq!(evicted, 1);
        assert!(book.read_from(old, 0).chunks.is_empty());
        assert_eq!(book.read_from(fresh, 0).chunks.len(), 1);
    }
}
