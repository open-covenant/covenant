//! Covenant session bindings around the independently verified vendor bytes.
//! Imported reference vectors are not a deployment approval. Until fresh
//! Covenant hardware acceptance is reviewed, no network capability is promoted.
use crate::{Policy, SnpExpectation, TdxEvent, VerificationError, vendor_types::*};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{DateTime, Duration, Utc};
use covenant_compute_protocol::ssh::{SshHostKey, validate_ssh_host};
use covenant_identity::LocalIdentity;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};
use uuid::Uuid;

pub const SERVED_TRUST_CEILING: TrustClass = TrustClass::Open;
pub const MAX_EVIDENCE_BYTES: usize = 512 * 1024;
pub const CHALLENGE_TTL_MS: u64 = 120_000;
pub const VERDICT_TTL_MS: u64 = 300_000;
const AUTH_DOMAIN: &[u8] = b"covenant.compute.hardware.http.v1\0";

#[derive(Debug, thiserror::Error)]
pub enum BindingError {
    #[error("invalid hardware binding: {0}")]
    Invalid(String),
    #[error("hardware verification: {0}")]
    Verification(#[from] VerificationError),
    #[error("hardware encoding: {0}")]
    Encoding(#[from] serde_json::Error),
}
type Result<T> = std::result::Result<T, BindingError>;
/// Selected by trusted lease admission, never inferred from provider evidence.
/// The default preserves the existing managed-KMS and SNP verification paths.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HardwareProfile {
    #[default]
    Standard,
    PhalaEphemeralDstack061,
}
impl HardwareProfile {
    fn is_standard(&self) -> bool {
        *self == Self::Standard
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HardwareScope {
    /// Signed into the report binding. Omission preserves legacy wire bytes.
    #[serde(default, skip_serializing_if = "HardwareProfile::is_standard")]
    pub profile: HardwareProfile,
    pub job_id: Uuid,
    pub operator_pubkey_b58: String,
    pub buyer_pubkey_b58: String,
    pub lease_generation: String,
    pub host: String,
    pub port: u16,
    /// Authenticated outer transport, distinct from the guest-owned SSH key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<covenant_compute_protocol::ssh::SshTransport>,
    pub username: String,
    pub guest_channel_key: SshHostKey,
    pub workload_sha256: String,
    pub lease_expires_at_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HardwareChallenge {
    pub schema_version: u8,
    pub challenge_id: Uuid,
    pub nonce_hex: String,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub scope: HardwareScope,
}
impl HardwareChallenge {
    pub fn new(scope: HardwareScope, now: u64) -> Result<Self> {
        // Two independent OS-random UUIDs provide 244 bits of unpredictability.
        let mut nonce = [0u8; 32];
        nonce[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        nonce[16..].copy_from_slice(Uuid::new_v4().as_bytes());
        let value = Self {
            schema_version: 1,
            challenge_id: Uuid::new_v4(),
            nonce_hex: hex::encode(nonce),
            issued_at_ms: now,
            expires_at_ms: now
                .saturating_add(CHALLENGE_TTL_MS)
                .min(scope.lease_expires_at_ms),
            scope,
        };
        value.validate(now)?;
        Ok(value)
    }
    pub fn validate(&self, now: u64) -> Result<()> {
        let invalid = |s: &str| BindingError::Invalid(s.into());
        if self.schema_version != 1
            || self.challenge_id.is_nil()
            || self.scope.job_id.is_nil()
            || now < self.issued_at_ms
            || now >= self.expires_at_ms
            || self.expires_at_ms > self.issued_at_ms.saturating_add(CHALLENGE_TTL_MS)
            || self.expires_at_ms > self.scope.lease_expires_at_ms
        {
            return Err(invalid("stale or invalid challenge"));
        }
        if hex::decode(&self.nonce_hex)
            .ok()
            .is_none_or(|b| b.len() != 32)
            || hex::decode(&self.scope.workload_sha256)
                .ok()
                .is_none_or(|b| b.len() != 32)
        {
            return Err(invalid("nonce/workload must be SHA-256 sized"));
        }
        for key in [
            &self.scope.operator_pubkey_b58,
            &self.scope.buyer_pubkey_b58,
        ] {
            if bs58::decode(key)
                .into_vec()
                .ok()
                .is_none_or(|b| b.len() != 32)
            {
                return Err(invalid("invalid party public key"));
            }
        }
        validate_ssh_host(&self.scope.host).map_err(|_| invalid("invalid SSH host"))?;
        if let Some(transport) = self.scope.transport {
            transport
                .validate_host(&self.scope.host)
                .map_err(|_| invalid("invalid SSH transport hostname"))?;
        }
        self.scope
            .guest_channel_key
            .validate()
            .map_err(|_| invalid("invalid guest key"))?;
        if self.scope.port == 0
            || self.scope.username.is_empty()
            || self.scope.username.len() > 64
            || !self
                .scope
                .username
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            || self.scope.lease_generation.is_empty()
            || self.scope.lease_generation.len() > 128
            || self.scope.lease_generation.chars().any(char::is_control)
        {
            return Err(invalid("invalid endpoint generation"));
        }
        if self.scope.profile == HardwareProfile::PhalaEphemeralDstack061
            && (self.scope.transport
                != Some(covenant_compute_protocol::ssh::SshTransport::OutboundRelay)
                || self.scope.port != 443
                || self.scope.username != "covenant"
                || self.scope.lease_generation
                    != format!("phala-ephemeral-{}-1", self.scope.job_id))
        {
            return Err(invalid(
                "ephemeral profile requires its exact outbound relay lease scope",
            ));
        }
        Ok(())
    }
    /// The entire binding enters the hardware report, including the UUID and
    /// host key. The vendor's legacy u64 lease field is never used as identity.
    pub fn guest_report_data(&self) -> Result<[u8; 64]> {
        let mut h = Sha512::new();
        h.update(b"covenant.compute.hardware.guest.v1\0");
        h.update(serde_jcs::to_vec(self)?);
        Ok(h.finalize().into())
    }
    /// For a combined guest+GPU claim, collect the GPU report locally first,
    /// then commit its exact bytes and certificate chain inside the CPU report.
    /// The accepted guest image must itself enforce local-device collection;
    /// this hash alone is not proof of physical placement or channel custody.
    pub fn guest_report_data_for_gpu(&self, gpu: Option<&GpuEvidence>) -> Result<[u8; 64]> {
        let Some(gpu) = gpu else {
            return self.guest_report_data();
        };
        let mut h = Sha512::new();
        h.update(b"covenant.compute.hardware.guest-gpu.v1\0");
        h.update(self.guest_report_data()?);
        h.update(Sha256::digest(serde_jcs::to_vec(gpu)?));
        Ok(h.finalize().into())
    }
    pub fn gpu_nonce(&self) -> Result<[u8; 32]> {
        let mut h = Sha256::new();
        h.update(b"covenant.compute.hardware.gpu.v1\0");
        h.update(serde_jcs::to_vec(self)?);
        Ok(h.finalize().into())
    }
    pub fn guest_key_line(&self) -> String {
        format!(
            "{} {}",
            self.scope.guest_channel_key.algorithm, self.scope.guest_channel_key.key_base64
        )
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GuestEvidence {
    SevSnp {
        report_base64: String,
        certificate_chain_base64: Vec<String>,
    },
    Tdx {
        quote_base64: String,
        collateral_json: String,
        events: Vec<TdxEventEntry>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GpuEvidence {
    pub report_base64: String,
    pub certificate_chain_base64: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HardwareEvidence {
    pub challenge: HardwareChallenge,
    pub guest: GuestEvidence,
    pub gpu_cc: Option<GpuEvidence>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PhalaEphemeralIdentity {
    pub app_id: String,
    pub instance_id: String,
}

// Called only after vendor signature validation and complete RTMR replay.
fn ephemeral_identity(events: &[TdxEventEntry]) -> Result<PhalaEphemeralIdentity> {
    let identity = |name: &str| -> Result<String> {
        let mut found = events.iter().filter(|event| {
            event.imr == 3 && event.event_type == 0x0800_0001 && event.event == name
        });
        let event = found
            .next()
            .ok_or_else(|| BindingError::Invalid("missing verified boot identity".into()))?;
        if found.next().is_some() {
            return Err(BindingError::Invalid(
                "duplicate verified boot identity".into(),
            ));
        }
        let bytes = hex::decode(&event.event_payload)
            .map_err(|_| BindingError::Invalid("invalid verified boot identity".into()))?;
        if bytes.len() != 20 {
            return Err(BindingError::Invalid(
                "invalid verified boot identity length".into(),
            ));
        }
        Ok(hex::encode(bytes))
    };
    Ok(PhalaEphemeralIdentity {
        app_id: identity("app-id")?,
        instance_id: identity("instance-id")?,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HardwareVerdict {
    pub challenge: HardwareChallenge,
    pub evidence_sha256: String,
    pub reference_sha256: String,
    pub guest_identity: String,
    /// Derived from the cryptographically replayed boot log, never RPC Info.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phala_identity: Option<PhalaEphemeralIdentity>,
    pub gpu_identity: Option<String>,
    pub channel_key_fingerprint: String,
    pub evidence_class: TrustClass,
    pub served_class: TrustClass,
    pub verifier_version: String,
    pub verified_at_ms: u64,
    pub expires_at_ms: u64,
}
impl HardwareVerdict {
    pub fn require_floor(&self, scope: &HardwareScope, floor: TrustClass, now: u64) -> Result<()> {
        if &self.challenge.scope != scope
            || now < self.verified_at_ms
            || now >= self.expires_at_ms
            || now >= scope.lease_expires_at_ms
            || self.reference_sha256 != reference_digest_for_profile(scope.profile)
            || self.served_class > self.evidence_class
            || self.served_class > SERVED_TRUST_CEILING
            || self.served_class < floor
        {
            return Err(BindingError::Invalid(
                "lease evidence is stale, mismatched or below the required released trust class"
                    .into(),
            ));
        }
        Ok(())
    }
}
pub fn reference_digest() -> String {
    let mut h = Sha256::new();
    h.update(b"covenant.compute.hardware.references.v1\0");
    for bytes in [
        include_bytes!("../roots/amd-ark-genoa.der").as_slice(),
        include_bytes!("../roots/nvidia-device-identity-root.der").as_slice(),
        include_bytes!("../vendor/dcap-qvl/src/TrustedRootCA.der").as_slice(),
        include_bytes!("../reference/h100-measurements.json").as_slice(),
        include_bytes!("../reference/snp-platform.json").as_slice(),
        include_bytes!("../reference/snp-launch-measurements.json").as_slice(),
        include_bytes!("../reference/tdx-launch-measurements.json").as_slice(),
    ] {
        h.update((bytes.len() as u64).to_be_bytes());
        h.update(bytes);
    }
    hex::encode(h.finalize())
}
pub fn reference_digest_for_profile(profile: HardwareProfile) -> String {
    if profile.is_standard() {
        return reference_digest();
    }
    let mut h = Sha256::new();
    h.update(b"covenant.compute.hardware.profile.phala-ephemeral-dstack061.v1\0");
    h.update(reference_digest());
    h.update(b"dstack_0_6_1;key-provider=none;id=empty;os=launch-tuple;os-image-hash-event=absent;storage-encrypted=1;storage-fs=ext4;tdx-only;gpu=none");
    hex::encode(h.finalize())
}
pub fn verify_evidence(evidence: &HardwareEvidence, now: u64) -> Result<HardwareVerdict> {
    verify_with_policy(evidence, now, &Policy::default())
}
/// Explicit profile admission. The caller must derive the expected profile and
/// workload from the authenticated lease, not copy them from incoming evidence.
pub fn verify_evidence_for_profile(
    evidence: &HardwareEvidence,
    now: u64,
    expected_profile: HardwareProfile,
) -> Result<HardwareVerdict> {
    verify_with_policy_at(evidence, now, now, &Policy::default(), expected_profile)
}
/// Buyer-side re-verification: the coordinator's asserted class is not trusted.
pub fn verify_saved_verdict(
    evidence: &HardwareEvidence,
    verdict: &HardwareVerdict,
    expected: &HardwareScope,
    now: u64,
) -> Result<HardwareVerdict> {
    verify_saved_with_policy(evidence, verdict, expected, now, &Policy::default())
}
pub fn verify_saved_verdict_for_profile(
    evidence: &HardwareEvidence,
    verdict: &HardwareVerdict,
    expected: &HardwareScope,
    now: u64,
    expected_profile: HardwareProfile,
) -> Result<HardwareVerdict> {
    verify_saved_with_profile(
        evidence,
        verdict,
        expected,
        now,
        &Policy::default(),
        expected_profile,
    )
}
fn verify_saved_with_policy(
    evidence: &HardwareEvidence,
    verdict: &HardwareVerdict,
    expected: &HardwareScope,
    now: u64,
    policy: &Policy,
) -> Result<HardwareVerdict> {
    verify_saved_with_profile(
        evidence,
        verdict,
        expected,
        now,
        policy,
        HardwareProfile::Standard,
    )
}
fn verify_saved_with_profile(
    evidence: &HardwareEvidence,
    verdict: &HardwareVerdict,
    expected: &HardwareScope,
    now: u64,
    policy: &Policy,
    expected_profile: HardwareProfile,
) -> Result<HardwareVerdict> {
    if &evidence.challenge.scope != expected
        || expected.profile != expected_profile
        || now < verdict.verified_at_ms
        || now >= verdict.expires_at_ms
        || now >= expected.lease_expires_at_ms
    {
        return Err(BindingError::Invalid(
            "saved hardware evidence does not bind the current lease".into(),
        ));
    }
    let computed = verify_with_policy_at(
        evidence,
        verdict.verified_at_ms,
        verdict.verified_at_ms,
        policy,
        expected_profile,
    )?;
    if &computed != verdict {
        return Err(BindingError::Invalid(
            "coordinator hardware verdict differs from local verification".into(),
        ));
    }
    // The challenge was consumed while fresh. Its short replay window must not
    // invalidate a saved verdict, but vendor certificates and collateral must
    // still be valid at the time the verdict is used.
    verify_with_policy_at(
        evidence,
        now,
        verdict.verified_at_ms,
        policy,
        expected_profile,
    )?;
    Ok(computed)
}
fn verify_with_policy(
    evidence: &HardwareEvidence,
    now: u64,
    policy: &Policy,
) -> Result<HardwareVerdict> {
    verify_with_policy_at(evidence, now, now, policy, HardwareProfile::Standard)
}
fn verify_with_policy_at(
    evidence: &HardwareEvidence,
    now: u64,
    challenge_verified_at: u64,
    policy: &Policy,
    expected_profile: HardwareProfile,
) -> Result<HardwareVerdict> {
    evidence.challenge.validate(challenge_verified_at)?;
    if evidence.challenge.scope.profile != expected_profile
        || (expected_profile == HardwareProfile::PhalaEphemeralDstack061
            && (!matches!(evidence.guest, GuestEvidence::Tdx { .. }) || evidence.gpu_cc.is_some()))
    {
        return Err(BindingError::Invalid(
            "hardware profile differs from admitted lease".into(),
        ));
    }
    let bytes = serde_json::to_vec(evidence)?;
    if bytes.len() > MAX_EVIDENCE_BYTES {
        return Err(BindingError::Invalid("evidence too large".into()));
    }
    let challenge = &evidence.challenge;
    let scope = &challenge.scope;
    let time = DateTime::<Utc>::from_timestamp_millis(
        i64::try_from(now).map_err(|_| BindingError::Invalid("invalid time".into()))?,
    )
    .ok_or_else(|| BindingError::Invalid("invalid time".into()))?;
    let policy = policy
        .clone()
        .with_lease_verdict_ttl(Duration::milliseconds(VERDICT_TTL_MS as i64));
    let workload: [u8; 32] = hex::decode(&scope.workload_sha256)
        .map_err(|_| BindingError::Invalid("workload digest".into()))?
        .try_into()
        .map_err(|_| BindingError::Invalid("workload digest".into()))?;
    let (guest_identity, channel_key_fingerprint) = match &evidence.guest {
        GuestEvidence::SevSnp {
            report_base64,
            certificate_chain_base64,
        } => {
            check_chain(certificate_chain_base64)?;
            let attestation = GuestAttestation {
                node_id: scope.operator_pubkey_b58.clone(),
                lease_id: 0,
                challenge_id: challenge.challenge_id,
                kind: AttestationKind::SevSnp,
                report_base64: report_base64.clone(),
                certificate_chain_base64: certificate_chain_base64.clone(),
                guest_channel_key: challenge.guest_key_line(),
                collected_at: time,
                signature: String::new(),
            };
            let v = crate::verify_sev_snp_attestation(
                &attestation,
                &SnpExpectation {
                    report_data: challenge.guest_report_data_for_gpu(evidence.gpu_cc.as_ref())?,
                    host_data: workload,
                    chip_id_digest: None,
                },
                time,
                &policy,
            )?;
            (
                format!("snp/{}", v.guest.chip_id_digest),
                v.guest.channel_key_fingerprint,
            )
        }
        GuestEvidence::Tdx {
            quote_base64,
            collateral_json,
            events,
        } => {
            if quote_base64.len() > 32 * 1024
                || collateral_json.len() > 128 * 1024
                || events.len() > 256
            {
                return Err(BindingError::Invalid("TDX evidence exceeds limit".into()));
            }
            let parsed = parse_events(events)?;
            let quote = STANDARD
                .decode(quote_base64)
                .map_err(|_| BindingError::Invalid("quote base64".into()))?;
            let verifier = match expected_profile {
                HardwareProfile::Standard => crate::verify_tdx_lease_attestation,
                HardwareProfile::PhalaEphemeralDstack061 => {
                    crate::verify_tdx_ephemeral_lease_attestation
                }
            };
            let v = verifier(
                0,
                &scope.operator_pubkey_b58,
                &quote,
                collateral_json,
                &parsed,
                &challenge.guest_report_data_for_gpu(evidence.gpu_cc.as_ref())?,
                &workload,
                &challenge.guest_key_line(),
                time,
                &policy,
            )?;
            (v.device_identity, v.channel_key_fingerprint)
        }
    };
    let gpu_identity = if let Some(gpu) = &evidence.gpu_cc {
        check_chain(&gpu.certificate_chain_base64)?;
        if gpu.report_base64.len() > 64 * 1024 {
            return Err(BindingError::Invalid("GPU report too large".into()));
        }
        let report = STANDARD
            .decode(&gpu.report_base64)
            .map_err(|_| BindingError::Invalid("GPU report base64".into()))?;
        let chain = gpu
            .certificate_chain_base64
            .iter()
            .map(|c| {
                STANDARD
                    .decode(c)
                    .map_err(|_| BindingError::Invalid("GPU chain base64".into()))
            })
            .collect::<Result<Vec<_>>>()?;
        Some(
            crate::verify_nvidia_cc_lease_attestation(
                0,
                &scope.operator_pubkey_b58,
                &report,
                &chain,
                &challenge.gpu_nonce()?,
                time,
                &policy,
            )?
            .device_identity,
        )
    } else {
        None
    };
    let evidence_class = if gpu_identity.is_some() {
        TrustClass::Confidential
    } else {
        TrustClass::Attested
    };
    Ok(HardwareVerdict {
        challenge: challenge.clone(),
        evidence_sha256: hex::encode(Sha256::digest(bytes)),
        reference_sha256: reference_digest_for_profile(expected_profile),
        guest_identity,
        phala_identity: match (&evidence.guest, expected_profile) {
            (GuestEvidence::Tdx { events, .. }, HardwareProfile::PhalaEphemeralDstack061) => {
                Some(ephemeral_identity(events)?)
            }
            _ => None,
        },
        gpu_identity,
        channel_key_fingerprint,
        evidence_class,
        served_class: evidence_class.min(SERVED_TRUST_CEILING),
        verifier_version: match expected_profile {
            HardwareProfile::Standard => "covenant.hardware/1",
            HardwareProfile::PhalaEphemeralDstack061 => {
                "covenant.hardware/phala-ephemeral-dstack061/1"
            }
        }
        .into(),
        verified_at_ms: now,
        expires_at_ms: now
            .saturating_add(VERDICT_TTL_MS)
            .min(scope.lease_expires_at_ms),
    })
}
fn check_chain(chain: &[String]) -> Result<()> {
    if chain.is_empty() || chain.len() > 5 || chain.iter().any(|c| c.len() > 16 * 1024) {
        return Err(BindingError::Invalid(
            "certificate chain exceeds limits".into(),
        ));
    }
    Ok(())
}
pub fn parse_events(events: &[TdxEventEntry]) -> Result<Vec<TdxEvent>> {
    events
        .iter()
        .map(|e| {
            if e.event.len() > 256
                || e.event_payload.len() > 8192
                || e.digest.len() > 96
                || e.preimage.as_ref().is_some_and(|p| p.len() > 24 * 1024)
                || !matches!(e.version, 1 | 2)
            {
                return Err(BindingError::Invalid("event exceeds limits".into()));
            }
            Ok(TdxEvent {
                imr: e.imr,
                event_type: e.event_type,
                name: e.event.clone(),
                digest: hex::decode(&e.digest)
                    .map_err(|_| BindingError::Invalid("event digest".into()))?,
                payload: hex::decode(&e.event_payload)
                    .map_err(|_| BindingError::Invalid("event payload".into()))?,
                version: e.version,
                preimage: e
                    .preimage
                    .as_ref()
                    .map(hex::decode)
                    .transpose()
                    .map_err(|_| BindingError::Invalid("event preimage".into()))?,
            })
        })
        .collect()
}
fn auth_bytes(method: &str, path: &str, body: &[u8], timestamp: u64) -> Result<Vec<u8>> {
    let mut bytes = AUTH_DOMAIN.to_vec();
    bytes.extend(serde_jcs::to_vec(&(
        method,
        path,
        hex::encode(Sha256::digest(body)),
        timestamp,
    ))?);
    Ok(bytes)
}
pub fn sign_request(
    identity: &LocalIdentity,
    method: &str,
    path: &str,
    body: &[u8],
    timestamp: u64,
) -> Result<String> {
    Ok(bs58::encode(
        identity
            .sign(&auth_bytes(method, path, body, timestamp)?)
            .to_bytes(),
    )
    .into_string())
}
pub fn verify_request(
    operator: &str,
    signature: &str,
    method: &str,
    path: &str,
    body: &[u8],
    timestamp: u64,
    now: u64,
) -> Result<()> {
    if now.abs_diff(timestamp) > 30_000 || body.len() > MAX_EVIDENCE_BYTES {
        return Err(BindingError::Invalid(
            "stale or oversized signed request".into(),
        ));
    }
    let key: [u8; 32] = bs58::decode(operator)
        .into_vec()
        .map_err(|_| BindingError::Invalid("operator key".into()))?
        .try_into()
        .map_err(|_| BindingError::Invalid("operator key".into()))?;
    let sig: [u8; 64] = bs58::decode(signature)
        .into_vec()
        .map_err(|_| BindingError::Invalid("signature".into()))?
        .try_into()
        .map_err(|_| BindingError::Invalid("signature".into()))?;
    covenant_identity::verify_with_pubkey(
        key,
        &auth_bytes(method, path, body, timestamp)?,
        &ed25519_dalek::Signature::from_bytes(&sig),
    )
    .map_err(|_| BindingError::Invalid("signature rejected".into()))
}

#[cfg(test)]
#[path = "binding_tests.rs"]
mod tests;
