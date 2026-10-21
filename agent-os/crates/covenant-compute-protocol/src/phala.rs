//! Buyer-signed, per-lease Phala workload pins. These authorize a workload;
//! they do not certify its security or raise the released hardware trust tier.
use crate::{parse_lease_terms, JobEnvelopePayload, JobKind, ProtocolError};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use covenant_mcp::Content;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const PHALA_CPU_MODEL: &str = "phala-cpu-dstack061";
pub const PHALA_EPHEMERAL_CPU_MODEL: &str = "phala-cpu-ephemeral-dstack061";
pub const PHALA_GATEWAY: &str = "dstack-pha-prod5.phala.network";
pub const PHALA_KMS_SPKI: &str = "3059301306072a8648ce3d020106082a8648ce3d030107034200048844eb42ccdf8c52fd4f174f362fcb9bbd19c45fd48f1edec2d8f1ca23536ec1a74021b4cee610c074f8294d431b2b7fee2c39e5333fdaf0a4522d43fb159d9f";
pub const PHALA_PRE_LAUNCH_SHA256: &str =
    "982181610f70be9087b1c69b36b719b47b82d37fcef8acc9289ed3bb3095ffe8";
pub const PHALA_EPHEMERAL_PRE_LAUNCH_SHA256: &str =
    "7a343993a5c15903b17198f7da721512be98f5dd553583ded861ce300c1fdfa3";
const COMPOSE_TEMPLATE: &str = include_str!("phala-compose.template.yaml");
const EPHEMERAL_COMPOSE_TEMPLATE: &str = include_str!("phala-ephemeral-compose.template.yaml");
/// Retained public v3 wrapper with the reviewed provider pre-launch script.
/// Its per-lease Docker Compose is generated below, never reused across jobs.
pub fn reviewed_phala_wrapper() -> Value {
    serde_json::from_str(include_str!("phala-wrapper-v3.json"))
        .expect("checked-in Phala wrapper is valid JSON")
}

/// Separate, single-boot profile: no hosted key provider, gateway or sealed env.
/// Every recovery requires a fresh VM and disk; this wire profile never permits
/// reuse or silently falls back to the managed-KMS profile.
pub fn reviewed_phala_ephemeral_wrapper() -> Value {
    serde_json::from_str(include_str!("phala-ephemeral-wrapper-v3.json"))
        .expect("checked-in ephemeral Phala wrapper is valid JSON")
}

fn invalid(message: &str) -> ProtocolError {
    ProtocolError::Invalid(message.into())
}
fn hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn sha256(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhalaLeaseSpec {
    pub schema_version: u8,
    pub job_id: Uuid,
    pub operator_pubkey_b58: String,
    pub lease_generation: String,
    /// Absolute guest deadline starts at signed issuance, not provider readiness.
    /// Queueing and boot therefore cannot extend this buyer-approved boundary.
    pub lease_expires_at_ms: u64,
    pub workload_sha256: String,
}

impl PhalaLeaseSpec {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.schema_version != 1
            || self.job_id.is_nil()
            || self.lease_generation != format!("phala-{}-1", self.job_id)
            || self.lease_expires_at_ms == 0
            || self.lease_expires_at_ms > (1u64 << 53) - 1
            || !hex_digest(&self.workload_sha256)
        {
            return Err(invalid(
                "invalid Phala lease identity, expiry or workload pin",
            ));
        }
        crate::validate_address_b58("Phala operator", &self.operator_pubkey_b58)?;
        Ok(())
    }

    pub fn validate_for_job(&self, job: &JobEnvelopePayload) -> Result<(), ProtocolError> {
        self.validate()?;
        let terms =
            parse_lease_terms(&job.input)?.ok_or_else(|| invalid("Phala lease terms missing"))?;
        let expected_expiry = job
            .issued_at_ms
            .checked_add(
                terms
                    .max_duration_secs
                    .checked_mul(1000)
                    .ok_or_else(|| invalid("Phala lease expiry overflow"))?,
            )
            .ok_or_else(|| invalid("Phala lease expiry overflow"))?;
        if self.job_id != job.job_id
            || job.kind != JobKind::LeaseSession
            || job.capability_requirement.gpu_class.as_deref() != Some("cpu")
            || job.capability_requirement.model_id.as_deref() != Some(PHALA_CPU_MODEL)
            || job.capability_requirement.min_vram_gb.is_some()
            || terms.max_duration_secs > 3600
            || self.lease_expires_at_ms != expected_expiry
        {
            return Err(invalid(
                "Phala workload must bind this CPU lease and its one-hour maximum window",
            ));
        }
        buyer_key(job)?;
        Ok(())
    }
}

