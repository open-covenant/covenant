//! The `FederationEscrow` trait — interface only, no implementation
//! (design-02 §3.3). Whether v1 backs this with a coordinator-custodial
//! ledger or a redesigned on-chain escrow is an open decision (design-02
//! §3.4, §6); this crate defines only the shape both a v1 and a v2
//! backend must satisfy, and the coordinator-signed attestation the
//! operator node checks before executing (design-02 §1.3).

use async_trait::async_trait;
use covenant_identity::LocalIdentity;
use covenant_types::AgentId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::receipt::SignedWorkReceipt;
use crate::sign::{
    sign_domain, to_canonical_json, verify_domain, ProtocolError, ESCROW_HOLD_DOMAIN,
};

/// Whether a payout is funded by the disclosed, time-boxed bootstrap
/// treasury bucket or by real buyer revenue. Every hold/receipt carries
/// this tag so the `subsidy_ratio` discipline (MASTER-PLAN.md) can be
/// computed without guessing which jobs were subsidized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FundingSource {
    Bootstrap,
    Organic,
}

/// The memo an on-chain deposit must carry to attribute itself: this
/// prefix followed by the buyer's compute identity pubkey (base58
/// ed25519 — the key that signs job envelopes, not a wallet address).
/// Wire-shared: the coordinator's rail parses it off transactions, and
/// buyer tooling tells its user to write it. The prefix keeps an
/// unrelated transfer that happens to hit the deposit account from
/// ever crediting anyone.
pub const DEPOSIT_MEMO_PREFIX: &str = "compute-buyer:";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EscrowHoldFields {
    job_id: Uuid,
    amount_micro_usdc: u64,
    funding_source: FundingSource,
    issued_at_ms: u64,
}

/// Coordinator-signed proof that funds are held for `job_id`. The
/// operator verifies this signature before executing (design-02 §1.3)
/// instead of an on-chain payment proof — cheap (one `verify_b58` call,
/// no RPC round-trip) at the cost of trusting the coordinator's word
/// that funds are actually held (design-02 §3.3, §6 — the flagged
/// coordinator-centralization tradeoff).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscrowHoldAttestation {
    pub job_id: Uuid,
    pub amount_micro_usdc: u64,
    pub funding_source: FundingSource,
    pub issued_at_ms: u64,
    pub payload_json: String,
    pub signature_b58: String,
    pub signer_pubkey_b58: String,
}

impl EscrowHoldAttestation {
    pub fn sign(
        job_id: Uuid,
        amount_micro_usdc: u64,
        funding_source: FundingSource,
        issued_at_ms: u64,
        coordinator_identity: &LocalIdentity,
    ) -> Result<Self, ProtocolError> {
        let fields = EscrowHoldFields {
            job_id,
            amount_micro_usdc,
            funding_source,
            issued_at_ms,
        };
        let payload_json = to_canonical_json(&fields)?;
        let (signature_b58, signer_pubkey_b58) =
            sign_domain(coordinator_identity, ESCROW_HOLD_DOMAIN, &payload_json);
        Ok(Self {
            job_id,
            amount_micro_usdc,
            funding_source,
            issued_at_ms,
            payload_json,
            signature_b58,
            signer_pubkey_b58,
        })
    }

