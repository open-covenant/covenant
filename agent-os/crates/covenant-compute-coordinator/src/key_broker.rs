//! Optional authority for fresh, challenge-bound application-key lease claims.
//!
//! Requests carry a buyer-signed broker challenge, never an assertion to sign.
//! Lease facts come from current durable coordinator state and freshly verified
//! hardware evidence. Key ownership/version policy remains the broker's job.
//! This adapter is disabled unless an explicit broker-id allowlist is configured.

use std::{collections::BTreeSet, sync::Arc};

use axum::{
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use covenant_compute_attestation::binding::{
    HardwareProfile, HardwareScope, PhalaEphemeralIdentity,
};
pub use covenant_compute_key_broker::LeaseAssertionRequest;
use covenant_compute_key_broker::{
    challenge_digest, verify_owner_authorization, BrokerChallenge, LeaseAssertionClaims,
    LeaseState, SignedLeaseAssertion, MAX_ASSERTION_TTL_MS,
};
use uuid::Uuid;

use crate::{hardware::HardwareService, CoordinatorState};

#[derive(Debug, thiserror::Error)]
#[error("key broker lease authorization rejected")]
pub struct LeaseAuthorizationError;

impl IntoResponse for LeaseAuthorizationError {
    fn into_response(self) -> Response {
        (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error":"key broker lease authorization rejected"})),
        )
            .into_response()
    }
}

type Result<T> = std::result::Result<T, LeaseAuthorizationError>;

pub struct KeyBrokerLeaseService {
    state: Arc<CoordinatorState>,
    hardware: Arc<HardwareService>,
    broker_ids: BTreeSet<String>,
    slots: Arc<tokio::sync::Semaphore>,
}

impl KeyBrokerLeaseService {
    fn issue(&self, job_id: Uuid, request: LeaseAssertionRequest) -> Result<SignedLeaseAssertion> {
        let requested_at = crate::epoch_ms();
        let challenge = &request.challenge;
        if challenge.binding.job_id != job_id
            || challenge.binding.lease_id != job_id.to_string()
            || !self.broker_ids.contains(&challenge.broker_id)
        {
            return Err(LeaseAuthorizationError);
        }
        verify_owner_authorization(challenge, &request.buyer_authorization)
            .map_err(|_| LeaseAuthorizationError)?;
        self.require_durable_live_hold(job_id)?;
        self.require_native_recipient(challenge)?;
        let buyer = &challenge.binding.owner_pubkey_b58;
        // Check ownership/profile before expensive vendor re-verification.
        self.hardware
            .key_broker_live_scope(job_id, buyer, requested_at)
            .map_err(|_| LeaseAuthorizationError)?;
        let evidence = self
            .hardware
            .evidence(job_id, buyer, requested_at)
            .map_err(|_| LeaseAuthorizationError)?;
        let now = crate::epoch_ms();
        if now < requested_at || now >= evidence.verdict.expires_at_ms {
            return Err(LeaseAuthorizationError);
        }
        let current_scope = self
            .hardware
            .key_broker_live_scope(job_id, buyer, now)
            .map_err(|_| LeaseAuthorizationError)?;
        if current_scope != evidence.verdict.challenge.scope {
            return Err(LeaseAuthorizationError);
        }
        self.require_durable_live_hold(job_id)?;
        self.require_native_recipient(challenge)?;
        let identity = evidence
            .verdict
            .phala_identity
            .as_ref()
            .ok_or(LeaseAuthorizationError)?;
        let claims = claims_from_verified_scope(
            challenge,
            &current_scope,
            identity,
            evidence.verdict.expires_at_ms,
            now,
        )?;
        self.state
            .escrow()
            .sign_key_broker_claims(claims)
            .map_err(|_| LeaseAuthorizationError)
    }

    fn require_durable_live_hold(&self, job_id: Uuid) -> Result<()> {
        if !self
            .state
            .journal()
            .is_some_and(|journal| journal.is_healthy())
        {
            return Err(LeaseAuthorizationError);
        }
        let job = self
            .state
            .jobs()
            .get(job_id)
            .ok_or(LeaseAuthorizationError)?;
        if job.phase != crate::JobPhase::Accepted || job.close_requested_at_ms.is_some() {
            return Err(LeaseAuthorizationError);
        }
        job.envelope.verify().map_err(|_| LeaseAuthorizationError)?;
        job.escrow_hold
            .verify(&self.state.coordinator_pubkey_b58())
            .map_err(|_| LeaseAuthorizationError)?;
        let Some((amount, source)) = self.state.escrow().hold_info(job_id) else {
            return Err(LeaseAuthorizationError);
        };
        if amount != job.envelope.payload.price_micro_usdc
            || amount != job.escrow_hold.amount_micro_usdc
            || source != job.escrow_hold.funding_source
            || job.escrow_hold.job_id != job_id
            || self
                .state
                .escrow()
                .settled_charge_micro_usdc(job_id)
                .is_some()
        {
            return Err(LeaseAuthorizationError);
        }
        Ok(())
    }

    fn require_native_recipient(&self, challenge: &BrokerChallenge) -> Result<()> {
        let job = self
            .state
            .jobs()
            .get(challenge.binding.job_id)
            .ok_or(LeaseAuthorizationError)?;
        let spec = covenant_compute_protocol::phala::parse_phala_ephemeral_lease_spec(
            &job.envelope.payload.input,
        )
        .map_err(|_| LeaseAuthorizationError)?
        .ok_or(LeaseAuthorizationError)?;
        spec.validate_for_job(&job.envelope.payload)
            .map_err(|_| LeaseAuthorizationError)?;
        let recipient = spec
            .application_key_recipient
            .as_ref()
            .ok_or(LeaseAuthorizationError)?;
        require_recipient_scope(challenge, recipient)
    }
}

fn require_recipient_scope(
    challenge: &BrokerChallenge,
    recipient: &covenant_compute_protocol::phala::ApplicationKeyRecipient,
) -> Result<()> {
    let binding = &challenge.binding;
    if recipient.protocol_version != covenant_compute_key_broker::PROTOCOL_VERSION
        || recipient.broker_id != challenge.broker_id
        || recipient.tenant_id != binding.tenant_id
        || recipient.key_id != binding.key_id
        || recipient.key_version != binding.key_version
        || recipient.purpose != binding.purpose
    {
        return Err(LeaseAuthorizationError);
    }
    Ok(())
}

/// Called only after real hardware re-verification. Kept separate to test exact
/// field binding without manufacturing a successful hardware-verification path.
fn claims_from_verified_scope(
    challenge: &BrokerChallenge,
    scope: &HardwareScope,
    identity: &PhalaEphemeralIdentity,
    verdict_expires_at_ms: u64,
    now: u64,
) -> Result<LeaseAssertionClaims> {
    let binding = &challenge.binding;
    if scope.profile != HardwareProfile::PhalaEphemeralDstack061
        || scope.job_id != binding.job_id
        || binding.lease_id != scope.job_id.to_string()
        || scope.buyer_pubkey_b58 != binding.owner_pubkey_b58
        || scope.operator_pubkey_b58 != binding.operator_pubkey_b58
        || scope.lease_generation != binding.lease_generation
        || scope.workload_sha256 != binding.workload_sha256
        || identity.app_id != binding.phala_app_id
        || identity.instance_id != binding.phala_instance_id
        || now == 0
        || now >= scope.lease_expires_at_ms
        || now >= verdict_expires_at_ms
    {
        return Err(LeaseAuthorizationError);
    }
    Ok(LeaseAssertionClaims {
        challenge_sha256: challenge_digest(challenge).map_err(|_| LeaseAuthorizationError)?,
        // Tenant/key/version/purpose are the owner's signed request, not claims
        // that the coordinator maintains a key registry. The broker checks its
        // immutable key policy independently before trusting this authorization.
        binding: binding.clone(),
        state: LeaseState::Active,
        observed_at_ms: now,
        expires_at_ms: now
            .saturating_add(MAX_ASSERTION_TTL_MS)
            .min(scope.lease_expires_at_ms)
            .min(verdict_expires_at_ms),
        lease_expires_at_ms: scope.lease_expires_at_ms,
    })
}

fn parse_broker_ids(value: &str) -> Result<BTreeSet<String>> {
    let values = value.split(',').map(str::trim).collect::<Vec<_>>();
    if values.is_empty()
        || values.len() > 32
        || values.iter().any(|value| {
            value.is_empty()
                || value.len() > 128
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-.:/".contains(&b))
        })
    {
        return Err(LeaseAuthorizationError);
    }
    let ids = values
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    Ok(ids)
}

pub fn build_from_env(
    state: Arc<CoordinatorState>,
    hardware: Option<Arc<HardwareService>>,
) -> Result<Option<Arc<KeyBrokerLeaseService>>> {
    let Some(ids) = std::env::var("COVENANT_COMPUTE_KEY_BROKER_IDS")
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    if !state.journal().is_some_and(|journal| journal.is_healthy()) {
        return Err(LeaseAuthorizationError);
    }
    Ok(Some(Arc::new(KeyBrokerLeaseService {
        state,
        hardware: hardware.ok_or(LeaseAuthorizationError)?,
        broker_ids: parse_broker_ids(&ids)?,
        slots: Arc::new(tokio::sync::Semaphore::new(2)),
    })))
}

pub fn router(service: Arc<KeyBrokerLeaseService>) -> Router {
    Router::new()
        .route(
            "/federation/key-broker/:job/lease-assertion",
            post(assertion_http),
        )
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(axum::middleware::map_response(
            |mut response: Response| async move {
                response.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-store"),
                );
                response
            },
        ))
        .with_state(service)
}

