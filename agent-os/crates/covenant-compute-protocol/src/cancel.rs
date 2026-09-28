//! A buyer's signed cancellation of a job no operator has accepted
//! yet. Submission is escrow-first — the hold lands before matching —
//! so a buyer who changes their mind (or whose agent submitted with an
//! hours-long deadline) would otherwise wait out the deadline sweep to
//! see their money again, with the job blocking their in-flight
//! ceiling the whole way. A cancel withdraws the offer and takes the
//! refund now.
//!
//! It is strictly pre-commitment: once an operator has accepted, the
//! work is theirs to finish and the deadline is the buyer's only out —
//! a cancel is never a clawback of committed compute. Same
//! wrap-don't-embed, domain-separated signing shape as
//! [`crate::dispute::DisputeRequest`], so a relay can neither forge a
//! cancellation nor re-point one at a different job.

use covenant_identity::LocalIdentity;
use covenant_types::AgentId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::sign::{sign_domain, to_canonical_json, verify_domain, ProtocolError, CANCEL_DOMAIN};

/// How far `cancelled_at_ms` may sit from the coordinator's clock.
/// Bounds how long a captured cancellation stays replayable — hygiene,
/// like the dispute skew: replaying one re-answers a refund already
/// taken.
pub const CANCEL_MAX_SKEW_MS: u64 = 120_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CancelFields {
    buyer: AgentId,
    job_id: Uuid,
    cancelled_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelRequest {
    pub buyer: AgentId,
    pub job_id: Uuid,
    pub cancelled_at_ms: u64,
    pub payload_json: String,
    pub signature_b58: String,
    pub signer_pubkey_b58: String,
}

impl CancelRequest {
    pub fn sign(
        buyer: AgentId,
        job_id: Uuid,
        cancelled_at_ms: u64,
        identity: &LocalIdentity,
    ) -> Result<Self, ProtocolError> {
        let fields = CancelFields {
            buyer: buyer.clone(),
            job_id,
            cancelled_at_ms,
        };
        let payload_json = to_canonical_json(&fields)?;
        let (signature_b58, signer_pubkey_b58) =
            sign_domain(identity, CANCEL_DOMAIN, &payload_json);
        Ok(Self {
            buyer,
            job_id,
            cancelled_at_ms,
            payload_json,
            signature_b58,
            signer_pubkey_b58,
        })
    }

    pub fn verify(&self) -> Result<(), ProtocolError> {
        if self.signer_pubkey_b58 != self.buyer.pubkey_base58() {
            return Err(ProtocolError::Invalid(
                "signer_pubkey_b58 does not match buyer".into(),
            ));
        }
        verify_domain(
            CANCEL_DOMAIN,
            &self.payload_json,
            &self.signature_b58,
            &self.signer_pubkey_b58,
        )?;
        let decoded: CancelFields = serde_json::from_str(&self.payload_json)?;
        let expected = CancelFields {
            buyer: self.buyer.clone(),
            job_id: self.job_id,
            cancelled_at_ms: self.cancelled_at_ms,
        };
        if decoded != expected {
            return Err(ProtocolError::Invalid(
                "payload_json does not match cancel fields".into(),
            ));
        }
        Ok(())
    }
}

/// What the coordinator answers a cancellation with — shared here so
/// the buyer's parse fails loudly the moment the coordinator's shape
/// drifts. `refunded_micro_usdc` is the full held amount: nothing was
/// served, so nothing is charged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelView {
    pub job_id: Uuid,
    /// The job's phase after the cancel — always `refunded`, including
    /// for an honest retry of a cancellation whose first answer was
    /// lost.
    pub status: String,
    pub refunded_micro_usdc: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_round_trips() {
        let buyer = LocalIdentity::generate("buyer@local");
        let req = CancelRequest::sign(buyer.agent_id(), Uuid::new_v4(), 1_000, &buyer).unwrap();
        req.verify().expect("verify");
    }

    #[test]
    fn a_repointed_or_redated_cancel_fails_verification() {
        let buyer = LocalIdentity::generate("buyer@local");
        let req = CancelRequest::sign(buyer.agent_id(), Uuid::new_v4(), 1_000, &buyer).unwrap();

        let mut repointed = req.clone();
        repointed.job_id = Uuid::new_v4();
        assert!(repointed.verify().is_err());

        let mut redated = req;
        redated.cancelled_at_ms = 2_000;
        assert!(redated.verify().is_err());
    }

    #[test]
    fn someone_elses_signature_is_rejected() {
        let buyer = LocalIdentity::generate("buyer@local");
        let impostor = LocalIdentity::generate("impostor@local");
        // Signed by the impostor's key but claiming the buyer's
        // identity: the signer/buyer pubkey cross-check catches it.
        let req = CancelRequest::sign(buyer.agent_id(), Uuid::new_v4(), 1_000, &impostor).unwrap();
        assert!(req.verify().is_err());
    }
}