    /// Verifies the signature and that it belongs to `expected_coordinator_pubkey_b58`
    /// — the operator's pinned, out-of-band-known coordinator key, not a
    /// key the message itself supplies.
    pub fn verify(&self, expected_coordinator_pubkey_b58: &str) -> Result<(), ProtocolError> {
        if self.signer_pubkey_b58 != expected_coordinator_pubkey_b58 {
            return Err(ProtocolError::Invalid(
                "escrow hold is not signed by the expected coordinator".into(),
            ));
        }
        verify_domain(
            ESCROW_HOLD_DOMAIN,
            &self.payload_json,
            &self.signature_b58,
            &self.signer_pubkey_b58,
        )?;
        let decoded: EscrowHoldFields = serde_json::from_str(&self.payload_json)?;
        let expected = EscrowHoldFields {
            job_id: self.job_id,
            amount_micro_usdc: self.amount_micro_usdc,
            funding_source: self.funding_source,
            issued_at_ms: self.issued_at_ms,
        };
        if decoded != expected {
            return Err(ProtocolError::Invalid(
                "escrow hold payload_json does not match its own fields".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefundReason {
    DeadlineExpired,
    AdmissionFailed,
    OperatorRejected,
    /// The operator submitted a verified receipt whose status is not
    /// `Ok`: the job ran and failed, so the buyer's hold goes back.
    ExecutionFailed,
    /// The buyer withdrew the job before any operator accepted it — no
    /// work was committed, so the hold goes straight back. Never valid
    /// once an operator has accepted; a cancel is not a clawback.
    BuyerCancelled,
    /// A metered job (a lease session) whose meter read zero: the
    /// coordinator never observed the session running, so there is
    /// nothing to bill and the whole escrowed window goes back. The
    /// operator's own timestamps never fill that gap — an unobserved
    /// session is worth zero, however the receipt reads.
    NoMeteredUsage,
    /// An agent task whose result another operator checked and failed:
    /// the patch did not apply, touched a path the buyer protected, or
    /// failed an acceptance command. The builder's work did not pass, so
    /// it earns nothing.
    CheckFailed,
    /// An agent task whose result no operator could check before its
    /// deadline. The work is unverified, so it is not paid, but the
    /// failure is not the builder's.
    CheckUnavailable,
}

impl RefundReason {
    /// The audit-row spelling — the same snake_case string serde
    /// produces, so log rows and wire payloads never disagree.
    pub fn as_str(self) -> &'static str {
        match self {
            RefundReason::DeadlineExpired => "deadline_expired",
            RefundReason::AdmissionFailed => "admission_failed",
            RefundReason::OperatorRejected => "operator_rejected",
            RefundReason::ExecutionFailed => "execution_failed",
            RefundReason::BuyerCancelled => "buyer_cancelled",
            RefundReason::NoMeteredUsage => "no_metered_usage",
            RefundReason::CheckFailed => "check_failed",
            RefundReason::CheckUnavailable => "check_unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscrowStatus {
    Held,
    Released,
    Refunded,
}

#[derive(Debug, thiserror::Error)]
pub enum EscrowError {
    #[error("no hold found for job {0}")]
    NotFound(Uuid),
    #[error("job {0} already settled (released or refunded)")]
    AlreadySettled(Uuid),
    /// A hold already exists for this job id. The ledger is keyed by
    /// job id and a hold is never overwritten, so a repeat `hold` — a
    /// buyer-signed envelope replayed verbatim, or an honest retry of a
    /// submission whose response was lost — is refused here rather than
    /// re-charging the buyer and re-dispatching work already done. The
    /// caller treats it as an idempotent duplicate, not a failure.
    #[error("job {0} already has an escrow hold")]
    AlreadyHeld(Uuid),
    /// The buyer's pre-funded balance cannot cover the hold. Distinct
    /// from `Backend` so the HTTP layer can answer 402 with the exact
    /// shortfall instead of a generic 500.
    #[error(
        "buyer {buyer_pubkey_b58} has insufficient funds: \
         hold needs {needed_micro_usdc} micro-USDC, {available_micro_usdc} available"
    )]
    InsufficientFunds {
        buyer_pubkey_b58: String,
        needed_micro_usdc: u64,
        available_micro_usdc: u64,
    },
    /// A bootstrap-funded hold would push subsidy spend past its
    /// ceiling (the anti-faucet kill-switch), or bootstrap funding has
    /// no policy configured at all. Never raised for organic holds.
    #[error(
        "bootstrap subsidy exhausted: {spent_micro_usdc} of \
         {ceiling_micro_usdc} micro-USDC ceiling already committed"
    )]
    SubsidyExhausted {
        spent_micro_usdc: u64,
        ceiling_micro_usdc: u64,
    },
    #[error("{0}")]
    Backend(String),
}

/// Interface only — no implementation in this crate (design-02 §3.3).
/// Shape is a direct structural match for `services/compute-broker`'s
/// `ComputeProvider` lease lifecycle (`reserve`/`activate`/`cancel`/
/// `reclaim`/`status`, `services/compute-broker/src/providers.ts:14-21`)
/// — same reserve-then-settle-then-release shape, job-escrow domain
/// swapped for GPU-lease domain.
///
/// A v1 backend is a coordinator-custodial ledger; a v2 backend
/// redesigns the on-chain settlement program's task escrow to require a
/// verified [`SignedWorkReceipt`] or a passed deadline instead of the
/// unilateral `has_one = client` release at
/// `programs/settlement/src/lib.rs:931` (design-02 §3.4, §6).
#[async_trait]
pub trait FederationEscrow: Send + Sync {
    async fn hold(
        &self,
        job_id: Uuid,
        buyer: &AgentId,
        amount_micro_usdc: u64,
    ) -> Result<EscrowHoldAttestation, EscrowError>;

    /// Called with a **verified** receipt — never a bare claim (design-02
    /// §3.3: release is mechanical, not discretionary).
    async fn release(&self, job_id: Uuid, receipt: &SignedWorkReceipt) -> Result<(), EscrowError>;

    /// Fires on deadline expiry with no valid receipt, or on admission
    /// failure — mechanical, the inverse of the broken unilateral-release
    /// on-chain pattern this design deliberately avoids.
    async fn refund(&self, job_id: Uuid, reason: RefundReason) -> Result<(), EscrowError>;

    async fn status(&self, job_id: Uuid) -> Result<EscrowStatus, EscrowError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_round_trips() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let held = EscrowHoldAttestation::sign(
            Uuid::new_v4(),
            5_000,
            FundingSource::Organic,
            10,
            &coordinator,
        )
        .expect("sign");
        let pk = bs58::encode(coordinator.pubkey_bytes()).into_string();
        held.verify(&pk).expect("verify");
    }

    #[test]
    fn verify_rejects_unexpected_coordinator() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let other = LocalIdentity::generate("other@local");
        let held = EscrowHoldAttestation::sign(
            Uuid::new_v4(),
            5_000,
            FundingSource::Bootstrap,
            10,
            &coordinator,
        )
        .expect("sign");
        let other_pk = bs58::encode(other.pubkey_bytes()).into_string();
        assert!(held.verify(&other_pk).is_err());
    }

    #[test]
    fn refund_reason_as_str_matches_its_serde_spelling() {
        // Audit rows are written through `as_str`, the wire through
        // serde. If the two ever spelled a reason differently, a refund
        // would read one way in an operator's log and another on the
        // buyer's receipt. Every variant must agree.
        for reason in [
            RefundReason::DeadlineExpired,
            RefundReason::AdmissionFailed,
            RefundReason::OperatorRejected,
            RefundReason::ExecutionFailed,
            RefundReason::BuyerCancelled,
            RefundReason::NoMeteredUsage,
        ] {
            assert_eq!(
                serde_json::to_value(reason).unwrap(),
                serde_json::Value::String(reason.as_str().to_string()),
                "{reason:?}"
            );
        }
    }

    #[test]
    fn verify_rejects_tampered_amount() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let mut held = EscrowHoldAttestation::sign(
            Uuid::new_v4(),
            5_000,
            FundingSource::Organic,
            10,
            &coordinator,
        )
        .expect("sign");
        held.amount_micro_usdc = 999_999_999;
        let pk = bs58::encode(coordinator.pubkey_bytes()).into_string();
        assert!(held.verify(&pk).is_err());
    }
}
