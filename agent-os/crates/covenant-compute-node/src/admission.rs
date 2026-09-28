//! Job admission (design-02 §1.3): verify the buyer's envelope
//! signature, freshness, the coordinator's escrow-hold attestation, a
//! capability match, and local capacity — all BEFORE executing.
//! Fail-closed at every step; no step is a warning, all are hard
//! rejects.
//!
//! The escrow-proof check is the [`covenant_compute_protocol::FederationEscrow`]
//! trait's output: the operator verifies a coordinator-signed
//! [`EscrowHoldAttestation`], not a raw on-chain payment proof — one
//! `verify_b58` call, no RPC round-trip (design-02 §1.3). Whichever
//! `FederationEscrow` backend the coordinator runs, this is the only
//! shape the node ever needs to check.

use covenant_compute_protocol::{CapabilityProfile, EscrowHoldAttestation, SignedJobEnvelope};
use uuid::Uuid;

/// Freshness window for a job envelope's `issued_at_ms`, mirroring
/// `covenantd`'s own identity-attestation skew check
/// (`covenantd/src/lib.rs:5463-5470`, 120s).
pub const FRESHNESS_WINDOW_MS: u64 = 120_000;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AdmissionError {
    #[error("envelope signature invalid: {0}")]
    BadEnvelopeSignature(String),
    #[error(
        "envelope stale: issued_at_ms={issued_at_ms}, now_ms={now_ms}, skew={skew_ms}ms exceeds {window_ms}ms"
    )]
    Stale {
        issued_at_ms: u64,
        now_ms: u64,
        skew_ms: u64,
        window_ms: u64,
    },
    #[error("escrow hold signature invalid: {0}")]
    BadEscrowSignature(String),
    #[error("escrow hold job_id {hold_job_id} does not match envelope job_id {envelope_job_id}")]
    EscrowJobMismatch {
        hold_job_id: Uuid,
        envelope_job_id: Uuid,
    },
    #[error("escrow hold amount {held} is less than envelope price {price}")]
    EscrowUnderfunded { held: u64, price: u64 },
    #[error("job input malformed: {0}")]
    MalformedInput(String),
    #[error("job offers {offered} micro-USDC, below this operator's ask of {ask}")]
    OfferBelowAsk { offered: u64, ask: u64 },
    #[error("capability mismatch: local profile does not satisfy the job's requirement")]
    CapabilityMismatch,
    #[error("local capacity exhausted: {in_flight} jobs in flight (max {max_in_flight})")]
    CapacityExhausted {
        in_flight: usize,
        max_in_flight: usize,
    },
}

pub struct AdmissionContext<'a> {
    pub local_profile: &'a CapabilityProfile,
    /// The operator's pinned, out-of-band-known coordinator pubkey —
    /// never taken from the message itself.
    pub coordinator_pubkey_b58: &'a str,
    pub max_in_flight: usize,
    pub in_flight: usize,
    pub now_ms: u64,
}

