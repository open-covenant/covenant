//! A buyer's signed close of a running lease session, and the access
//! grant a node publishes when the session comes up.
//!
//! A lease is the one job kind the buyer ends. Batch and inference jobs
//! finish when the work is done; a session runs until someone stops it,
//! and every second it runs is billed. So the buyer needs a way to say
//! "I'm done" that stops the meter immediately — without it the only
//! exit is the window's own expiry, which bills the whole ceiling the
//! escrow was holding and makes early release meaningless.
//!
//! [`LeaseCloseRequest`] is the same wrap-don't-embed, domain-separated
//! shape as [`crate::cancel::CancelRequest`] — a relay can neither
//! forge a close nor re-point one at another job — but the two are
//! opposites: a cancel is strictly pre-commitment and refunds whole; a
//! close is strictly post-acceptance and settles the seconds served.
//!
//! [`LeaseAccess`] travels the other way, as the session's first
//! streamed chunk: the node publishes where the machine is and how to
//! reach it the moment the session is up, so the buyer can use what
//! they are already paying for rather than waiting for a receipt.

use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use covenant_types::AgentId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::sign::{
    sign_domain, to_canonical_json, verify_domain, ProtocolError, LEASE_CLOSE_DOMAIN,
};

/// How far `closed_at_ms` may sit from the coordinator's clock. Same
/// hygiene as the cancel and dispute skews: it bounds how long a
/// captured close stays replayable.
pub const LEASE_CLOSE_MAX_SKEW_MS: u64 = 120_000;
/// Longest an access endpoint string may be — a host:port, a URL, or a
/// short command line, never a document.
pub const MAX_ACCESS_ENDPOINT_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LeaseCloseFields {
    buyer: AgentId,
    job_id: Uuid,
    closed_at_ms: u64,
}

/// The buyer's signed instruction to end a running session now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseCloseRequest {
    pub buyer: AgentId,
    pub job_id: Uuid,
    pub closed_at_ms: u64,
    pub payload_json: String,
    pub signature_b58: String,
    pub signer_pubkey_b58: String,
}

impl LeaseCloseRequest {
    pub fn sign(
        buyer: AgentId,
        job_id: Uuid,
        closed_at_ms: u64,
        identity: &LocalIdentity,
    ) -> Result<Self, ProtocolError> {
        let fields = LeaseCloseFields {
            buyer: buyer.clone(),
            job_id,
            closed_at_ms,
        };
        let payload_json = to_canonical_json(&fields)?;
        let (signature_b58, signer_pubkey_b58) =
            sign_domain(identity, LEASE_CLOSE_DOMAIN, &payload_json);
        Ok(Self {
            buyer,
            job_id,
            closed_at_ms,
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
            LEASE_CLOSE_DOMAIN,
            &self.payload_json,
            &self.signature_b58,
            &self.signer_pubkey_b58,
        )?;
        let decoded: LeaseCloseFields = serde_json::from_str(&self.payload_json)?;
        let expected = LeaseCloseFields {
            buyer: self.buyer.clone(),
            job_id: self.job_id,
            closed_at_ms: self.closed_at_ms,
        };
        if decoded != expected {
            return Err(ProtocolError::Invalid(
                "lease close payload_json does not match its own fields".into(),
            ));
        }
        Ok(())
    }
}

/// Where a live session is and how to reach it — published by the node
/// as the session's first chunk, and echoed on the buyer's lease view.
/// Deliberately transport-agnostic: `endpoint` is whatever the access
/// path hands out (an `ssh user@host -p N` line today, a tunnelled
/// local port later), and `note` carries anything a human needs to use
/// it. No credential material rides here — the buyer's own public key
/// was signed into the lease terms, so what comes back is an address,
/// not a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseAccess {
    pub job_id: Uuid,
    pub endpoint: String,
    /// Coordinator-visible time the session became reachable — what a
    /// buyer's "is it up yet" poll is really asking.
    pub ready_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl LeaseAccess {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.endpoint.trim().is_empty() {
            return Err(ProtocolError::Invalid(
                "lease access endpoint is empty".into(),
            ));
        }
        if self.endpoint.len() > MAX_ACCESS_ENDPOINT_BYTES {
            return Err(ProtocolError::Invalid(format!(
                "lease access endpoint of {} bytes exceeds the {MAX_ACCESS_ENDPOINT_BYTES}-byte cap",
                self.endpoint.len()
            )));
        }
        Ok(())
    }
}

