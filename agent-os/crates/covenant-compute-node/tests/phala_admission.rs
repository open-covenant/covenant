//! Buyer-signed Phala pins must survive the final node admission boundary.
//! These tests never provision a resource or claim hardware verification.
use base64::{engine::general_purpose::STANDARD, Engine as _};
use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
use covenant_compute_node::admission::{admit_job, AdmissionContext, AdmissionError};
use covenant_compute_protocol::phala::{
    phala_lease_input, phala_workload, reviewed_phala_wrapper, PHALA_CPU_MODEL,
};
use covenant_compute_protocol::{
    lease_input, CapabilityProfile, CapabilityRequirement, EscrowHoldAttestation, FundingSource,
    HardwareClass, JobEnvelopePayload, JobKind, LeaseTerms, PriceAsk, PriceUnit, SignedJobEnvelope,
};
use covenant_identity::LocalIdentity;
use uuid::Uuid;

const ISSUED: u64 = 1_792_540_800_000;
const DURATION_MS: u64 = 60_000;

struct Fixture {
    buyer: LocalIdentity,
    coordinator: LocalIdentity,
    profile: CapabilityProfile,
    envelope: SignedJobEnvelope,
    hold: EscrowHoldAttestation,
}

impl Fixture {
    fn new() -> Self {
        let buyer = LocalIdentity::generate("buyer@phala-admission");
        let operator = LocalIdentity::generate("operator@phala-admission");
        let coordinator = LocalIdentity::generate("coordinator@phala-admission");
        let key =
            STANDARD.encode([b"\0\0\0\x0bssh-ed25519\0\0\0\x20".as_slice(), &[7; 32]].concat());
        let mut payload = JobEnvelopePayload {
            job_id: Uuid::new_v4(),
            buyer: buyer.agent_id(),
            kind: JobKind::LeaseSession,
            capability_requirement: CapabilityRequirement {
                inference_tariff: None,
                gpu_class: Some("cpu".into()),
                min_vram_gb: None,
                model_id: Some(PHALA_CPU_MODEL.into()),
                kind: JobKind::LeaseSession,
                max_duration_secs: 60,
                min_reputation_bps: None,
            },
            input: vec![lease_input(LeaseTerms {
                max_duration_secs: 60,
                rate_micro_usdc_per_sec: 1,
                client_public_key: Some(format!("ssh-ed25519 {key} buyer")),
            })
            .unwrap()],
            price_micro_usdc: 60,
            deadline_ms: 180_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "phala-admission"),
            issued_at_ms: ISSUED,
            referral_code: None,
            stream: true,
        };
        let (_, spec) = phala_workload(
            &payload,
            &operator.agent_id().pubkey_base58(),
            &reviewed_phala_wrapper(),
            &format!("sha256:{}", "ab".repeat(32)),
        )
        .unwrap();
        payload.input.push(phala_lease_input(spec).unwrap());
        let hold = EscrowHoldAttestation::sign(
            payload.job_id,
            payload.price_micro_usdc,
            FundingSource::Organic,
            ISSUED,
            &coordinator,
        )
        .unwrap();
        let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
        Self {
            buyer,
            coordinator,
            profile: CapabilityProfile {
                operator: operator.agent_id(),
                hardware: HardwareClass::CpuOnly,
                vram_gb: 0,
                models_served: vec![PHALA_CPU_MODEL.into()],
                job_kinds: vec![JobKind::LeaseSession],
                price: PriceAsk {
                    unit: PriceUnit::PerJob,
                    micro_usdc: 1,
                },
                tee_capable: false,
                kind_prices: Vec::new(),
                kind_models: Vec::new(),
                inference_tariffs: Vec::new(),
            },
            envelope,
            hold,
        }
    }

    fn admit(&self, now: u64) -> Result<(), AdmissionError> {
        let coordinator = self.coordinator.agent_id().pubkey_base58();
        admit_job(
            &self.envelope,
            &self.hold,
            &AdmissionContext {
                local_profile: &self.profile,
                coordinator_pubkey_b58: &coordinator,
                max_in_flight: 1,
                in_flight: 0,
                now_ms: now,
            },
        )
    }
}

#[test]
fn phala_admission_requires_the_signed_operator_even_when_capability_matches() {
    let mut f = Fixture::new();
    f.admit(ISSUED).unwrap();
    f.profile.operator = LocalIdentity::generate("other@phala-admission").agent_id();
    assert!(f
        .profile
        .satisfies(&f.envelope.payload.capability_requirement));
    assert!(matches!(
        f.admit(ISSUED),
        Err(AdmissionError::ExecutorRefused(_))
    ));
}

#[test]
fn phala_admission_refuses_the_exact_expiry_even_inside_global_freshness_window() {
    let f = Fixture::new();
    f.admit(ISSUED + DURATION_MS - 1).unwrap();
    assert!(matches!(
        f.admit(ISSUED + DURATION_MS),
        Err(AdmissionError::ExecutorRefused(_))
    ));
    assert!(matches!(
        f.admit(ISSUED + DURATION_MS + 1),
        Err(AdmissionError::ExecutorRefused(_))
    ));
}

#[test]
fn phala_admission_refuses_future_issuance_even_inside_global_clock_skew() {
    let f = Fixture::new();
    // Generic jobs tolerate clock skew. A Phala guest embeds an absolute
    // signed window: starting it before issuance can make readiness precede
    // the buyer's minimum time and extend its actual permitted runtime.
    assert!(matches!(
        f.admit(ISSUED - 1),
        Err(AdmissionError::ExecutorRefused(_))
    ));
    f.admit(ISSUED).unwrap();
}

#[test]
fn a_valid_buyer_signature_does_not_make_missing_or_ambiguous_phala_pins_admissible() {
    for mode in 0..3 {
        let mut f = Fixture::new();
        let payload = &mut f.envelope.payload;
        match mode {
            0 => {
                payload.input.pop();
            }
            1 => payload.input.push(payload.input.last().unwrap().clone()),
            2 => {
                let covenant_mcp::Content::Json { value } = payload.input.last_mut().unwrap()
                else {
                    unreachable!()
                };
                value["phala_lease"]["lease_generation"] = "different-instance".into();
            }
            _ => unreachable!(),
        }
        f.envelope = SignedJobEnvelope::sign(f.envelope.payload.clone(), &f.buyer).unwrap();
        f.envelope.verify().unwrap();
        assert!(
            matches!(f.admit(ISSUED), Err(AdmissionError::MalformedInput(_))),
            "mode {mode}"
        );
    }
}