async fn assertion_http(
    State(service): State<Arc<KeyBrokerLeaseService>>,
    Path(job): Path<Uuid>,
    Json(request): Json<LeaseAssertionRequest>,
) -> Result<Json<SignedLeaseAssertion>> {
    let permit = service
        .slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| LeaseAuthorizationError)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        service.issue(job, request)
    })
    .await
    .map_err(|_| LeaseAuthorizationError)?
    .map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_compute_protocol::{
        CapabilityRequirement, FederationEscrow, JobEnvelopePayload, JobKind, LeaseTerms,
        SignedJobEnvelope,
    };
    use covenant_identity::LocalIdentity;

    fn fixture() -> (BrokerChallenge, HardwareScope, PhalaEphemeralIdentity, u64) {
        let vector: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../deploy/compute-key-broker/guest/challenge-vector.json"
        ))
        .unwrap();
        let mut challenge: BrokerChallenge =
            serde_json::from_value(vector["challenge"].clone()).unwrap();
        challenge.binding.lease_id = challenge.binding.job_id.to_string();
        let b = &challenge.binding;
        let now = 1_800_000_000_000;
        let scope: HardwareScope = serde_json::from_value(serde_json::json!({
            "profile":"phala_ephemeral_dstack061",
            "job_id":b.job_id,
            "operator_pubkey_b58":b.operator_pubkey_b58,
            "buyer_pubkey_b58":b.owner_pubkey_b58,
            "lease_generation":b.lease_generation,
            "host":"relay.example.com", "port":443,
            "transport":{"kind":"outbound_relay"}, "username":"covenant",
            "guest_channel_key":{"algorithm":"ssh-ed25519","key_base64":"fixture"},
            "workload_sha256":b.workload_sha256,
            "lease_expires_at_ms":now+120_000
        }))
        .unwrap();
        let identity = PhalaEphemeralIdentity {
            app_id: b.phala_app_id.clone(),
            instance_id: b.phala_instance_id.clone(),
        };
        (challenge, scope, identity, now)
    }

    #[test]
    fn assertion_copies_only_exact_verified_lease_scope_and_caps_lifetime() {
        let (challenge, scope, identity, now) = fixture();
        let claims =
            claims_from_verified_scope(&challenge, &scope, &identity, now + 100_000, now).unwrap();
        assert_eq!(claims.binding, challenge.binding);
        assert_eq!(claims.expires_at_ms, now + MAX_ASSERTION_TTL_MS);
        assert_eq!(claims.lease_expires_at_ms, scope.lease_expires_at_ms);
        let claims =
            claims_from_verified_scope(&challenge, &scope, &identity, now + 5000, now).unwrap();
        assert_eq!(claims.expires_at_ms, now + 5000);
        let mut short = scope;
        short.lease_expires_at_ms = now + 3000;
        let claims =
            claims_from_verified_scope(&challenge, &short, &identity, now + 5000, now).unwrap();
        assert_eq!(claims.expires_at_ms, now + 3000);
    }

    #[test]
    fn wrong_owner_operator_job_lease_generation_workload_and_provider_identity_are_rejected() {
        for mutation in 0..10 {
            let (mut challenge, scope, identity, now) = fixture();
            match mutation {
                0 => challenge.binding.owner_pubkey_b58 = "wrong-owner".into(),
                1 => challenge.binding.operator_pubkey_b58 = "wrong-operator".into(),
                2 => challenge.binding.job_id = Uuid::new_v4(),
                3 => challenge.binding.lease_id = "different-lease".into(),
                4 => challenge.binding.lease_generation = "replacement-vm".into(),
                5 => challenge.binding.workload_sha256 = "00".repeat(32),
                6 => challenge.binding.phala_app_id = "00".repeat(20),
                7 => challenge.binding.phala_instance_id = "00".repeat(20),
                8 => {
                    let mut scope = scope;
                    scope.profile = HardwareProfile::Standard;
                    assert!(claims_from_verified_scope(
                        &challenge,
                        &scope,
                        &identity,
                        now + 10_000,
                        now
                    )
                    .is_err());
                    continue;
                }
                _ => {
                    assert!(
                        claims_from_verified_scope(&challenge, &scope, &identity, now, now)
                            .is_err()
                    );
                    continue;
                }
            }
            assert!(
                claims_from_verified_scope(&challenge, &scope, &identity, now + 10_000, now)
                    .is_err()
            );
        }
    }

    #[test]
    fn expired_lease_and_zero_time_fail_closed() {
        let (challenge, mut scope, identity, now) = fixture();
        assert!(claims_from_verified_scope(&challenge, &scope, &identity, now + 1000, 0).is_err());
        scope.lease_expires_at_ms = now;
        assert!(
            claims_from_verified_scope(&challenge, &scope, &identity, now + 1000, now).is_err()
        );
    }

    #[test]
    fn broker_allowlist_rejects_empty_ambiguous_and_unbounded_configuration() {
        assert!(parse_broker_ids("").is_err());
        assert!(parse_broker_ids("broker-a,").is_err());
        assert!(parse_broker_ids("broker a").is_err());
        assert!(parse_broker_ids(&"x".repeat(129)).is_err());
        assert_eq!(parse_broker_ids("broker-a, broker-b").unwrap().len(), 2);
    }

    #[test]
    fn durable_native_recipient_must_bind_every_requested_key_scope_field() {
        let (challenge, _, _, _) = fixture();
        let config = covenant_compute_protocol::phala::ApplicationKeyRecipient {
            protocol_version: 1,
            broker_id: challenge.broker_id.clone(),
            broker_response_pubkey_b58: challenge.binding.owner_pubkey_b58.clone(),
            tenant_id: challenge.binding.tenant_id.clone(),
            key_id: challenge.binding.key_id.clone(),
            key_version: challenge.binding.key_version,
            purpose: challenge.binding.purpose.clone(),
            recipient_image_digest: format!("sha256:{}", "aa".repeat(32)),
        };
        assert!(require_recipient_scope(&challenge, &config).is_ok());
        for field in 0..6 {
            let mut changed = config.clone();
            match field {
                0 => changed.protocol_version = 2,
                1 => changed.broker_id.push_str("-other"),
                2 => changed.tenant_id.push_str("-other"),
                3 => changed.key_id.push_str("-other"),
                4 => changed.key_version += 1,
                _ => changed.purpose.push_str("-other"),
            }
            assert!(require_recipient_scope(&challenge, &changed).is_err());
        }
    }

    #[tokio::test]
    async fn durable_authority_rejects_closed_tampered_and_refunded_leases() {
        let directory = tempfile::tempdir().unwrap();
        let state = Arc::new(
            CoordinatorState::with_journal(
                LocalIdentity::generate("coordinator@key-broker-test"),
                crate::CoordinatorConfig::default(),
                Arc::new(crate::reputation::NoReputation),
                Arc::new(crate::MockPayout::new()),
                Arc::new(covenant_audit::InMemoryAuditLog::new()),
                &directory.path().join("journal.jsonl"),
                None,
            )
            .await
            .unwrap(),
        );
        let buyer = LocalIdentity::generate("buyer@key-broker-test");
        let id = Uuid::new_v4();
        let now = crate::epoch_ms();
        let envelope = SignedJobEnvelope::sign(
            JobEnvelopePayload {
                job_id: id,
                buyer: buyer.agent_id(),
                kind: JobKind::LeaseSession,
                capability_requirement: CapabilityRequirement {
                    inference_tariff: None,
                    gpu_class: None,
                    min_vram_gb: None,
                    model_id: None,
                    kind: JobKind::LeaseSession,
                    max_duration_secs: 60,
                    min_reputation_bps: None,
                },
                input: vec![covenant_compute_protocol::lease_input(LeaseTerms {
                    max_duration_secs: 60,
                    rate_micro_usdc_per_sec: 1,
                    client_public_key: None,
                })
                .unwrap()],
                price_micro_usdc: 60,
                deadline_ms: 120_000,
                idempotency: covenant_a2a::A2AIdempotency::new(
                    covenant_a2a::A2ADuplicateSafety::Idempotent,
                    "key-broker-test",
                ),
                issued_at_ms: now,
                referral_code: None,
                stream: false,
            },
            &buyer,
        )
        .unwrap();
        let hold = state
            .escrow()
            .hold(id, &buyer.agent_id(), 60)
            .await
            .unwrap();
        let record: crate::JobRecord = serde_json::from_value(serde_json::json!({
            "operator_pubkey_b58":buyer.agent_id().pubkey_base58(),
            "envelope":envelope,"escrow_hold":hold,"phase":"accepted",
            "receipt":null,"output":null,"fee_micro_usdc":0,"accepted_at_ms":now
        }))
        .unwrap();
        state.jobs().insert(id, record.clone()).unwrap();
        let hardware = Arc::new(
            HardwareService::open_signed_leases(
                state.clone(),
                &directory.path().join("hardware.json"),
            )
            .unwrap(),
        );
        let service = KeyBrokerLeaseService {
            state: state.clone(),
            hardware,
            broker_ids: parse_broker_ids("broker-a").unwrap(),
            slots: Arc::new(tokio::sync::Semaphore::new(2)),
        };
        assert!(service.require_durable_live_hold(id).is_ok());
        let mut closed = record.clone();
        closed.close_requested_at_ms = Some(now);
        state.jobs().insert(id, closed).unwrap();
        assert!(service.require_durable_live_hold(id).is_err());
        let mut tampered = record.clone();
        tampered.envelope.payload.price_micro_usdc = 1;
        state.jobs().insert(id, tampered).unwrap();
        assert!(service.require_durable_live_hold(id).is_err());
        let mut completed = record.clone();
        completed.phase = crate::JobPhase::Completed;
        state.jobs().insert(id, completed).unwrap();
        assert!(service.require_durable_live_hold(id).is_err());
        state.jobs().insert(id, record).unwrap();
        state
            .escrow()
            .refund(
                id,
                covenant_compute_protocol::RefundReason::OperatorRejected,
            )
            .await
            .unwrap();
        assert!(service.require_durable_live_hold(id).is_err());
    }
}
