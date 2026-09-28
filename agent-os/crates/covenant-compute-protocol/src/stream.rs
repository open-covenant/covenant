//! Incremental job output — the live token feed a streaming job
//! ([`crate::JobEnvelopePayload::stream`]) produces while it runs.
//!
//! Trust shape, deliberately weaker than everything else on this wire:
//! chunks are a PREVIEW, not an artifact. They ride the operator's
//! authenticated session to the coordinator and a buyer-signed read
//! back out, but no chunk is individually signed — the signed
//! [`crate::WorkReceiptPayload`] over the final output's hash is what
//! settles money, and a buyer who assembled the chunks can compare
//! them against that verified output afterwards. Signing every token
//! batch would burn signature CPU to certify bytes the receipt already
//! makes checkable.
//!
//! Delivery is best-effort by design: a lost batch, a node that
//! cannot stream, or a coordinator restart degrades a streaming job to
//! exactly the non-streaming experience — wait for the receipt.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One piece of incremental output. `seq` starts at 0 and increments
/// by 1 per chunk over the job's whole stream; the coordinator only
/// appends in order, so a reader holding `next_seq` chunks has the
/// stream's exact prefix — gaps are impossible, not just detectable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamChunk {
    pub seq: u64,
    pub text: String,
}

/// A batch of chunks the executing node pushes to the coordinator
/// (`POST /federation/jobs/:job_id/stream`, operator-session-authed).
/// Batches may be retried verbatim: already-appended seqs are ignored,
/// so a retry after a half-landed push is safe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamPush {
    pub job_id: Uuid,
    pub chunks: Vec<StreamChunk>,
    /// The executor finished producing output — successfully or not —
    /// and no more chunks will follow. Concluding the JOB (receipt,
    /// settlement) stays the result path's business; this only lets a
    /// reader stop polling for more text.
    #[serde(default)]
    pub done: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_push_round_trips_and_defaults_done_to_false() {
        let push = StreamPush {
            job_id: Uuid::new_v4(),
            chunks: vec![
                StreamChunk {
                    seq: 0,
                    text: "hel".into(),
                },
                StreamChunk {
                    seq: 1,
                    text: "lo".into(),
                },
            ],
            done: true,
        };
        let json = serde_json::to_string(&push).unwrap();
        assert_eq!(serde_json::from_str::<StreamPush>(&json).unwrap(), push);

        let bare: StreamPush = serde_json::from_str(
            &serde_json::json!({ "job_id": push.job_id, "chunks": [] }).to_string(),
        )
        .unwrap();
        assert!(!bare.done);
    }
}