fn buyer_key(job: &JobEnvelopePayload) -> Result<String, ProtocolError> {
    let terms =
        parse_lease_terms(&job.input)?.ok_or_else(|| invalid("Phala lease terms missing"))?;
    let raw = terms
        .client_public_key
        .ok_or_else(|| invalid("Phala lease requires the buyer SSH public key"))?;
    let fields: Vec<_> = raw.split_whitespace().collect();
    if fields.len() < 2 || fields[0] != "ssh-ed25519" {
        return Err(invalid(
            "Phala lease requires an Ed25519 buyer SSH public key",
        ));
    }
    let key = crate::ssh::SshHostKey {
        algorithm: fields[0].into(),
        key_base64: fields[1].into(),
    };
    key.validate()?;
    let bytes = STANDARD
        .decode(&key.key_base64)
        .map_err(|_| invalid("invalid buyer key"))?;
    if STANDARD.encode(bytes) != key.key_base64 {
        return Err(invalid("noncanonical buyer key"));
    }
    Ok(format!("{} {}", key.algorithm, key.key_base64))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputBlock {
    phala_lease: PhalaLeaseSpec,
}
pub fn phala_lease_input(spec: PhalaLeaseSpec) -> Result<Content, ProtocolError> {
    spec.validate()?;
    Ok(Content::json(
        serde_json::to_value(InputBlock { phala_lease: spec })
            .map_err(|_| invalid("Phala lease serialization failed"))?,
    ))
}
pub fn parse_phala_lease_spec(input: &[Content]) -> Result<Option<PhalaLeaseSpec>, ProtocolError> {
    let mut result = None;
    for content in input {
        let Content::Json { value } = content else {
            continue;
        };
        if value.get("phala_lease").is_none() {
            continue;
        }
        if result.is_some() {
            return Err(invalid("duplicate Phala lease block"));
        }
        let block: InputBlock = serde_json::from_value(value.clone())
            .map_err(|_| invalid("malformed Phala lease block"))?;
        block.phala_lease.validate()?;
        result = Some(block.phala_lease);
    }
    Ok(result)
}

/// Exact measured v3 wrapper from a reviewed provider preflight. The provider's
/// API input is a different schema: callers must compare its provisioned hash
/// to these bytes before creating a CVM, never silently normalize this pin.
fn reviewed_wrapper(wrapper: &Value) -> Result<(), ProtocolError> {
    let object = wrapper
        .as_object()
        .ok_or_else(|| invalid("Phala wrapper must be an object"))?;
    let fields = [
        "allowed_envs",
        "docker_compose_file",
        "features",
        "gateway_enabled",
        "init_script",
        "key_provider",
        "key_provider_id",
        "kms_enabled",
        "local_key_provider_enabled",
        "manifest_version",
        "name",
        "no_instance_id",
        "pre_launch_script",
        "public_logs",
        "public_sysinfo",
        "public_tcbinfo",
        "runner",
        "secure_time",
        "storage_fs",
        "swap_size",
        "tproxy_enabled",
    ];
    if object.len() != fields.len() || fields.iter().any(|key| !object.contains_key(*key)) {
        return Err(invalid("unreviewed Phala wrapper fields"));
    }
    let expected = json!({
        "allowed_envs": ["COVENANT_COLLECTOR_TOKEN"],
        "features": ["kms", "tproxy-net"], "gateway_enabled": true,
        "init_script": [], "key_provider": "kms", "key_provider_id": PHALA_KMS_SPKI,
        "kms_enabled": true, "local_key_provider_enabled": false,
        "manifest_version": "3", "name": "", "no_instance_id": false,
        "runner": "docker-compose", "secure_time": false,
        "storage_fs": "ext4", "swap_size": "0", "tproxy_enabled": true
    });
    if expected
        .as_object()
        .unwrap()
        .iter()
        .any(|(k, v)| object.get(k) != Some(v))
        || ["public_logs", "public_sysinfo", "public_tcbinfo"]
            .iter()
            .any(|key| !object[*key].is_boolean())
        || !object["docker_compose_file"].is_string()
        || object["pre_launch_script"]
            .as_str()
            .is_none_or(|script| sha256(script.as_bytes()) != PHALA_PRE_LAUNCH_SHA256)
    {
        return Err(invalid(
            "Phala wrapper differs from the reviewed guest profile",
        ));
    }
    Ok(())
}

/// Prepare the exact workload and pin before signing a new job. This is pure:
/// it creates no resource, changes no lease, and performs no key release.
pub fn phala_workload(
    job: &JobEnvelopePayload,
    operator: &str,
    wrapper_template: &Value,
    image_digest: &str,
) -> Result<(String, PhalaLeaseSpec), ProtocolError> {
    reviewed_wrapper(wrapper_template)?;
    let digest = image_digest
        .strip_prefix("sha256:")
        .filter(|d| hex_digest(d))
        .ok_or_else(|| invalid("immutable guest image digest required"))?;
    let terms = parse_lease_terms(&job.input)?.ok_or_else(|| invalid("lease terms missing"))?;
    let mut spec = PhalaLeaseSpec {
        schema_version: 1,
        job_id: job.job_id,
        operator_pubkey_b58: operator.into(),
        lease_generation: format!("phala-{}-1", job.job_id),
        lease_expires_at_ms: job
            .issued_at_ms
            .checked_add(
                terms
                    .max_duration_secs
                    .checked_mul(1000)
                    .ok_or_else(|| invalid("lease expiry overflow"))?,
            )
            .ok_or_else(|| invalid("lease expiry overflow"))?,
        workload_sha256: "0".repeat(64),
    };
    spec.validate_for_job(job)?;
    let policy = json!({
        "schema_version": 1, "job_id": job.job_id,
        "operator_pubkey_b58": operator,
        "buyer_pubkey_b58": job.buyer.pubkey_base58(),
        "lease_generation": spec.lease_generation,
        "lease_expires_at_ms": spec.lease_expires_at_ms,
        "gateway_domain": PHALA_GATEWAY,
        "buyer_ssh_public_key": buyer_key(job)?,
        "max_challenges": 32
    });
    let encoded = STANDARD
        .encode(serde_json::to_vec(&policy).map_err(|_| invalid("policy serialization failed"))?);
    let compose = COMPOSE_TEMPLATE
        .replace("__IMAGE_DIGEST__", digest)
        .replace("__LEASE_POLICY_B64__", &encoded)
        .replace("__JOB_ID__", &job.job_id.to_string());
    let mut wrapper = wrapper_template.clone();
    wrapper["docker_compose_file"] = Value::String(compose);
    let exact =
        serde_json::to_string(&wrapper).map_err(|_| invalid("workload serialization failed"))?;
    spec.workload_sha256 = sha256(exact.as_bytes());
    Ok((exact, spec))
}

pub fn render_phala_manifest(
    job: &JobEnvelopePayload,
    operator: &str,
    spec: &PhalaLeaseSpec,
    wrapper_template: &Value,
    image_digest: &str,
) -> Result<String, ProtocolError> {
    spec.validate_for_job(job)?;
    let (exact, computed) = phala_workload(job, operator, wrapper_template, image_digest)?;
    if &computed != spec {
        return Err(invalid(
            "rendered Phala workload differs from the buyer-signed pin",
        ));
    }
    Ok(exact)
}

/// Public routing configuration. The relay transports the attested channel;
/// these fields are not authentication credentials or a hardware verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhalaRelayConfig {
    pub url: String,
    pub session_id: String,
}

