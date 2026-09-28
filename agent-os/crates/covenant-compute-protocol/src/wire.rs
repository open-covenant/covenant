//! Long-poll wire messages between an operator node and the
//! coordinator — the transport contract both sides implement
//! (design-02 §2, §5's NAT/home-operator solution: neither side ever
//! accepts an inbound connection, both dial out to the coordinator).
//! Every signed message follows the same wrap-don't-embed,
//! domain-separated convention as [`crate::envelope`]/[`crate::receipt`].

use covenant_identity::LocalIdentity;
use covenant_types::AgentId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::capability::CapabilityProfile;
use crate::envelope::SignedJobEnvelope;
use crate::escrow::EscrowHoldAttestation;
use crate::receipt::SignedWorkReceipt;
use crate::sign::{
    sign_domain, to_canonical_json, verify_domain, ProtocolError, HEARTBEAT_DOMAIN, REGISTER_DOMAIN,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorStatus {
    Online,
    Busy,
    Offline,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct RegisterFields {
    profile: CapabilityProfile,
    payout_address: String,
    /// Skipped when `None` so a referral-free payload is byte-identical
    /// to the pre-referral wire shape — old signatures keep verifying.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    referral_code: Option<String>,
}

/// Register message: an operator's signed capability declaration plus
/// payout address, sent once on startup and re-sent on re-registration.
/// `referral_code` is the partner attribution for supply-side
/// rev-share (C8) — inside the signed payload, so a relay can't
/// re-attribute an operator's registration to a different partner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub profile: CapabilityProfile,
    pub payout_address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referral_code: Option<String>,
    pub payload_json: String,
    pub signature_b58: String,
    pub signer_pubkey_b58: String,
}

impl RegisterRequest {
    pub fn sign(
        profile: CapabilityProfile,
        payout_address: String,
        identity: &LocalIdentity,
    ) -> Result<Self, ProtocolError> {
        Self::sign_referred(profile, payout_address, None, identity)
    }

    pub fn sign_referred(
        profile: CapabilityProfile,
        payout_address: String,
        referral_code: Option<String>,
        identity: &LocalIdentity,
    ) -> Result<Self, ProtocolError> {
        let fields = RegisterFields {
            profile: profile.clone(),
            payout_address: payout_address.clone(),
            referral_code: referral_code.clone(),
        };
        let payload_json = to_canonical_json(&fields)?;
        let (signature_b58, signer_pubkey_b58) =
            sign_domain(identity, REGISTER_DOMAIN, &payload_json);
        Ok(Self {
            profile,
            payout_address,
            referral_code,
            payload_json,
            signature_b58,
            signer_pubkey_b58,
        })
    }

    pub fn verify(&self) -> Result<(), ProtocolError> {
        if self.signer_pubkey_b58 != self.profile.operator.pubkey_base58() {
            return Err(ProtocolError::Invalid(
                "signer_pubkey_b58 does not match profile.operator".into(),
            ));
        }
        verify_domain(
            REGISTER_DOMAIN,
            &self.payload_json,
            &self.signature_b58,
            &self.signer_pubkey_b58,
        )?;
        let decoded: RegisterFields = serde_json::from_str(&self.payload_json)?;
        let expected = RegisterFields {
            profile: self.profile.clone(),
            payout_address: self.payout_address.clone(),
            referral_code: self.referral_code.clone(),
        };
        if decoded != expected {
            return Err(ProtocolError::Invalid(
                "payload_json does not match register fields".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterResponse {
    pub accepted: bool,
    /// Opaque session token the coordinator mints on acceptance. Real
    /// shape (bearer token vs. signed capability) is the coordinator
    /// worker's call.
    pub operator_session: Option<String>,
    pub reason: Option<String>,
    /// The marketplace fee this coordinator takes from each release,
    /// in basis points — disclosed at registration so an operator
    /// prices its ask knowing the take. Serde-default for coordinators
    /// predating fee capture (they took nothing).
    #[serde(default)]
    pub fee_bps: u32,
}

/// How far a heartbeat's signed `ts_ms` may sit from the coordinator's
/// clock before it is refused as stale. A heartbeat is a bearer-free
/// signed message that refreshes an operator's liveness, so its own
/// bytes replay verbatim; without a freshness bound a single captured
/// `Online` beat could be replayed forever to keep a crashed node
/// matchable. Matches the other signed-request skew windows
/// (`WITHDRAWAL_MAX_SKEW_MS`, `UNBOND_MAX_SKEW_MS`, …) so an operator
/// whose clock is close enough to transact is close enough to beat.
pub const HEARTBEAT_MAX_SKEW_MS: u64 = 120_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct HeartbeatFields {
    operator: AgentId,
    status: OperatorStatus,
    queue_depth: u32,
    ts_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatRequest {
    pub operator: AgentId,
    pub status: OperatorStatus,
    pub queue_depth: u32,
    pub ts_ms: u64,
    pub payload_json: String,
    pub signature_b58: String,
    pub signer_pubkey_b58: String,
}

impl HeartbeatRequest {
    pub fn sign(
        operator: AgentId,
        status: OperatorStatus,
        queue_depth: u32,
        ts_ms: u64,
        identity: &LocalIdentity,
    ) -> Result<Self, ProtocolError> {
        let fields = HeartbeatFields {
            operator: operator.clone(),
            status,
            queue_depth,
            ts_ms,
        };
        let payload_json = to_canonical_json(&fields)?;
        let (signature_b58, signer_pubkey_b58) =
            sign_domain(identity, HEARTBEAT_DOMAIN, &payload_json);
        Ok(Self {
            operator,
            status,
            queue_depth,
            ts_ms,
            payload_json,
            signature_b58,
            signer_pubkey_b58,
        })
    }

    pub fn verify(&self) -> Result<(), ProtocolError> {
        if self.signer_pubkey_b58 != self.operator.pubkey_base58() {
            return Err(ProtocolError::Invalid(
                "signer_pubkey_b58 does not match operator".into(),
            ));
        }
        verify_domain(
            HEARTBEAT_DOMAIN,
            &self.payload_json,
            &self.signature_b58,
            &self.signer_pubkey_b58,
        )?;
        let decoded: HeartbeatFields = serde_json::from_str(&self.payload_json)?;
        let expected = HeartbeatFields {
            operator: self.operator.clone(),
            status: self.status,
            queue_depth: self.queue_depth,
            ts_ms: self.ts_ms,
        };
        if decoded != expected {
            return Err(ProtocolError::Invalid(
                "payload_json does not match heartbeat fields".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatResponse {
    pub ack: bool,
}

/// A job dispatched to a specific operator: the buyer's signed envelope
/// plus the coordinator's escrow-hold attestation for it. The operator
/// verifies both before executing (design-02 §1.3). Delivered via the
/// outbound long-poll `next-job` call (design-02 §5): the operator
/// dials out and hangs for ~30s waiting for one of these, never accepts
/// an inbound connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobOffer {
    pub envelope: SignedJobEnvelope,
    pub escrow_hold: EscrowHoldAttestation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum JobAccept {
    Accept { job_id: Uuid },
    Reject { job_id: Uuid, reason: String },
}

/// The operator's signed work receipt plus the job's actual output,
/// submitted back to the coordinator as the basis for payout
/// (design-01 §5). `output` itself is not signed — the receipt signs
/// its hash (`result_hash_hex` = [`crate::receipt::output_hash_hex`]),
/// so any party holding both can verify the bytes without trusting
/// whoever relayed them. The coordinator refuses a result whose output
/// doesn't hash to the signed receipt before releasing escrow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobResultMessage {
    pub receipt: SignedWorkReceipt,
    #[serde(default)]
    pub output: Vec<covenant_mcp::Content>,
}

/// How the coordinator settled the job a submitted result concluded —
/// the `submit_result` response body. A 200 alone is only delivery;
/// this is the verdict. `Released` is the only answer that pays.
/// `Refunded` means the hold went back to the buyer — the deadline
/// passed first, or the receipt itself reported a failure — so the
/// operator must not book earnings for the job, however cleanly the
/// result landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultSettlement {
    Released,
    Refunded,
}

/// `submit_result`'s acknowledgment: the job the result concluded, what
/// its escrow settled as, and the gross it released to the operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobResultAck {
    pub job_id: Uuid,
    pub settled: ResultSettlement,
    /// The gross the coordinator released to the operator: a lease's
    /// metered draw, every other kind's whole envelope price. The
    /// operator node sizes its earned credit from this, not the receipt's
    /// envelope price, so a lease that settled on the seconds it ran books
    /// what the coordinator actually paid rather than the escrowed
    /// ceiling. `0` when `settled` is `Refunded` — nothing was released.
    ///
    /// Deliberately not serde-defaulted: a settlement acknowledgment must
    /// state the gross it settled. An ack body that omits this field then
    /// fails to decode rather than reading `0`, which — for a `Released`
    /// job — would silently book real, released earnings at nothing and
    /// conclude the job as paid-in-full-of-zero. A coordinator too old to
    /// carry the field is a wire skew the node must not paper over; the
    /// decode failure routes to the retryable transport path, and the
    /// operator's books reconcile from the coordinator's payout feed. Every
    /// current coordinator sets it on every branch, so a matched pair is
    /// unaffected.
    pub released_gross_micro_usdc: u64,
}

/// The human-facing reason inside a coordinator error response. The
/// coordinator answers every refusal as `{"error": "<reason>"}` (its
/// `ApiError` response shape), so a caller surfaces the reason alone
/// rather than the JSON envelope around it — the difference between
/// reading `raise the price to at least ...` and reading that wrapped in
/// braces and quotes. A body that isn't that shape (an axum-native
/// plain-text 405/415, an upstream proxy's 502, a truncated read) passes
/// through trimmed; an empty body is named rather than left as a
/// dangling colon. Both the buyer client and the operator node render
/// coordinator refusals through this, so the wording never drifts
/// between the two sides of the network.
pub fn coordinator_reason(body: &str) -> String {
    #[derive(Deserialize)]
    struct ErrorBody {
        error: String,
    }
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "(no response body)".to_string();
    }
    serde_json::from_str::<ErrorBody>(trimmed)
        .map(|parsed| parsed.error)
        .unwrap_or_else(|_| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{HardwareClass, JobKind, PriceAsk, PriceUnit};

    fn profile(identity: &LocalIdentity) -> CapabilityProfile {
        CapabilityProfile {
            operator: identity.agent_id(),
            hardware: HardwareClass::CpuOnly,
            vram_gb: 0,
            models_served: vec!["any".into()],
            job_kinds: vec![JobKind::BatchJob],
            price: PriceAsk {
                unit: PriceUnit::PerJob,
                micro_usdc: 100,
            },
            tee_capable: false,
        }
    }

    #[test]
    fn register_request_sign_and_verify_round_trips() {
        let identity = LocalIdentity::generate("operator@local");
        let req =
            RegisterRequest::sign(profile(&identity), "payout-addr-1".into(), &identity).unwrap();
        req.verify().expect("verify");
    }

    #[test]
    fn register_request_verify_rejects_tampered_payout_address() {
        let identity = LocalIdentity::generate("operator@local");
        let mut req =
            RegisterRequest::sign(profile(&identity), "payout-addr-1".into(), &identity).unwrap();
        req.payout_address = "attacker-address".into();
        assert!(req.verify().is_err());
    }

    #[test]
    fn register_request_verify_rejects_an_inflated_profile() {
        let identity = LocalIdentity::generate("operator@local");
        let mut req =
            RegisterRequest::sign(profile(&identity), "payout-addr-1".into(), &identity).unwrap();
        // The operator and payout are left intact, but the advertised
        // capabilities are rewritten to claim VRAM the operator never
        // signed for. The profile rides inside the signed payload, so a
        // relay cannot inflate it to draw work the node can't serve.
        req.profile.vram_gb = 80;
        assert!(req.verify().is_err());
    }

    #[test]
    fn a_registration_signed_by_someone_else_is_rejected() {
        let operator = LocalIdentity::generate("operator@local");
        let impostor = LocalIdentity::generate("impostor@local");
        // Signed by the impostor's key but claiming the operator's
        // profile — internally consistent, so only the signer-vs-
        // profile cross-check refuses the registration (and the payout
        // redirect it would smuggle in).
        let req =
            RegisterRequest::sign(profile(&operator), "impostor-payout".into(), &impostor).unwrap();
        assert!(req.verify().is_err());
    }

    #[test]
    fn a_referral_free_registration_keeps_the_pre_referral_payload_shape() {
        let identity = LocalIdentity::generate("operator@local");
        let req =
            RegisterRequest::sign(profile(&identity), "payout-addr-1".into(), &identity).unwrap();
        assert!(
            !req.payload_json.contains("referral_code"),
            "None must serialize to the old wire bytes so old signatures keep verifying"
        );
        req.verify().expect("verify");
    }

    #[test]
    fn a_referred_registration_signs_the_attribution_and_rejects_reassignment() {
        let identity = LocalIdentity::generate("operator@local");
        let req = RegisterRequest::sign_referred(
            profile(&identity),
            "payout-addr-1".into(),
            Some("partner-a".into()),
            &identity,
        )
        .unwrap();
        req.verify().expect("verify");

        // A relay rewriting the attribution to its own code must fail
        // verification — the referral is inside the signed payload.
        let mut stolen = req.clone();
        stolen.referral_code = Some("partner-b".into());
        assert!(stolen.verify().is_err());
        let mut stripped = req;
        stripped.referral_code = None;
        assert!(stripped.verify().is_err());
    }

    #[test]
    fn heartbeat_request_sign_and_verify_round_trips() {
        let identity = LocalIdentity::generate("operator@local");
        let req = HeartbeatRequest::sign(
            identity.agent_id(),
            OperatorStatus::Online,
            0,
            1_000,
            &identity,
        )
        .unwrap();
        req.verify().expect("verify");
    }

    #[test]
    fn heartbeat_request_verify_rejects_tampered_status() {
        let identity = LocalIdentity::generate("operator@local");
        let mut req = HeartbeatRequest::sign(
            identity.agent_id(),
            OperatorStatus::Online,
            0,
            1_000,
            &identity,
        )
        .unwrap();
        req.status = OperatorStatus::Offline;
        assert!(req.verify().is_err());
    }

    #[test]
    fn a_heartbeat_signed_by_someone_else_is_rejected() {
        let operator = LocalIdentity::generate("operator@local");
        let impostor = LocalIdentity::generate("impostor@local");
        // A forged beat could keep a vanished operator matchable — or
        // declare it Offline and trigger a re-offer of its queue — so
        // the signer must be the operator it speaks for.
        let req = HeartbeatRequest::sign(
            operator.agent_id(),
            OperatorStatus::Offline,
            0,
            1_000,
            &impostor,
        )
        .unwrap();
        assert!(req.verify().is_err());
    }

    #[test]
    fn job_accept_serde_round_trips_both_variants() {
        let accept = JobAccept::Accept {
            job_id: Uuid::new_v4(),
        };
        let json = serde_json::to_string(&accept).unwrap();
        assert_eq!(serde_json::from_str::<JobAccept>(&json).unwrap(), accept);

        let reject = JobAccept::Reject {
            job_id: Uuid::new_v4(),
            reason: "queue full".into(),
        };
        let json = serde_json::to_string(&reject).unwrap();
        assert_eq!(serde_json::from_str::<JobAccept>(&json).unwrap(), reject);
    }

    #[test]
    fn coordinator_reason_unwraps_the_error_envelope() {
        assert_eq!(
            coordinator_reason(r#"{"error":"raise the price to at least 500"}"#),
            "raise the price to at least 500"
        );
        // A body that isn't the ApiError shape (a proxy's plain-text
        // gateway error, an axum-native 415) is not mangled into a parse
        // failure — it passes through trimmed.
        assert_eq!(coordinator_reason("  502 Bad Gateway  "), "502 Bad Gateway");
        assert_eq!(coordinator_reason(""), "(no response body)");
    }

    #[test]
    fn a_result_ack_round_trips_its_released_gross() {
        let ack = JobResultAck {
            job_id: Uuid::new_v4(),
            settled: ResultSettlement::Released,
            released_gross_micro_usdc: 4_200,
        };
        let json = serde_json::to_string(&ack).unwrap();
        assert!(json.contains("released_gross_micro_usdc"));
        assert_eq!(serde_json::from_str::<JobResultAck>(&json).unwrap(), ack);
    }

    #[test]
    fn a_result_ack_missing_its_released_gross_fails_to_decode() {
        // A settlement acknowledgment must state the gross it settled. An
        // ack whose body omits the field — a coordinator too old to carry
        // it, or a proxy that dropped it — must fail to decode, not read a
        // released job's earnings as a silent zero. The node's client maps
        // this 2xx-undecodable body to the retryable transport class.
        let body = format!(r#"{{"job_id":"{}","settled":"released"}}"#, Uuid::new_v4());
        assert!(serde_json::from_str::<JobResultAck>(&body).is_err());
    }
}
