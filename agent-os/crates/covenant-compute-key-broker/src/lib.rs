//! Attestation-gated application-key release. This is not a dstack boot KMS.
//!
//! Production callers cannot inject an attestation verdict. A release verifies
//! Intel DCAP, the measured no-KMS boot profile, workload, pinned key-owner
//! authorization, and a fresh coordinator assertion bound to an unpredictable
//! broker challenge. The broker has no plaintext-key serialization API.
//!
//! This core does not itself establish Nitro isolation, policy governance,
//! durable audit, online revocation, ciphertext rollback protection, or disaster
//! recovery. Deployment qualification must establish those properties separately.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use covenant_compute_attestation::vendor_types::TdxEventEntry;
use covenant_compute_protocol::{SignedJobEnvelope, phala};
use ed25519_dalek::{Signature, VerifyingKey};
use rand::{RngCore, rngs::OsRng};
use rsa::pkcs8::{DecodePublicKey, EncodePublicKey};
use rsa::traits::PublicKeyParts;
use rsa::{Oaep, RsaPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroizing;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_CHALLENGE_TTL_MS: u64 = 60_000;
pub const MAX_ASSERTION_TTL_MS: u64 = 30_000;
pub const MAX_REQUEST_BYTES: usize = 256 * 1024;
pub const BUYER_AUTHORIZATION_DOMAIN: &[u8] =
    b"covenant.compute.key-broker.buyer-authorization.v1\0";
pub const LEASE_ASSERTION_DOMAIN: &[u8] = b"covenant.compute.key-broker.lease-assertion.v1\0";
pub const REPORT_DATA_DOMAIN: &[u8] = b"covenant.compute.key-broker.report-data.v1\0";
pub const RELEASE_RESPONSE_DOMAIN: &[u8] = b"covenant.compute.key-broker.release-response.v1\0";

/// All fields are exact, case-sensitive values. None are copied from a verdict.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KeyBinding {
    pub tenant_id: String,
    pub key_id: String,
    pub key_version: u64,
    pub purpose: String,
    pub owner_pubkey_b58: String,
    pub job_id: Uuid,
    pub lease_id: String,
    pub lease_generation: String,
    pub operator_pubkey_b58: String,
    pub workload_sha256: String,
    pub phala_app_id: String,
    pub phala_instance_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ChallengeRequest {
    pub binding: KeyBinding,
    pub recipient_spki_der_base64: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_job_envelope: Option<SignedJobEnvelope>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrokerChallenge {
    pub protocol_version: u32,
    pub broker_id: String,
    pub challenge_id: Uuid,
    pub nonce_hex: String,
    pub ttl_ms: u64,
    pub binding: KeyBinding,
    pub recipient_spki_der_base64: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BuyerAuthorization {
    pub owner_pubkey_b58: String,
    pub signature_b58: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeaseAssertionRequest {
    pub challenge: BrokerChallenge,
    pub buyer_authorization: BuyerAuthorization,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LeaseState {
    Active,
}

/// The trusted coordinator must construct these claims from its current
/// durable lease state, not sign a caller-provided assertion body blindly.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeaseAssertionClaims {
    pub challenge_sha256: String,
    pub binding: KeyBinding,
    pub state: LeaseState,
    pub observed_at_ms: u64,
    pub expires_at_ms: u64,
    pub lease_expires_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignedLeaseAssertion {
    pub coordinator_pubkey_b58: String,
    pub claims: LeaseAssertionClaims,
    pub signature_b58: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TdxKeyEvidence {
    pub quote_base64: String,
    pub collateral_json: String,
    pub events: Vec<TdxEventEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseRequest {
    pub challenge: BrokerChallenge,
    pub buyer_authorization: BuyerAuthorization,
    pub lease_assertion: SignedLeaseAssertion,
    pub evidence: TdxKeyEvidence,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseResponse {
    pub protocol_version: u32,
    pub challenge_id: Uuid,
    pub challenge_sha256: String,
    pub key_id: String,
    pub key_version: u64,
    pub ciphertext_base64: String,
    pub algorithm: String,
}

/// OAEP encryption alone does not authenticate the sender: anyone knows the
/// guest's public key. The guest must verify this signature against its measured
/// policy's pinned broker key before accepting or decrypting the ciphertext.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignedReleaseResponse {
    pub response: ReleaseResponse,
    pub broker_pubkey_b58: String,
    pub signature_b58: String,
}

/// Immutable configuration installed and measured with the broker. Key blobs
/// and owners are never accepted from the release request. Editing this policy
/// requires a new approved broker measurement/deployment, not a hot reload.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyPolicy {
    pub tenant_id: String,
    pub key_id: String,
    pub key_version: u64,
    pub purpose: String,
    pub owner_pubkey_b58: String,
    pub workload: WorkloadPolicy,
    pub kms_key_arn: String,
    pub wrapped_key_base64: String,
}

/// A fixed digest is useful only for isolated qualification: the native Compose
/// hash includes the job and expiry. Normal reuse approves immutable images and
/// the compiled template, then independently reconstructs each signed lease's
/// exact manifest. No caller-supplied template or family verdict is accepted.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkloadPolicy {
    FixedQualificationHash {
        workload_sha256: String,
    },
    NativeTemplate {
        guest_image_digest: String,
        recipient_image_digest: String,
        broker_response_pubkey_b58: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerPolicy {
    pub broker_id: String,
    pub coordinator_pubkey_b58: String,
    pub challenge_ttl_ms: u64,
    pub max_pending_challenges: usize,
    pub keys: Vec<KeyPolicy>,
}

/// Backend errors deliberately carry no remote response, plaintext, or key blob.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BrokerError {
    #[error("broker policy is invalid")]
    InvalidPolicy,
    #[error("request encoding is invalid or exceeds limits")]
    InvalidRequest,
    #[error("key binding is not authorized by broker policy")]
    BindingRejected,
    #[error("recipient must be a canonical RSA-3072 SPKI key with exponent 65537")]
    RecipientRejected,
    #[error("broker challenge capacity reached")]
    Capacity,
    #[error("challenge is unknown, consumed, expired, or mismatched")]
    ChallengeRejected,
    #[error("key owner authorization failed")]
    OwnerAuthorizationRejected,
    #[error("fresh coordinator lease authorization failed")]
    LeaseAuthorizationRejected,
    #[error("hardware attestation failed")]
    AttestationRejected,
    #[error("key unwrap failed; outcome must not be retried for this challenge")]
    UnwrapFailed,
    #[error("recipient encryption failed")]
    EncryptionFailed,
    #[error("broker response authentication failed")]
    ResponseAuthenticationRejected,
}

pub type Result<T> = std::result::Result<T, BrokerError>;
pub type DataKey = Zeroizing<[u8; 32]>;

/// Must unwrap only via the measured Nitro recipient flow. No production
/// plaintext file/env/test backend is provided by this crate.
#[async_trait]
pub trait KeyUnwrapper: Send + Sync {
    async fn unwrap_data_key(
        &self,
        key: &KeyPolicy,
        encryption_context: &BTreeMap<String, String>,
    ) -> Result<DataKey>;
}

struct PendingChallenge {
    challenge: BrokerChallenge,
    issued: Instant,
}

pub struct Broker {
    policy: BrokerPolicy,
    pending: HashMap<Uuid, PendingChallenge>,
}

/// One-use authorization, constructible only by the broker after all checks.
/// It is neither cloneable nor serializable. Signing-key initialization may run
/// after authorization; its latency never extends this permit's deadline.
pub struct AuthorizedRelease {
    pending: PendingChallenge,
    claims: LeaseAssertionClaims,
    key: KeyPolicy,
    context: BTreeMap<String, String>,
    recipient: RsaPublicKey,
}

impl AuthorizedRelease {
    pub async fn release(
        self,
        unwrapper: &(impl KeyUnwrapper + ?Sized),
    ) -> Result<ReleaseResponse> {
        trusted_time(&self.pending, &self.claims)?;
        let plaintext = unwrapper
            .unwrap_data_key(&self.key, &self.context)
            .await
            .map_err(|_| BrokerError::UnwrapFailed)?;
        // A slow backend or signing-key initialization cannot extend the lease.
        trusted_time(&self.pending, &self.claims)?;
        let challenge = &self.pending.challenge;
        let challenge_sha256 = challenge_digest(challenge)?;
        let ciphertext = self
            .recipient
            .encrypt(
                &mut OsRng,
                Oaep::new_with_label::<Sha256, _>(response_label(&challenge_sha256)),
                plaintext.as_ref(),
            )
            .map_err(|_| BrokerError::EncryptionFailed)?;
        trusted_time(&self.pending, &self.claims)?;
        Ok(ReleaseResponse {
            protocol_version: PROTOCOL_VERSION,
            challenge_id: challenge.challenge_id,
            challenge_sha256,
            key_id: self.key.key_id.clone(),
            key_version: self.key.key_version,
            ciphertext_base64: STANDARD.encode(ciphertext),
            algorithm: "RSA_OAEP_SHA256".into(),
        })
    }
}

impl Broker {
    pub fn new(policy: BrokerPolicy) -> Result<Self> {
        validate_policy(&policy)?;
        Ok(Self {
            policy,
            pending: HashMap::new(),
        })
    }

    pub fn issue_challenge(&mut self, request: ChallengeRequest) -> Result<BrokerChallenge> {
        if serde_json::to_vec(&request)
            .map_err(|_| BrokerError::InvalidRequest)?
            .len()
            > MAX_REQUEST_BYTES
        {
            return Err(BrokerError::InvalidRequest);
        }
        validate_binding(&request.binding)?;
        let key = self.key_policy(&request.binding)?;
        authorize_workload(&self.policy.broker_id, key, &request)?;
        decode_recipient(&request.recipient_spki_der_base64)?;
        self.pending
            .retain(|_, p| p.issued.elapsed() < Duration::from_millis(p.challenge.ttl_ms));
        if self.pending.len() >= self.policy.max_pending_challenges {
            return Err(BrokerError::Capacity);
        }
        let mut nonce = [0u8; 32];
        OsRng.fill_bytes(&mut nonce);
        let challenge = BrokerChallenge {
            protocol_version: PROTOCOL_VERSION,
            broker_id: self.policy.broker_id.clone(),
            challenge_id: Uuid::new_v4(),
            nonce_hex: hex::encode(nonce),
            ttl_ms: self.policy.challenge_ttl_ms,
            binding: request.binding,
            recipient_spki_der_base64: request.recipient_spki_der_base64,
        };
        self.pending.insert(
            challenge.challenge_id,
            PendingChallenge {
                challenge: challenge.clone(),
                issued: Instant::now(),
            },
        );
        Ok(challenge)
    }

    /// A challenge is consumed before verification and before any KMS call.
    /// Failures and unknown unwrap outcomes never restore it. Restart loses all
    /// challenges, and there is no caller-supplied wall clock used for DCAP.
    pub async fn release(
        &mut self,
        request: ReleaseRequest,
        unwrapper: &(impl KeyUnwrapper + ?Sized),
    ) -> Result<ReleaseResponse> {
        self.authorize_release(request)?.release(unwrapper).await
    }

    /// Consume and verify before any backend operation, including optional
    /// response-signing-key initialization. The returned permit rechecks time
    /// immediately before application-key unwrap and before returning output.
    pub fn authorize_release(&mut self, request: ReleaseRequest) -> Result<AuthorizedRelease> {
        self.authorize_inner(request, verify_hardware)
    }

    #[cfg(test)]
    async fn release_inner(
        &mut self,
        request: ReleaseRequest,
        unwrapper: &(impl KeyUnwrapper + ?Sized),
        verify: fn(&TdxKeyEvidence, &BrokerChallenge, u64) -> Result<()>,
    ) -> Result<ReleaseResponse> {
        self.authorize_inner(request, verify)?
            .release(unwrapper)
            .await
    }

    fn authorize_inner(
        &mut self,
        request: ReleaseRequest,
        verify: fn(&TdxKeyEvidence, &BrokerChallenge, u64) -> Result<()>,
    ) -> Result<AuthorizedRelease> {
        let pending = self
            .pending
            .remove(&request.challenge.challenge_id)
            .ok_or(BrokerError::ChallengeRejected)?;
        if pending.challenge != request.challenge
            || pending.issued.elapsed() >= Duration::from_millis(pending.challenge.ttl_ms)
        {
            return Err(BrokerError::ChallengeRejected);
        }
        if serde_json::to_vec(&request)
            .map_err(|_| BrokerError::InvalidRequest)?
            .len()
            > MAX_REQUEST_BYTES
        {
            return Err(BrokerError::InvalidRequest);
        }
        let challenge = &pending.challenge;
        let key = self.key_policy(&challenge.binding)?;
        verify_owner_authorization(challenge, &request.buyer_authorization)?;
        let signed = &request.lease_assertion;
        verify_lease_authorization(challenge, signed, &self.policy.coordinator_pubkey_b58)?;
        let verification_time = trusted_time(&pending, &signed.claims)?;
        verify(&request.evidence, challenge, verification_time)?;
        let recipient = decode_recipient(&challenge.recipient_spki_der_base64)?;
        let context = encryption_context(&self.policy.broker_id, key);
        // Re-check after expensive DCAP processing, before a billable unwrap.
        trusted_time(&pending, &signed.claims)?;
        Ok(AuthorizedRelease {
            pending,
            claims: signed.claims.clone(),
            key: key.clone(),
            context,
            recipient,
        })
    }

    fn key_policy(&self, binding: &KeyBinding) -> Result<&KeyPolicy> {
        self.policy
            .keys
            .iter()
            .find(|key| {
                key.tenant_id == binding.tenant_id
                    && key.key_id == binding.key_id
                    && key.key_version == binding.key_version
                    && key.purpose == binding.purpose
                    && key.owner_pubkey_b58 == binding.owner_pubkey_b58
            })
            .ok_or(BrokerError::BindingRejected)
    }
}

fn authorize_workload(broker_id: &str, key: &KeyPolicy, request: &ChallengeRequest) -> Result<()> {
    match &key.workload {
        WorkloadPolicy::FixedQualificationHash { workload_sha256 } => {
            if request.signed_job_envelope.is_some()
                || workload_sha256 != &request.binding.workload_sha256
            {
                return Err(BrokerError::BindingRejected);
            }
        }
        WorkloadPolicy::NativeTemplate {
            guest_image_digest,
            recipient_image_digest,
            broker_response_pubkey_b58,
        } => {
            let envelope = request
                .signed_job_envelope
                .as_ref()
                .ok_or(BrokerError::BindingRejected)?;
            envelope
                .verify()
                .map_err(|_| BrokerError::BindingRejected)?;
            envelope
                .payload
                .validate_input()
                .map_err(|_| BrokerError::BindingRejected)?;
            let binding = &request.binding;
            let job = &envelope.payload;
            if job.job_id != binding.job_id
                || job.job_id.to_string() != binding.lease_id
                || job.buyer.pubkey_base58() != key.owner_pubkey_b58
            {
                return Err(BrokerError::BindingRejected);
            }
            let spec = phala::parse_phala_ephemeral_lease_spec(&job.input)
                .map_err(|_| BrokerError::BindingRejected)?
                .ok_or(BrokerError::BindingRejected)?;
            let expected_recipient = phala::ApplicationKeyRecipient {
                protocol_version: PROTOCOL_VERSION,
                broker_id: broker_id.into(),
                broker_response_pubkey_b58: broker_response_pubkey_b58.clone(),
                tenant_id: key.tenant_id.clone(),
                key_id: key.key_id.clone(),
                key_version: key.key_version,
                purpose: key.purpose.clone(),
                recipient_image_digest: recipient_image_digest.clone(),
            };
            if spec.operator_pubkey_b58 != binding.operator_pubkey_b58
                || spec.lease_generation != binding.lease_generation
                || spec.workload_sha256 != binding.workload_sha256
                || spec.application_key_recipient.as_ref() != Some(&expected_recipient)
            {
                return Err(BrokerError::BindingRejected);
            }
            let exact = phala::render_phala_ephemeral_manifest(
                job,
                &binding.operator_pubkey_b58,
                &spec,
                &phala::reviewed_phala_ephemeral_wrapper(),
                guest_image_digest,
            )
            .map_err(|_| BrokerError::BindingRejected)?;
            if hex::encode(Sha256::digest(exact.as_bytes())) != binding.workload_sha256 {
                return Err(BrokerError::BindingRejected);
            }
        }
    }
    Ok(())
}

pub fn challenge_digest(challenge: &BrokerChallenge) -> Result<String> {
    Ok(hex::encode(Sha256::digest(canonical(challenge)?)))
}

pub fn report_data(challenge: &BrokerChallenge) -> Result<[u8; 64]> {
    let mut hash = Sha512::new();
    hash.update(REPORT_DATA_DOMAIN);
    hash.update(canonical(challenge)?);
    Ok(hash.finalize().into())
}

pub fn buyer_authorization_bytes(challenge: &BrokerChallenge) -> Result<Vec<u8>> {
    domain_bytes(BUYER_AUTHORIZATION_DOMAIN, challenge)
}

pub fn lease_assertion_bytes(claims: &LeaseAssertionClaims) -> Result<Vec<u8>> {
    domain_bytes(LEASE_ASSERTION_DOMAIN, claims)
}

pub fn release_response_bytes(response: &ReleaseResponse) -> Result<Vec<u8>> {
    domain_bytes(RELEASE_RESPONSE_DOMAIN, response)
}

pub fn verify_signed_response(
    pinned_broker_pubkey_b58: &str,
    challenge: &BrokerChallenge,
    signed: &SignedReleaseResponse,
) -> Result<()> {
    let response = &signed.response;
    if signed.broker_pubkey_b58 != pinned_broker_pubkey_b58
        || response.protocol_version != PROTOCOL_VERSION
        || response.challenge_id != challenge.challenge_id
        || response.challenge_sha256 != challenge_digest(challenge)?
        || response.key_id != challenge.binding.key_id
        || response.key_version != challenge.binding.key_version
        || response.algorithm != "RSA_OAEP_SHA256"
        || response.ciphertext_base64.len() != 512
    {
        return Err(BrokerError::ResponseAuthenticationRejected);
    }
    verify_signature(
        pinned_broker_pubkey_b58,
        &signed.signature_b58,
        &release_response_bytes(response)?,
    )
    .map_err(|_| BrokerError::ResponseAuthenticationRejected)
}

pub fn response_label(challenge_sha256: &str) -> String {
    format!("covenant.compute.key-broker.key-response.v1:{challenge_sha256}")
}

pub fn encryption_context(broker_id: &str, key: &KeyPolicy) -> BTreeMap<String, String> {
    [
        ("covenant:protocol", "application-key-v1".into()),
        ("covenant:broker_id", broker_id.into()),
        ("covenant:tenant_id", key.tenant_id.clone()),
        ("covenant:key_id", key.key_id.clone()),
        ("covenant:key_version", key.key_version.to_string()),
        ("covenant:purpose", key.purpose.clone()),
        ("covenant:owner_pubkey_b58", key.owner_pubkey_b58.clone()),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value))
    .collect()
}

fn canonical(value: &impl Serialize) -> Result<Vec<u8>> {
    serde_jcs::to_vec(value).map_err(|_| BrokerError::InvalidRequest)
}

fn domain_bytes(domain: &[u8], value: &impl Serialize) -> Result<Vec<u8>> {
    let mut bytes = domain.to_vec();
    bytes.extend(canonical(value)?);
    Ok(bytes)
}

pub fn verify_owner_authorization(
    challenge: &BrokerChallenge,
    authorization: &BuyerAuthorization,
) -> Result<()> {
    validate_binding(&challenge.binding)?;
    if challenge.protocol_version != PROTOCOL_VERSION
        || !valid_identifier(&challenge.broker_id)
        || challenge.challenge_id.is_nil()
        || !valid_hex(&challenge.nonce_hex, 32)
        || challenge.ttl_ms == 0
        || challenge.ttl_ms > MAX_CHALLENGE_TTL_MS
    {
        return Err(BrokerError::InvalidRequest);
    }
    decode_recipient(&challenge.recipient_spki_der_base64)?;
    if authorization.owner_pubkey_b58 != challenge.binding.owner_pubkey_b58 {
        return Err(BrokerError::OwnerAuthorizationRejected);
    }
    verify_signature(
        &authorization.owner_pubkey_b58,
        &authorization.signature_b58,
        &buyer_authorization_bytes(challenge)?,
    )
    .map_err(|_| BrokerError::OwnerAuthorizationRejected)
}

/// Verify the authority signature and exact challenge/lease binding. The broker
/// separately applies its monotonic lifetime check; other callers must enforce
/// freshness against their own trusted current time before relying on this.
pub fn verify_lease_authorization(
    challenge: &BrokerChallenge,
    assertion: &SignedLeaseAssertion,
    expected_coordinator: &str,
) -> Result<()> {
    let claims = &assertion.claims;
    if assertion.coordinator_pubkey_b58 != expected_coordinator
        || claims.binding != challenge.binding
        || claims.challenge_sha256 != challenge_digest(challenge)?
        || claims.observed_at_ms == 0
        || claims.observed_at_ms >= claims.expires_at_ms
        || claims.expires_at_ms - claims.observed_at_ms > MAX_ASSERTION_TTL_MS
        || claims.expires_at_ms > claims.lease_expires_at_ms
        || claims.lease_expires_at_ms > 9_007_199_254_740_991
    {
        return Err(BrokerError::LeaseAuthorizationRejected);
    }
    verify_signature(
        expected_coordinator,
        &assertion.signature_b58,
        &lease_assertion_bytes(claims)?,
    )
    .map_err(|_| BrokerError::LeaseAuthorizationRejected)
}

fn trusted_time(pending: &PendingChallenge, claims: &LeaseAssertionClaims) -> Result<u64> {
    let elapsed = pending.issued.elapsed();
    if elapsed >= Duration::from_millis(pending.challenge.ttl_ms) {
        return Err(BrokerError::ChallengeRejected);
    }
    // The nonce-bound assertion was signed after challenge issuance. Adding all
    // elapsed time is conservative: delivery delay cannot make old collateral
    // or an expired lease look fresh. The coordinator clock is a trusted input.
    let elapsed_ms =
        u64::try_from(elapsed.as_millis()).map_err(|_| BrokerError::LeaseAuthorizationRejected)?;
    let now = claims
        .observed_at_ms
        .checked_add(elapsed_ms)
        .ok_or(BrokerError::LeaseAuthorizationRejected)?;
    if now >= claims.expires_at_ms || now >= claims.lease_expires_at_ms {
        return Err(BrokerError::LeaseAuthorizationRejected);
    }
    Ok(now)
}

fn verify_hardware(
    evidence: &TdxKeyEvidence,
    challenge: &BrokerChallenge,
    now_ms: u64,
) -> Result<()> {
    if evidence.quote_base64.len() > 32 * 1024
        || evidence.collateral_json.len() > 128 * 1024
        || evidence.events.len() > 256
    {
        return Err(BrokerError::AttestationRejected);
    }
    let quote = STANDARD
        .decode(&evidence.quote_base64)
        .map_err(|_| BrokerError::AttestationRejected)?;
    let workload: [u8; 32] = hex::decode(&challenge.binding.workload_sha256)
        .map_err(|_| BrokerError::AttestationRejected)?
        .try_into()
        .map_err(|_| BrokerError::AttestationRejected)?;
    let identity =
        covenant_compute_attestation::key_broker::verify_phala_ephemeral_key_attestation(
            &quote,
            &evidence.collateral_json,
            &evidence.events,
            &report_data(challenge)?,
            &workload,
            now_ms,
        )
        .map_err(|_| BrokerError::AttestationRejected)?;
    if identity.app_id != challenge.binding.phala_app_id
        || identity.instance_id != challenge.binding.phala_instance_id
    {
        return Err(BrokerError::AttestationRejected);
    }
    Ok(())
}

fn verify_signature(key: &str, signature: &str, bytes: &[u8]) -> Result<()> {
    if key.len() > 44 || signature.len() > 88 {
        return Err(BrokerError::InvalidRequest);
    }
    let key_bytes: [u8; 32] = bs58::decode(key)
        .into_vec()
        .map_err(|_| BrokerError::InvalidRequest)?
        .try_into()
        .map_err(|_| BrokerError::InvalidRequest)?;
    let signature_bytes: [u8; 64] = bs58::decode(signature)
        .into_vec()
        .map_err(|_| BrokerError::InvalidRequest)?
        .try_into()
        .map_err(|_| BrokerError::InvalidRequest)?;
    VerifyingKey::from_bytes(&key_bytes)
        .map_err(|_| BrokerError::InvalidRequest)?
        .verify_strict(bytes, &Signature::from_bytes(&signature_bytes))
        .map_err(|_| BrokerError::InvalidRequest)
}

fn decode_recipient(encoded: &str) -> Result<RsaPublicKey> {
    if encoded.len() > 1024 {
        return Err(BrokerError::RecipientRejected);
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| BrokerError::RecipientRejected)?;
    let key =
        RsaPublicKey::from_public_key_der(&bytes).map_err(|_| BrokerError::RecipientRejected)?;
    if key.n().bits() != 3072
        || key.e() != &rsa::BigUint::from(65537u32)
        || key
            .to_public_key_der()
            .map_err(|_| BrokerError::RecipientRejected)?
            .as_ref()
            != bytes
        || STANDARD.encode(&bytes) != encoded
    {
        return Err(BrokerError::RecipientRejected);
    }
    Ok(key)
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.:/".contains(&b))
}

fn valid_hex(value: &str, bytes: usize) -> bool {
    value.len() == bytes * 2
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_image_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|digest| valid_hex(digest, 32))
}

fn valid_pubkey(value: &str) -> bool {
    if value.len() > 44 {
        return false;
    }
    let Ok(bytes) = bs58::decode(value).into_vec() else {
        return false;
    };
    let Ok(bytes) = <[u8; 32]>::try_from(bytes) else {
        return false;
    };
    VerifyingKey::from_bytes(&bytes).is_ok_and(|key| !key.is_weak())
        && bs58::encode(bytes).into_string() == value
}

fn valid_kms_key_arn(value: &str) -> bool {
    let parts = value.split(':').collect::<Vec<_>>();
    if parts.len() != 6
        || parts[..3] != ["arn", "aws", "kms"]
        || parts[3].is_empty()
        || parts[3].len() > 32
        || !parts[3]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || parts[4].len() != 12
        || !parts[4].bytes().all(|b| b.is_ascii_digit())
    {
        return false;
    }
    let Some(key_id) = parts[5].strip_prefix("key/") else {
        return false;
    };
    if let Some(multi_region_id) = key_id.strip_prefix("mrk-") {
        valid_hex(multi_region_id, 16)
    } else {
        Uuid::parse_str(key_id).is_ok_and(|id| !id.is_nil() && id.to_string() == key_id)
    }
}

fn validate_binding(binding: &KeyBinding) -> Result<()> {
    if ![
        &binding.tenant_id,
        &binding.key_id,
        &binding.purpose,
        &binding.lease_id,
        &binding.lease_generation,
    ]
    .into_iter()
    .all(|value| valid_identifier(value))
        || binding.key_version == 0
        // Restrict interoperable JCS integers to JavaScript's exact range.
        || binding.key_version > 9_007_199_254_740_991
        || binding.job_id.is_nil()
        || !valid_pubkey(&binding.owner_pubkey_b58)
        || !valid_pubkey(&binding.operator_pubkey_b58)
        || !valid_hex(&binding.workload_sha256, 32)
        || !valid_hex(&binding.phala_app_id, 20)
        || !valid_hex(&binding.phala_instance_id, 20)
    {
        return Err(BrokerError::BindingRejected);
    }
    Ok(())
}

fn validate_policy(policy: &BrokerPolicy) -> Result<()> {
    if !valid_identifier(&policy.broker_id)
        || !valid_pubkey(&policy.coordinator_pubkey_b58)
        || policy.challenge_ttl_ms == 0
        || policy.challenge_ttl_ms > MAX_CHALLENGE_TTL_MS
        || policy.max_pending_challenges == 0
        || policy.max_pending_challenges > 4096
        || policy.keys.is_empty()
        || policy.keys.len() > 4096
    {
        return Err(BrokerError::InvalidPolicy);
    }
    let mut unique = std::collections::HashSet::new();
    for key in &policy.keys {
        if ![&key.tenant_id, &key.key_id, &key.purpose]
            .into_iter()
            .all(|value| valid_identifier(value))
            || key.key_version == 0
            || key.key_version > 9_007_199_254_740_991
            // The response-signing seed is never an application-key API entry,
            // even if an operator accidentally copies its envelope into policy.
            || key.key_id == "broker-response-signing"
            || key.purpose == "broker-response-signing"
            || !valid_pubkey(&key.owner_pubkey_b58)
            || match &key.workload {
                WorkloadPolicy::FixedQualificationHash { workload_sha256 } => {
                    !valid_hex(workload_sha256, 32)
                }
                WorkloadPolicy::NativeTemplate {
                    guest_image_digest,
                    recipient_image_digest,
                    broker_response_pubkey_b58,
                } => {
                    !valid_image_digest(guest_image_digest)
                        || !valid_image_digest(recipient_image_digest)
                        || !valid_pubkey(broker_response_pubkey_b58)
                }
            }
            || !valid_kms_key_arn(&key.kms_key_arn)
            || key.wrapped_key_base64.len() > 8192
            || STANDARD
                .decode(&key.wrapped_key_base64)
                .map_or(true, |blob| {
                    blob.is_empty() || STANDARD.encode(blob) != key.wrapped_key_base64
                })
            || !unique.insert((&key.tenant_id, &key.key_id, key.key_version, &key.purpose))
        {
            return Err(BrokerError::InvalidPolicy);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