impl PhalaRelayConfig {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.host()?;
        let session = Uuid::parse_str(&self.session_id)
            .map_err(|_| invalid("invalid Phala relay session identity"))?;
        if session.is_nil() || session.to_string() != self.session_id {
            return Err(invalid("noncanonical Phala relay session identity"));
        }
        Ok(())
    }

    /// The only dial target is this canonical DNS origin on HTTPS port 443.
    /// DNS resolution and transport authorization remain the caller's duty.
    pub fn host(&self) -> Result<&str, ProtocolError> {
        let host = self
            .url
            .strip_prefix("https://")
            .ok_or_else(|| invalid("Phala relay requires a canonical HTTPS origin"))?;
        if host.len() > 253
            || !host.contains('.')
            || host.parse::<std::net::IpAddr>().is_ok()
            || host.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
            })
            || host.rsplit('.').next().is_none_or(|suffix| {
                suffix.bytes().all(|byte| byte.is_ascii_digit())
                    || matches!(suffix, "local" | "localhost" | "internal")
            })
        {
            return Err(invalid(
                "Phala relay requires a canonical public DNS origin",
            ));
        }
        Ok(host)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhalaEphemeralLeaseSpec {
    pub schema_version: u8,
    pub job_id: Uuid,
    pub operator_pubkey_b58: String,
    pub lease_generation: String,
    pub lease_expires_at_ms: u64,
    pub workload_sha256: String,
    pub relay: PhalaRelayConfig,
}

/// Provider deletion confirmation is separate from guest access and billing.
pub const PHALA_EPHEMERAL_CLEANUP_GRACE_MS: u64 = 300_000;
/// New orders reserve cleanup time and the ordinary result-delivery margin.
pub const PHALA_EPHEMERAL_RESULT_SLACK_MS: u64 =
    PHALA_EPHEMERAL_CLEANUP_GRACE_MS + crate::LEASE_DEADLINE_SLACK_MS;

/// Admission policy for new orders, not historical signature/workload validation.
/// Old signed envelopes and journals remain readable without gaining a new grace.
pub fn validate_phala_ephemeral_admission(job: &JobEnvelopePayload) -> Result<(), ProtocolError> {
    let Some(spec) = parse_phala_ephemeral_lease_spec(&job.input)? else {
        return Ok(());
    };
    spec.validate_for_job(job)?;
    let required = spec
        .lease_expires_at_ms
        .checked_add(PHALA_EPHEMERAL_RESULT_SLACK_MS)
        .ok_or_else(|| invalid("ephemeral Phala result deadline overflow"))?;
    let deadline = job
        .issued_at_ms
        .checked_add(job.deadline_ms)
        .ok_or_else(|| invalid("ephemeral Phala result deadline overflow"))?;
    if deadline < required {
        return Err(invalid("new ephemeral Phala lease requires 300s cleanup and 60s result-delivery slack beyond its unchanged guest expiry"));
    }
    Ok(())
}