/// The buyer's live view of one lease session: where the machine is,
/// how long it has run, and what it has cost so far. Returned by the
/// coordinator's lease view and close endpoints, so the buyer reads the
/// same meter settlement will use — every figure is derived from the
/// signed terms and the coordinator's own clock, not taken on trust.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseView {
    pub job_id: Uuid,
    /// The job's phase: `offered` while it waits for an operator,
    /// `accepted` while the session runs, then a terminal `completed`,
    /// `failed`, `refunded` or `rejected`.
    pub status: String,
    /// Where the session can be reached, once the node has published it.
    /// `None` while the machine is still coming up.
    pub access: Option<LeaseAccess>,
    /// The signed terms the meter runs against.
    pub rate_micro_usdc_per_sec: u64,
    pub max_duration_secs: u64,
    /// Session time the coordinator has observed, in milliseconds:
    /// running for a live lease, final for a settled one.
    pub elapsed_ms: u64,
    /// What the lease has cost at `elapsed_ms` under its signed terms.
    /// For a settled lease this is exactly what was released.
    pub charged_micro_usdc: u64,
    /// Set once the buyer asked to end the session.
    pub close_requested: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct LeaseAccessBlock {
    lease_access: LeaseAccess,
}

/// Packs a validated access grant as a chunk payload.
pub fn lease_access_chunk(access: LeaseAccess) -> Result<Content, ProtocolError> {
    access.validate()?;
    let value = serde_json::to_value(LeaseAccessBlock {
        lease_access: access,
    })
    .expect("lease access serializes infallibly");
    Ok(Content::json(value))
}

/// Reads an access grant back out of streamed chunks (or job output).
/// `Ok(None)` means no grant has been published yet — the session is
/// still coming up.
pub fn parse_lease_access(content: &[Content]) -> Result<Option<LeaseAccess>, ProtocolError> {
    for item in content {
        let Content::Json { value } = item else {
            continue;
        };
        if value.get("lease_access").is_none() {
            continue;
        }
        let block: LeaseAccessBlock = serde_json::from_value(value.clone())
            .map_err(|e| ProtocolError::Invalid(format!("lease access: {e}")))?;
        block.lease_access.validate()?;
        return Ok(Some(block.lease_access));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access(job_id: Uuid) -> LeaseAccess {
        LeaseAccess {
            job_id,
            endpoint: "ssh renter@203.0.113.7 -p 2222".into(),
            ready_at_ms: 1_700_000_000_000,
            note: Some("session ends when you close the lease".into()),
        }
    }

    #[test]
    fn a_signed_close_verifies_and_resists_tampering() {
        let buyer = LocalIdentity::generate("buyer@local");
        let job_id = Uuid::new_v4();
        let req = LeaseCloseRequest::sign(buyer.agent_id(), job_id, 42, &buyer).unwrap();
        req.verify().unwrap();

        // Re-pointing the close at another job breaks it.
        let mut moved = req.clone();
        moved.job_id = Uuid::new_v4();
        assert!(moved.verify().is_err());

        // So does swapping in another buyer's name.
        let stranger = LocalIdentity::generate("stranger@local");
        let mut impersonated = req.clone();
        impersonated.buyer = stranger.agent_id();
        assert!(impersonated.verify().is_err());
    }

    #[test]
    fn a_close_signed_by_someone_else_never_verifies() {
        let buyer = LocalIdentity::generate("buyer@local");
        let attacker = LocalIdentity::generate("attacker@local");
        let job_id = Uuid::new_v4();
        // The attacker signs a close naming the buyer as the closer.
        let forged = LeaseCloseRequest::sign(buyer.agent_id(), job_id, 7, &attacker).unwrap();
        assert!(forged.verify().is_err());
    }

    #[test]
    fn access_round_trips_through_a_chunk_and_refuses_junk() {
        let job_id = Uuid::new_v4();
        let chunk = lease_access_chunk(access(job_id)).unwrap();
        let parsed = parse_lease_access(&[Content::text("noise"), chunk])
            .unwrap()
            .unwrap();
        assert_eq!(parsed, access(job_id));
        assert!(parse_lease_access(&[Content::text("nothing here")])
            .unwrap()
            .is_none());

        let mut empty = access(job_id);
        empty.endpoint = "   ".into();
        assert!(lease_access_chunk(empty).is_err());

        let mut huge = access(job_id);
        huge.endpoint = "x".repeat(MAX_ACCESS_ENDPOINT_BYTES + 1);
        assert!(lease_access_chunk(huge).is_err());
    }
}
