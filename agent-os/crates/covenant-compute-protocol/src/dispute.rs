//! A buyer's signed dispute of a completed job (C4). The receipt
//! machinery proves the operator signed what it returned; a dispute is
//! the buyer's counter-attestation that what it returned was not the
//! work — the demand-side analogue of the coordinator's canary verdict,
//! for the jobs no probe covered. Same wrap-don't-embed, domain-
//! separated signing shape as [`crate::wire::HeartbeatRequest`], so a
//! relay can neither forge a dispute nor re-point one at a different
//! job.
//!
//! A dispute moves no money. The escrow released against a verified
//! receipt, and clawing that back on an unverifiable content judgment
//! would make instant-dispute a free-work strategy. What it does is
//! land as a reputation fault against the operator, durably and with
//! the buyer's signature kept as evidence — disputing costs the buyer
//! the job price they already paid, which is what keeps it from being
//! free griefing until stake makes the judgment enforceable (Phase 2).

use covenant_identity::LocalIdentity;
use covenant_types::AgentId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::sign::{sign_domain, to_canonical_json, verify_domain, ProtocolError, DISPUTE_DOMAIN};

/// Ceiling on `reason` — the dispute rides the coordinator's journal
/// and audit chain verbatim, so an unbounded reason is an unbounded
/// write. Enforced at signing AND at verification: a request signed by
/// a patched client still bounces.
pub const MAX_DISPUTE_REASON_BYTES: usize = 2_000;

/// How far `disputed_at_ms` may sit from the coordinator's clock.
/// Bounds how long a captured dispute stays replayable — mostly
/// hygiene, since replaying one re-records a fact already recorded.
pub const DISPUTE_MAX_SKEW_MS: u64 = 120_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DisputeFields {
    buyer: AgentId,
    job_id: Uuid,
    reason: String,
    disputed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisputeRequest {
    pub buyer: AgentId,
    pub job_id: Uuid,
    pub reason: String,
    pub disputed_at_ms: u64,
    pub payload_json: String,
    pub signature_b58: String,
    pub signer_pubkey_b58: String,
}

impl DisputeRequest {
    pub fn sign(
        buyer: AgentId,
        job_id: Uuid,
        reason: String,
        disputed_at_ms: u64,
        identity: &LocalIdentity,
    ) -> Result<Self, ProtocolError> {
        check_reason(&reason)?;
        let fields = DisputeFields {
            buyer: buyer.clone(),
            job_id,
            reason: reason.clone(),
            disputed_at_ms,
        };
        let payload_json = to_canonical_json(&fields)?;
        let (signature_b58, signer_pubkey_b58) =
            sign_domain(identity, DISPUTE_DOMAIN, &payload_json);
        Ok(Self {
            buyer,
            job_id,
            reason,
            disputed_at_ms,
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
        check_reason(&self.reason)?;
        verify_domain(
            DISPUTE_DOMAIN,
            &self.payload_json,
            &self.signature_b58,
            &self.signer_pubkey_b58,
        )?;
        let decoded: DisputeFields = serde_json::from_str(&self.payload_json)?;
        let expected = DisputeFields {
            buyer: self.buyer.clone(),
            job_id: self.job_id,
            reason: self.reason.clone(),
            disputed_at_ms: self.disputed_at_ms,
        };
        if decoded != expected {
            return Err(ProtocolError::Invalid(
                "payload_json does not match dispute fields".into(),
            ));
        }
        Ok(())
    }
}

fn check_reason(reason: &str) -> Result<(), ProtocolError> {
    if reason.trim().is_empty() {
        return Err(ProtocolError::Invalid(
            "dispute reason must not be empty".into(),
        ));
    }
    if reason.len() > MAX_DISPUTE_REASON_BYTES {
        return Err(ProtocolError::Invalid(format!(
            "dispute reason is {} bytes, max {MAX_DISPUTE_REASON_BYTES}",
            reason.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_round_trips() {
        let buyer = LocalIdentity::generate("buyer@local");
        let req = DisputeRequest::sign(
            buyer.agent_id(),
            Uuid::new_v4(),
            "output was unrelated to the prompt".into(),
            1_000,
            &buyer,
        )
        .unwrap();
        req.verify().expect("verify");
    }

    #[test]
    fn a_repointed_or_reworded_dispute_fails_verification() {
        let buyer = LocalIdentity::generate("buyer@local");
        let req = DisputeRequest::sign(
            buyer.agent_id(),
            Uuid::new_v4(),
            "garbage output".into(),
            1_000,
            &buyer,
        )
        .unwrap();

        let mut repointed = req.clone();
        repointed.job_id = Uuid::new_v4();
        assert!(repointed.verify().is_err());

        let mut reworded = req;
        reworded.reason = "different complaint".into();
        assert!(reworded.verify().is_err());
    }

    #[test]
    fn someone_elses_signature_is_rejected() {
        let buyer = LocalIdentity::generate("buyer@local");
        let impostor = LocalIdentity::generate("impostor@local");
        // Signed by the impostor's key but claiming the buyer's
        // identity: the signer/buyer pubkey cross-check catches it.
        let req = DisputeRequest::sign(
            buyer.agent_id(),
            Uuid::new_v4(),
            "not my job".into(),
            1_000,
            &impostor,
        )
        .unwrap();
        assert!(req.verify().is_err());
    }

    #[test]
    fn empty_and_oversized_reasons_bounce_at_both_ends() {
        let buyer = LocalIdentity::generate("buyer@local");
        assert!(DisputeRequest::sign(
            buyer.agent_id(),
            Uuid::new_v4(),
            "   ".into(),
            1_000,
            &buyer
        )
        .is_err());

        let oversized = "x".repeat(MAX_DISPUTE_REASON_BYTES + 1);
        assert!(
            DisputeRequest::sign(buyer.agent_id(), Uuid::new_v4(), oversized, 1_000, &buyer)
                .is_err()
        );

        // A patched client that signed an oversized reason anyway still
        // bounces at verification.
        let mut req =
            DisputeRequest::sign(buyer.agent_id(), Uuid::new_v4(), "ok".into(), 1_000, &buyer)
                .unwrap();
        req.reason = "x".repeat(MAX_DISPUTE_REASON_BYTES + 1);
        assert!(req.verify().is_err());
    }
}