impl PhalaEphemeralLeaseSpec {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.relay.validate()?;
        if self.schema_version != 1
            || self.job_id.is_nil()
            || self.lease_generation != format!("phala-ephemeral-{}-1", self.job_id)
            || self.relay.session_id != self.job_id.to_string()
            || self.lease_expires_at_ms == 0
            || self.lease_expires_at_ms > (1u64 << 53) - 1
            || !hex_digest(&self.workload_sha256)
        {
            return Err(invalid(
                "invalid ephemeral Phala lease identity, relay or workload pin",
            ));
        }
        crate::validate_address_b58("Phala operator", &self.operator_pubkey_b58)?;
        Ok(())
    }

    pub fn validate_for_job(&self, job: &JobEnvelopePayload) -> Result<(), ProtocolError> {
        self.validate()?;
        let terms =
            parse_lease_terms(&job.input)?.ok_or_else(|| invalid("Phala lease terms missing"))?;
        let expected_expiry = job
            .issued_at_ms
            .checked_add(
                terms
                    .max_duration_secs
                    .checked_mul(1000)
                    .ok_or_else(|| invalid("Phala lease expiry overflow"))?,
            )
            .ok_or_else(|| invalid("Phala lease expiry overflow"))?;
        if self.job_id != job.job_id
            || job.kind != JobKind::LeaseSession
            || job.capability_requirement.gpu_class.as_deref() != Some("cpu")
            || job.capability_requirement.model_id.as_deref() != Some(PHALA_EPHEMERAL_CPU_MODEL)
            || job.capability_requirement.min_vram_gb.is_some()
            || terms.max_duration_secs > 3600
            || self.lease_expires_at_ms != expected_expiry
        {
            return Err(invalid(
                "ephemeral Phala workload must bind this CPU lease and its one-hour maximum window",
            ));
        }
        buyer_key(job)?;
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EphemeralInputBlock {
    phala_ephemeral_lease: PhalaEphemeralLeaseSpec,
}

pub fn phala_ephemeral_lease_input(
    spec: PhalaEphemeralLeaseSpec,
) -> Result<Content, ProtocolError> {
    spec.validate()?;
    Ok(Content::json(
        serde_json::to_value(EphemeralInputBlock {
            phala_ephemeral_lease: spec,
        })
        .map_err(|_| invalid("ephemeral Phala lease serialization failed"))?,
    ))
}

pub fn parse_phala_ephemeral_lease_spec(
    input: &[Content],
) -> Result<Option<PhalaEphemeralLeaseSpec>, ProtocolError> {
    let mut result = None;
    for content in input {
        let Content::Json { value } = content else {
            continue;
        };
        if value.get("phala_ephemeral_lease").is_none() {
            continue;
        }
        if result.is_some() {
            return Err(invalid("duplicate ephemeral Phala lease block"));
        }
        let block: EphemeralInputBlock = serde_json::from_value(value.clone())
            .map_err(|_| invalid("malformed ephemeral Phala lease block"))?;
        block.phala_ephemeral_lease.validate()?;
        result = Some(block.phala_ephemeral_lease);
    }
    Ok(result)
}

/// Shared lifecycle pin. Never infer a Phala profile from remote evidence.
/// Both profiles keep their original wire encoding and validation rules.
pub fn phala_lease_window(
    job: &JobEnvelopePayload,
) -> Result<Option<(String, u64)>, ProtocolError> {
    let managed = parse_phala_lease_spec(&job.input)?;
    let ephemeral = parse_phala_ephemeral_lease_spec(&job.input)?;
    match (managed, ephemeral) {
        (Some(_), Some(_)) => Err(invalid("mixed Phala lease profiles")),
        (Some(spec), None) => {
            spec.validate_for_job(job)?;
            Ok(Some((spec.operator_pubkey_b58, spec.lease_expires_at_ms)))
        }
        (None, Some(spec)) => {
            spec.validate_for_job(job)?;
            Ok(Some((spec.operator_pubkey_b58, spec.lease_expires_at_ms)))
        }
        (None, None) => {
            if matches!(
                job.capability_requirement.model_id.as_deref(),
                Some(PHALA_CPU_MODEL | PHALA_EPHEMERAL_CPU_MODEL)
            ) {
                return Err(invalid("Phala model requires its signed lease pin"));
            }
            Ok(None)
        }
    }
}

fn reviewed_ephemeral_wrapper(wrapper: &Value) -> Result<(), ProtocolError> {
    let mut candidate = wrapper.clone();
    let object = candidate
        .as_object_mut()
        .ok_or_else(|| invalid("ephemeral Phala wrapper must be an object"))?;
    if !object
        .get("docker_compose_file")
        .is_some_and(Value::is_string)
        || object
            .get("pre_launch_script")
            .and_then(Value::as_str)
            .is_none_or(|script| sha256(script.as_bytes()) != PHALA_EPHEMERAL_PRE_LAUNCH_SHA256)
    {
        return Err(invalid(
            "unreviewed ephemeral Phala workload or pre-launch script",
        ));
    }
    object.insert("docker_compose_file".into(), Value::String(String::new()));
    if candidate != reviewed_phala_ephemeral_wrapper() {
        return Err(invalid(
            "ephemeral Phala wrapper differs from the reviewed single-boot profile",
        ));
    }
    Ok(())
}

/// Generate a new single-boot workload. This is a pure preparation step and
/// does not authorize a provider retry, VM restart, disk reuse or key release.
pub fn phala_ephemeral_workload(
    job: &JobEnvelopePayload,
    operator: &str,
    wrapper_template: &Value,
    image_digest: &str,
    relay: &PhalaRelayConfig,
) -> Result<(String, PhalaEphemeralLeaseSpec), ProtocolError> {
    reviewed_ephemeral_wrapper(wrapper_template)?;
    let digest = image_digest
        .strip_prefix("sha256:")
        .filter(|digest| hex_digest(digest))
        .ok_or_else(|| invalid("immutable guest image digest required"))?;
    let terms = parse_lease_terms(&job.input)?.ok_or_else(|| invalid("lease terms missing"))?;
    let mut spec = PhalaEphemeralLeaseSpec {
        schema_version: 1,
        job_id: job.job_id,
        operator_pubkey_b58: operator.into(),
        lease_generation: format!("phala-ephemeral-{}-1", job.job_id),
        lease_expires_at_ms: job
            .issued_at_ms
            .checked_add(
                terms
                    .max_duration_secs
                    .checked_mul(1000)
                    .ok_or_else(|| invalid("lease expiry overflow"))?,
            )
            .ok_or_else(|| invalid("lease expiry overflow"))?,
        workload_sha256: "0".repeat(64),
        relay: relay.clone(),
    };
    spec.validate_for_job(job)?;
    let policy = json!({
        "schema_version": 1, "profile": "phala_ephemeral_dstack061",
        "job_id": job.job_id, "operator_pubkey_b58": operator,
        "buyer_pubkey_b58": job.buyer.pubkey_base58(),
        "lease_generation": spec.lease_generation,
        "lease_expires_at_ms": spec.lease_expires_at_ms,
        "relay_endpoint": relay.url, "relay_session_id": relay.session_id,
        "buyer_ssh_public_key": buyer_key(job)?, "max_challenges": 8
    });
    let encoded = STANDARD
        .encode(serde_json::to_vec(&policy).map_err(|_| invalid("policy serialization failed"))?);
    let compose = EPHEMERAL_COMPOSE_TEMPLATE
        .replace("__IMAGE_DIGEST__", digest)
        .replace("__LEASE_POLICY_B64__", &encoded)
        .replace("__JOB_ID__", &job.job_id.to_string());
    if compose.contains("__IMAGE_DIGEST__")
        || compose.contains("__LEASE_POLICY_B64__")
        || compose.contains("__JOB_ID__")
    {
        return Err(invalid("unresolved ephemeral Phala workload template"));
    }
    let mut wrapper = wrapper_template.clone();
    wrapper["docker_compose_file"] = Value::String(compose);
    let exact =
        serde_json::to_string(&wrapper).map_err(|_| invalid("workload serialization failed"))?;
    spec.workload_sha256 = sha256(exact.as_bytes());
    Ok((exact, spec))
}

pub fn render_phala_ephemeral_manifest(
    job: &JobEnvelopePayload,
    operator: &str,
    spec: &PhalaEphemeralLeaseSpec,
    wrapper_template: &Value,
    image_digest: &str,
) -> Result<String, ProtocolError> {
    spec.validate_for_job(job)?;
    let (exact, computed) =
        phala_ephemeral_workload(job, operator, wrapper_template, image_digest, &spec.relay)?;
    if &computed != spec {
        return Err(invalid(
            "rendered ephemeral Phala workload differs from the buyer-signed pin",
        ));
    }
    Ok(exact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lease_input, CapabilityRequirement, LeaseTerms};
    use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
    use covenant_types::AgentId;

    fn job() -> JobEnvelopePayload {
        let key =
            STANDARD.encode([b"\0\0\0\x0bssh-ed25519\0\0\0\x20".as_slice(), &[0x9e; 32]].concat());
        JobEnvelopePayload {
            job_id: Uuid::parse_str("05446ce4-557a-4ee0-9e47-f47228bb703c").unwrap(),
            buyer: AgentId::new("buyer", [1; 32]),
            kind: JobKind::LeaseSession,
            capability_requirement: CapabilityRequirement {
                inference_tariff: None,
                gpu_class: Some("cpu".into()),
                min_vram_gb: None,
                model_id: Some(PHALA_CPU_MODEL.into()),
                kind: JobKind::LeaseSession,
                max_duration_secs: 600,
                min_reputation_bps: None,
            },
            input: vec![lease_input(LeaseTerms {
                max_duration_secs: 600,
                rate_micro_usdc_per_sec: 1,
                client_public_key: Some(format!("ssh-ed25519 {key} buyer-comment")),
            })
            .unwrap()],
            price_micro_usdc: 600,
            deadline_ms: 900_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "phala"),
            issued_at_ms: 1_792_540_800_000,
            referral_code: None,
            stream: true,
        }
    }
    fn operator() -> String {
        bs58::encode([2; 32]).into_string()
    }
    fn image() -> String {
        format!("sha256:{}", "ab".repeat(32))
    }

    fn ephemeral_job() -> JobEnvelopePayload {
        let mut job = job();
        job.capability_requirement.model_id = Some(PHALA_EPHEMERAL_CPU_MODEL.into());
        job
    }

    fn relay(job: &JobEnvelopePayload) -> PhalaRelayConfig {
        PhalaRelayConfig {
            url: "https://relay.example.com".into(),
            session_id: job.job_id.to_string(),
        }
    }

    #[test]
    fn cleanup_slack_is_new_admission_policy_without_rewriting_guest_or_history() {
        let mut job = ephemeral_job();
        let (manifest, spec) = phala_ephemeral_workload(
            &job,
            &operator(),
            &reviewed_phala_ephemeral_wrapper(),
            &image(),
            &relay(&job),
        )
        .unwrap();
        job.input
            .push(phala_ephemeral_lease_input(spec.clone()).unwrap());
        // The old envelope still authenticates its original workload and expiry.
        job.deadline_ms = 600_000 + crate::LEASE_DEADLINE_SLACK_MS;
        job.validate_input().unwrap();
        spec.validate_for_job(&job).unwrap();
        assert!(validate_phala_ephemeral_admission(&job).is_err());
        for slack in [
            PHALA_EPHEMERAL_RESULT_SLACK_MS - 1,
            PHALA_EPHEMERAL_RESULT_SLACK_MS,
        ] {
            job.deadline_ms = 600_000 + slack;
            assert_eq!(
                validate_phala_ephemeral_admission(&job).is_ok(),
                slack == PHALA_EPHEMERAL_RESULT_SLACK_MS
            );
        }
        assert_eq!(
            render_phala_ephemeral_manifest(
                &job,
                &operator(),
                &spec,
                &reviewed_phala_ephemeral_wrapper(),
                &image(),
            )
            .unwrap(),
            manifest
        );
        assert_eq!(spec.lease_expires_at_ms, job.issued_at_ms + 600_000);
        assert_eq!(job.price_micro_usdc, 600);
        job.deadline_ms = u64::MAX;
        assert!(validate_phala_ephemeral_admission(&job).is_err());
        assert!(validate_phala_ephemeral_admission(&self::job()).is_ok());
    }

    #[test]
    fn workload_is_deterministic_bound_to_lease_and_requires_buyer_pin() {
        let mut job = job();
        let wrapper = reviewed_phala_wrapper();
        let (manifest, spec) = phala_workload(&job, &operator(), &wrapper, &image()).unwrap();
        assert!(
            job.validate_input().is_err(),
            "no implicit unpinned Phala workload"
        );
        job.input.push(phala_lease_input(spec.clone()).unwrap());
        job.validate_input().unwrap();
        assert_eq!(
            render_phala_manifest(&job, &operator(), &spec, &wrapper, &image()).unwrap(),
            manifest
        );
        assert_eq!(sha256(manifest.as_bytes()), spec.workload_sha256);
        assert_eq!(spec.lease_expires_at_ms, job.issued_at_ms + 600_000);
        let parsed: Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(parsed["swap_size"], "0");
        let compose = parsed["docker_compose_file"].as_str().unwrap();
        assert!(compose.contains("18090:8090"));
        assert!(!compose.contains("__LEASE_POLICY_B64__"));
        assert!(!compose.contains("__JOB_ID__"));
        assert!(compose.contains(&format!("collector-state-{}:/state", job.job_id)));
        for change in 0..5 {
            let mut altered = job.clone();
            match change {
                0 => altered.job_id = Uuid::new_v4(),
                1 => altered.issued_at_ms += 1,
                2 => altered.buyer = AgentId::new("other-buyer", [3; 32]),
                3 => altered.capability_requirement.gpu_class = Some("h100".into()),
                _ => altered.capability_requirement.model_id = None,
            }
            assert!(
                render_phala_manifest(&altered, &operator(), &spec, &wrapper, &image()).is_err()
            );
        }
        assert!(render_phala_manifest(
            &job,
            &bs58::encode([4; 32]).into_string(),
            &spec,
            &wrapper,
            &image()
        )
        .is_err());
        assert!(render_phala_manifest(
            &job,
            &operator(),
            &spec,
            &wrapper,
            &format!("sha256:{}", "cd".repeat(32))
        )
        .is_err());
    }

    #[test]
    fn ambiguous_input_and_unreviewed_wrapper_changes_fail_closed() {
        let mut job = job();
        let wrapper = reviewed_phala_wrapper();
        let (_, spec) = phala_workload(&job, &operator(), &wrapper, &image()).unwrap();
        let block = phala_lease_input(spec.clone()).unwrap();
        assert!(parse_phala_lease_spec(&[block.clone(), block.clone()]).is_err());
        let mut value = serde_json::to_value(InputBlock { phala_lease: spec }).unwrap();
        value["phala_lease"]["new_field"] = json!(true);
        assert!(parse_phala_lease_spec(&[Content::json(value)]).is_err());
        job.input.push(block);
        for (key, value) in [
            ("pre_launch_script", json!("echo altered")),
            ("key_provider_id", json!("untrusted")),
            ("no_instance_id", json!(true)),
            ("local_key_provider_enabled", json!(true)),
            ("secure_time", json!(true)),
            ("swap_size", json!("1")),
            ("swap_size", json!(0)),
            ("swap_size", Value::Null),
            ("new_privileged_field", json!(true)),
        ] {
            let mut altered = wrapper.clone();
            altered[key] = value;
            assert!(
                phala_workload(&job, &operator(), &altered, &image()).is_err(),
                "{key}"
            );
        }
        let mut absent_swap = wrapper.clone();
        absent_swap.as_object_mut().unwrap().remove("swap_size");
        assert!(phala_workload(&job, &operator(), &absent_swap, &image()).is_err());
    }

    #[test]
    fn reviewed_compose_and_source_recipe_do_not_drift() {
        // Both files are packaged independently: Rust consumers build from the
        // agent-os tree, while the guest builder uses deploy/. Compare in CI.
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../deploy/compute-attested-cpu/compose.template.yaml");
        if source.exists() {
            assert_eq!(std::fs::read_to_string(source).unwrap(), COMPOSE_TEMPLATE);
        }
        reviewed_wrapper(&reviewed_phala_wrapper()).unwrap();
    }

    #[test]
    fn ephemeral_workload_binds_public_relay_and_exact_single_boot_policy() {
        let mut job = ephemeral_job();
        let wrapper = reviewed_phala_ephemeral_wrapper();
        let relay = relay(&job);
        assert!(job.validate_input().is_err());
        let (manifest, spec) =
            phala_ephemeral_workload(&job, &operator(), &wrapper, &image(), &relay).unwrap();
        job.input
            .push(phala_ephemeral_lease_input(spec.clone()).unwrap());
        job.validate_input().unwrap();
        assert_eq!(
            render_phala_ephemeral_manifest(&job, &operator(), &spec, &wrapper, &image()).unwrap(),
            manifest
        );
        assert_eq!(sha256(manifest.as_bytes()), spec.workload_sha256);
        assert_eq!(spec.relay, relay);
        assert_eq!(
            spec.lease_generation,
            format!("phala-ephemeral-{}-1", job.job_id)
        );
        assert_eq!(spec.lease_expires_at_ms, job.issued_at_ms + 600_000);
        let parsed: Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(parsed["key_provider"], "none");
        assert!(parsed.get("key_provider_id").is_none());
        assert_eq!(parsed["swap_size"], "0");
        assert_eq!(parsed["allowed_envs"], json!([]));
        assert_eq!(parsed["features"], json!([]));
        for field in [
            "kms_enabled",
            "gateway_enabled",
            "tproxy_enabled",
            "local_key_provider_enabled",
        ] {
            assert_eq!(parsed[field], false);
        }
        let compose = parsed["docker_compose_file"].as_str().unwrap();
        assert!(!compose.contains("__LEASE_POLICY_B64__"));
        assert!(!compose.contains("__JOB_ID__"));
        assert!(!compose.contains("COVENANT_COLLECTOR_TOKEN"));
        let encoded = compose
            .lines()
            .find_map(|line| line.trim().strip_prefix("COVENANT_LEASE_POLICY_B64: \""))
            .unwrap()
            .strip_suffix('"')
            .unwrap();
        let policy: Value = serde_json::from_slice(&STANDARD.decode(encoded).unwrap()).unwrap();
        assert_eq!(policy["schema_version"], 1);
        assert_eq!(policy["profile"], "phala_ephemeral_dstack061");
        assert_eq!(policy["max_challenges"], 8);
        assert_eq!(policy["relay_endpoint"], relay.url);
        assert_eq!(policy["relay_session_id"], job.job_id.to_string());
        assert!(policy.get("gateway_domain").is_none());

        let mut redirected = spec.clone();
        redirected.relay.url = "https://other-relay.example.com".into();
        assert!(render_phala_ephemeral_manifest(
            &job,
            &operator(),
            &redirected,
            &wrapper,
            &image()
        )
        .is_err());
        let mut renewed = spec.clone();
        renewed.lease_generation = format!("phala-ephemeral-{}-2", job.job_id);
        assert!(renewed.validate().is_err());
        let mut another_session = relay.clone();
        another_session.session_id = Uuid::new_v4().to_string();
        assert!(
            phala_ephemeral_workload(&job, &operator(), &wrapper, &image(), &another_session)
                .is_err()
        );
        for change in 0..4 {
            let mut altered = job.clone();
            match change {
                0 => altered.job_id = Uuid::new_v4(),
                1 => altered.issued_at_ms += 1,
                2 => altered.buyer = AgentId::new("other-buyer", [3; 32]),
                _ => altered.capability_requirement.model_id = Some(PHALA_CPU_MODEL.into()),
            }
            assert!(render_phala_ephemeral_manifest(
                &altered,
                &operator(),
                &spec,
                &wrapper,
                &image()
            )
            .is_err());
        }
        assert!(render_phala_ephemeral_manifest(
            &job,
            &bs58::encode([4; 32]).into_string(),
            &spec,
            &wrapper,
            &image()
        )
        .is_err());
        assert!(render_phala_ephemeral_manifest(
            &job,
            &operator(),
            &spec,
            &wrapper,
            &format!("sha256:{}", "cd".repeat(32))
        )
        .is_err());
    }

    #[test]
    fn ephemeral_profile_never_accepts_managed_blocks_or_wrapper_variants() {
        let mut job = ephemeral_job();
        let wrapper = reviewed_phala_ephemeral_wrapper();
        let relay = relay(&job);
        let (_, spec) =
            phala_ephemeral_workload(&job, &operator(), &wrapper, &image(), &relay).unwrap();
        let block = phala_ephemeral_lease_input(spec.clone()).unwrap();
        assert!(parse_phala_ephemeral_lease_spec(&[block.clone(), block.clone()]).is_err());
        let mut unknown = serde_json::to_value(EphemeralInputBlock {
            phala_ephemeral_lease: spec,
        })
        .unwrap();
        unknown["phala_ephemeral_lease"]["relay"]["credential"] = json!("forbidden");
        assert!(parse_phala_ephemeral_lease_spec(&[Content::json(unknown)]).is_err());
        let mut managed_job = self::job();
        let (_, managed) = phala_workload(
            &managed_job,
            &operator(),
            &reviewed_phala_wrapper(),
            &image(),
        )
        .unwrap();
        let managed_block = phala_lease_input(managed).unwrap();
        job.input.push(managed_block.clone());
        assert!(job.validate_input().is_err());
        job.input.push(block.clone());
        assert!(job.validate_input().is_err());
        managed_job.input.push(block);
        assert!(managed_job.validate_input().is_err());
        assert!(phala_workload(
            &ephemeral_job(),
            &operator(),
            &reviewed_phala_wrapper(),
            &image()
        )
        .is_err());
        assert!(
            phala_ephemeral_workload(&self::job(), &operator(), &wrapper, &image(), &relay)
                .is_err()
        );
        assert!(phala_ephemeral_workload(
            &ephemeral_job(),
            &operator(),
            &reviewed_phala_wrapper(),
            &image(),
            &relay
        )
        .is_err());

        for (field, value) in [
            ("key_provider", json!("kms")),
            ("key_provider_id", json!("")),
            ("key_provider_id", Value::Null),
            ("kms_enabled", json!(true)),
            ("gateway_enabled", json!(true)),
            ("tproxy_enabled", json!(true)),
            ("features", json!(["kms"])),
            ("allowed_envs", json!(["SECRET"])),
            ("pre_launch_script", json!("echo changed")),
            ("swap_size", json!("1")),
            ("swap_size", json!(0)),
            ("new_field", json!(true)),
            ("public_logs", json!(false)),
        ] {
            let mut changed = wrapper.clone();
            changed[field] = value;
            assert!(
                phala_ephemeral_workload(&ephemeral_job(), &operator(), &changed, &image(), &relay)
                    .is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn relay_origin_has_one_canonical_host_and_job_session() {
        let job = ephemeral_job();
        let good = relay(&job);
        good.validate().unwrap();
        assert_eq!(good.host().unwrap(), "relay.example.com");
        for endpoint in [
            "http://relay.example.com",
            "https://relay.example.com/",
            "https://relay.example.com/path",
            "https://relay.example.com:443",
            "https://user@relay.example.com",
            "https://relay.example.com?q=1",
            "https://relay.example.com#fragment",
            "https://RELAY.example.com",
            "https://relay.example.com.",
            "https://relay..example.com",
            "https://-relay.example.com",
            "https://relay-.example.com",
            "https://127.0.0.1",
            "https://[::1]",
            "https://localhost",
            "https://relay.local",
            "https://relay.internal",
            "https://relay.localhost",
            "https://relay.example.com\n",
            "https://relay_1.example.com",
        ] {
            let mut config = good.clone();
            config.url = endpoint.into();
            assert!(config.validate().is_err(), "{endpoint}");
        }
        for session in [
            job.job_id.simple().to_string(),
            job.job_id.to_string().to_uppercase(),
            Uuid::nil().to_string(),
            "../session".into(),
        ] {
            let mut config = good.clone();
            config.session_id = session;
            assert!(config.validate().is_err());
        }
    }

    #[test]
    fn ephemeral_compose_matches_the_guest_source_recipe() {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../deploy/compute-attested-cpu/compose.ephemeral.template.yaml");
        if source.exists() {
            assert_eq!(
                std::fs::read_to_string(source).unwrap(),
                EPHEMERAL_COMPOSE_TEMPLATE
            );
        }
        let wrapper = reviewed_phala_ephemeral_wrapper();
        reviewed_ephemeral_wrapper(&wrapper).unwrap();
        let prelaunch = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../deploy/compute-attested-cpu/ephemeral-pre-launch.sh");
        if prelaunch.exists() {
            let script = std::fs::read_to_string(prelaunch).unwrap();
            assert_eq!(wrapper["pre_launch_script"], script);
            assert_eq!(sha256(script.as_bytes()), PHALA_EPHEMERAL_PRE_LAUNCH_SHA256);
        }
        assert_ne!(PHALA_EPHEMERAL_PRE_LAUNCH_SHA256, PHALA_PRE_LAUNCH_SHA256);
    }
}