/// Runs every admission check in order, returning on the first failure.
/// Callers must not execute the job unless this returns `Ok`.
pub fn admit_job(
    envelope: &SignedJobEnvelope,
    escrow_hold: &EscrowHoldAttestation,
    ctx: &AdmissionContext<'_>,
) -> Result<(), AdmissionError> {
    envelope
        .verify()
        .map_err(|e| AdmissionError::BadEnvelopeSignature(e.to_string()))?;

    let skew_ms = ctx.now_ms.abs_diff(envelope.payload.issued_at_ms);
    if skew_ms > FRESHNESS_WINDOW_MS {
        return Err(AdmissionError::Stale {
            issued_at_ms: envelope.payload.issued_at_ms,
            now_ms: ctx.now_ms,
            skew_ms,
            window_ms: FRESHNESS_WINDOW_MS,
        });
    }

    escrow_hold
        .verify(ctx.coordinator_pubkey_b58)
        .map_err(|e| AdmissionError::BadEscrowSignature(e.to_string()))?;

    if escrow_hold.job_id != envelope.payload.job_id {
        return Err(AdmissionError::EscrowJobMismatch {
            hold_job_id: escrow_hold.job_id,
            envelope_job_id: envelope.payload.job_id,
        });
    }
    if escrow_hold.amount_micro_usdc < envelope.payload.price_micro_usdc {
        return Err(AdmissionError::EscrowUnderfunded {
            held: escrow_hold.amount_micro_usdc,
            price: envelope.payload.price_micro_usdc,
        });
    }

    // An honest coordinator refuses malformed inference input at
    // submission, so this only fires on a coordinator forwarding what
    // it should have refused — the one party a rejection here should
    // reflect on. Rejecting beats executing: the executor would fail
    // the job on the same parse.
    envelope
        .payload
        .validate_input()
        .map_err(|e| AdmissionError::MalformedInput(e.to_string()))?;

    // The receipt will price this job at the envelope's offer, so an
    // offer below the declared ask is a pay cut — reject it here even
    // though the coordinator's matcher applies the same filter. The
    // operator's price is the operator's to enforce. A lease priced by
    // the GPU-hour floors on its per-hour rate scaled to the metered
    // window, the same figure the matcher cleared it against; comparing
    // the raw per-hour number here would reject every lease the
    // coordinator legitimately routed.
    let ask = ctx.local_profile.price.job_floor_micro_usdc(
        envelope.payload.kind,
        envelope.payload.capability_requirement.max_duration_secs,
    );
    if envelope.payload.price_micro_usdc < ask {
        return Err(AdmissionError::OfferBelowAsk {
            offered: envelope.payload.price_micro_usdc,
            ask,
        });
    }

    if !ctx
        .local_profile
        .satisfies(&envelope.payload.capability_requirement)
    {
        return Err(AdmissionError::CapabilityMismatch);
    }

    if ctx.in_flight >= ctx.max_in_flight {
        return Err(AdmissionError::CapacityExhausted {
            in_flight: ctx.in_flight,
            max_in_flight: ctx.max_in_flight,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
    use covenant_compute_protocol::{
        CapabilityRequirement, FundingSource, HardwareClass, JobEnvelopePayload, JobKind, PriceAsk,
        PriceUnit,
    };
    use covenant_identity::LocalIdentity;
    use covenant_mcp::Content;

    fn profile(operator: &LocalIdentity) -> CapabilityProfile {
        CapabilityProfile {
            operator: operator.agent_id(),
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

    fn envelope(buyer: &LocalIdentity, job_id: Uuid, price: u64, now_ms: u64) -> SignedJobEnvelope {
        let payload = JobEnvelopePayload {
            job_id,
            buyer: buyer.agent_id(),
            kind: JobKind::BatchJob,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: None,
                kind: JobKind::BatchJob,
                max_duration_secs: 30,
                min_reputation_bps: None,
            },
            input: vec![Content::text("do work")],
            price_micro_usdc: price,
            deadline_ms: 60_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "admit-test"),
            issued_at_ms: now_ms,
            referral_code: None,
            stream: false,
        };
        SignedJobEnvelope::sign(payload, buyer).unwrap()
    }

    fn escrow_hold(
        coordinator: &LocalIdentity,
        job_id: Uuid,
        amount: u64,
        now_ms: u64,
    ) -> EscrowHoldAttestation {
        EscrowHoldAttestation::sign(job_id, amount, FundingSource::Organic, now_ms, coordinator)
            .unwrap()
    }

    fn coordinator_pubkey(coordinator: &LocalIdentity) -> String {
        bs58::encode(coordinator.pubkey_bytes()).into_string()
    }

    #[test]
    fn admits_a_well_formed_job() {
        let buyer = LocalIdentity::generate("buyer@local");
        let coordinator = LocalIdentity::generate("coordinator@local");
        let job_id = Uuid::new_v4();
        let env = envelope(&buyer, job_id, 100, 1_000);
        let hold = escrow_hold(&coordinator, job_id, 100, 1_000);
        let p = profile(&buyer);
        let pk = coordinator_pubkey(&coordinator);
        let ctx = AdmissionContext {
            local_profile: &p,
            coordinator_pubkey_b58: &pk,
            max_in_flight: 1,
            in_flight: 0,
            now_ms: 1_000,
        };
        admit_job(&env, &hold, &ctx).expect("should admit");
    }

    #[test]
    fn rejects_tampered_envelope_signature() {
        let buyer = LocalIdentity::generate("buyer@local");
        let coordinator = LocalIdentity::generate("coordinator@local");
        let job_id = Uuid::new_v4();
        let mut env = envelope(&buyer, job_id, 100, 1_000);
        env.payload.price_micro_usdc = 1;
        let hold = escrow_hold(&coordinator, job_id, 100, 1_000);
        let p = profile(&buyer);
        let pk = coordinator_pubkey(&coordinator);
        let ctx = AdmissionContext {
            local_profile: &p,
            coordinator_pubkey_b58: &pk,
            max_in_flight: 1,
            in_flight: 0,
            now_ms: 1_000,
        };
        assert!(matches!(
            admit_job(&env, &hold, &ctx),
            Err(AdmissionError::BadEnvelopeSignature(_))
        ));
    }

    #[test]
    fn rejects_stale_envelope() {
        let buyer = LocalIdentity::generate("buyer@local");
        let coordinator = LocalIdentity::generate("coordinator@local");
        let job_id = Uuid::new_v4();
        let env = envelope(&buyer, job_id, 100, 1_000);
        let hold = escrow_hold(&coordinator, job_id, 100, 1_000);
        let p = profile(&buyer);
        let pk = coordinator_pubkey(&coordinator);
        let ctx = AdmissionContext {
            local_profile: &p,
            coordinator_pubkey_b58: &pk,
            max_in_flight: 1,
            in_flight: 0,
            now_ms: 1_000 + FRESHNESS_WINDOW_MS + 1,
        };
        assert!(matches!(
            admit_job(&env, &hold, &ctx),
            Err(AdmissionError::Stale { .. })
        ));
    }

    #[test]
    fn rejects_escrow_hold_from_an_unexpected_coordinator() {
        let buyer = LocalIdentity::generate("buyer@local");
        let coordinator = LocalIdentity::generate("coordinator@local");
        let impostor_coordinator = LocalIdentity::generate("impostor-coordinator@local");
        let job_id = Uuid::new_v4();
        let env = envelope(&buyer, job_id, 100, 1_000);
        let hold = escrow_hold(&impostor_coordinator, job_id, 100, 1_000);
        let p = profile(&buyer);
        let pk = coordinator_pubkey(&coordinator);
        let ctx = AdmissionContext {
            local_profile: &p,
            coordinator_pubkey_b58: &pk,
            max_in_flight: 1,
            in_flight: 0,
            now_ms: 1_000,
        };
        assert!(matches!(
            admit_job(&env, &hold, &ctx),
            Err(AdmissionError::BadEscrowSignature(_))
        ));
    }

    #[test]
    fn rejects_escrow_hold_for_a_different_job_id() {
        let buyer = LocalIdentity::generate("buyer@local");
        let coordinator = LocalIdentity::generate("coordinator@local");
        let job_id = Uuid::new_v4();
        let env = envelope(&buyer, job_id, 100, 1_000);
        let hold = escrow_hold(&coordinator, Uuid::new_v4(), 100, 1_000);
        let p = profile(&buyer);
        let pk = coordinator_pubkey(&coordinator);
        let ctx = AdmissionContext {
            local_profile: &p,
            coordinator_pubkey_b58: &pk,
            max_in_flight: 1,
            in_flight: 0,
            now_ms: 1_000,
        };
        assert!(matches!(
            admit_job(&env, &hold, &ctx),
            Err(AdmissionError::EscrowJobMismatch { .. })
        ));
    }

    #[test]
    fn rejects_underfunded_escrow_hold() {
        let buyer = LocalIdentity::generate("buyer@local");
        let coordinator = LocalIdentity::generate("coordinator@local");
        let job_id = Uuid::new_v4();
        let env = envelope(&buyer, job_id, 100, 1_000);
        let hold = escrow_hold(&coordinator, job_id, 10, 1_000);
        let p = profile(&buyer);
        let pk = coordinator_pubkey(&coordinator);
        let ctx = AdmissionContext {
            local_profile: &p,
            coordinator_pubkey_b58: &pk,
            max_in_flight: 1,
            in_flight: 0,
            now_ms: 1_000,
        };
        assert!(matches!(
            admit_job(&env, &hold, &ctx),
            Err(AdmissionError::EscrowUnderfunded { .. })
        ));
    }

    #[test]
    fn rejects_an_offer_below_the_local_ask() {
        let buyer = LocalIdentity::generate("buyer@local");
        let coordinator = LocalIdentity::generate("coordinator@local");
        let job_id = Uuid::new_v4();
        let env = envelope(&buyer, job_id, 99, 1_000); // profile asks 100
        let hold = escrow_hold(&coordinator, job_id, 99, 1_000);
        let p = profile(&buyer);
        let pk = coordinator_pubkey(&coordinator);
        let ctx = AdmissionContext {
            local_profile: &p,
            coordinator_pubkey_b58: &pk,
            max_in_flight: 1,
            in_flight: 0,
            now_ms: 1_000,
        };
        assert!(matches!(
            admit_job(&env, &hold, &ctx),
            Err(AdmissionError::OfferBelowAsk {
                offered: 99,
                ask: 100
            })
        ));
    }

    #[test]
    fn rejects_malformed_inference_input() {
        let buyer = LocalIdentity::generate("buyer@local");
        let coordinator = LocalIdentity::generate("coordinator@local");
        let job_id = Uuid::new_v4();
        let mut payload = envelope(&buyer, job_id, 100, 1_000).payload;
        payload.kind = JobKind::InferenceCall;
        payload.capability_requirement.kind = JobKind::InferenceCall;
        payload.input = vec![
            Content::text("hi"),
            Content::json(serde_json::json!({ "generation": { "temperature": 99 } })),
        ];
        let env = SignedJobEnvelope::sign(payload, &buyer).unwrap();
        let hold = escrow_hold(&coordinator, job_id, 100, 1_000);
        let p = profile(&buyer);
        let pk = coordinator_pubkey(&coordinator);
        let ctx = AdmissionContext {
            local_profile: &p,
            coordinator_pubkey_b58: &pk,
            max_in_flight: 1,
            in_flight: 0,
            now_ms: 1_000,
        };
        assert!(matches!(
            admit_job(&env, &hold, &ctx),
            Err(AdmissionError::MalformedInput(_))
        ));
    }

    #[test]
    fn a_junk_json_block_on_a_batch_job_stays_advisory() {
        let buyer = LocalIdentity::generate("buyer@local");
        let coordinator = LocalIdentity::generate("coordinator@local");
        let job_id = Uuid::new_v4();
        let mut payload = envelope(&buyer, job_id, 100, 1_000).payload;
        payload.input = vec![
            Content::text("echo ok"),
            Content::json(serde_json::json!({ "generation": { "temprature": 9 } })),
        ];
        let env = SignedJobEnvelope::sign(payload, &buyer).unwrap();
        let hold = escrow_hold(&coordinator, job_id, 100, 1_000);
        let p = profile(&buyer);
        let pk = coordinator_pubkey(&coordinator);
        let ctx = AdmissionContext {
            local_profile: &p,
            coordinator_pubkey_b58: &pk,
            max_in_flight: 1,
            in_flight: 0,
            now_ms: 1_000,
        };
        admit_job(&env, &hold, &ctx).expect("batch executors never read these blocks");
    }

    #[test]
    fn rejects_capability_mismatch() {
        let buyer = LocalIdentity::generate("buyer@local");
        let coordinator = LocalIdentity::generate("coordinator@local");
        let job_id = Uuid::new_v4();
        let env = envelope(&buyer, job_id, 100, 1_000);
        let hold = escrow_hold(&coordinator, job_id, 100, 1_000);
        let mut p = profile(&buyer);
        p.job_kinds = vec![JobKind::InferenceCall]; // envelope asks for BatchJob
        let pk = coordinator_pubkey(&coordinator);
        let ctx = AdmissionContext {
            local_profile: &p,
            coordinator_pubkey_b58: &pk,
            max_in_flight: 1,
            in_flight: 0,
            now_ms: 1_000,
        };
        assert!(matches!(
            admit_job(&env, &hold, &ctx),
            Err(AdmissionError::CapabilityMismatch)
        ));
    }

    #[test]
    fn rejects_when_at_capacity() {
        let buyer = LocalIdentity::generate("buyer@local");
        let coordinator = LocalIdentity::generate("coordinator@local");
        let job_id = Uuid::new_v4();
        let env = envelope(&buyer, job_id, 100, 1_000);
        let hold = escrow_hold(&coordinator, job_id, 100, 1_000);
        let p = profile(&buyer);
        let pk = coordinator_pubkey(&coordinator);
        let ctx = AdmissionContext {
            local_profile: &p,
            coordinator_pubkey_b58: &pk,
            max_in_flight: 1,
            in_flight: 1,
            now_ms: 1_000,
        };
        assert!(matches!(
            admit_job(&env, &hold, &ctx),
            Err(AdmissionError::CapacityExhausted { .. })
        ));
    }
}
