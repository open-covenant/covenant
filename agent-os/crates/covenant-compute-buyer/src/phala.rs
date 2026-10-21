//! Offline preparation and explicit one-attempt purchase of a pinned Phala lease.
//! Preparation never contacts a provider or coordinator. Submission cannot
//! refresh the signed window, replace the UUID, or raise the Open trust ceiling.
use crate::{BuyerConfig, BuyerError, SingleAttemptClient};
use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
use covenant_compute_protocol::{
    lease_input,
    phala::{
        parse_phala_ephemeral_lease_spec, parse_phala_lease_spec, phala_ephemeral_lease_input,
        phala_ephemeral_workload_with_recipient, phala_lease_input, phala_workload,
        reviewed_phala_ephemeral_wrapper, reviewed_phala_wrapper,
        validate_phala_ephemeral_admission, ApplicationKeyRecipient, PhalaRelayConfig,
        PHALA_CPU_MODEL, PHALA_EPHEMERAL_CPU_MODEL, PHALA_EPHEMERAL_RESULT_SLACK_MS,
    },
    CapabilityRequirement, JobEnvelopePayload, JobKind, LeaseTerms, SignedJobEnvelope,
    JOB_FRESHNESS_WINDOW_MS, LEASE_DEADLINE_SLACK_MS,
};
use covenant_identity::LocalIdentity;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub const ENVELOPE_FILENAME: &str = "originalsignedenvelope.json";
pub const WORKLOAD_FILENAME: &str = "workload.json";
const MAX_ENVELOPE_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone)]
pub struct PreparePhala {
    pub job_id: Uuid,
    pub operator_pubkey_b58: String,
    pub image_digest: String,
    pub duration_secs: u64,
    pub rate_micro_usdc_per_sec: u64,
    pub buyer_ssh_public_key: String,
}

#[derive(Debug)]
pub struct PreparedPhala {
    pub envelope: SignedJobEnvelope,
    /// Exact compact JSON bytes measured by dstack; never pretty-print this.
    pub workload: String,
}

fn invalid(message: impl ToString) -> BuyerError {
    BuyerError::Protocol(message.to_string())
}

fn prepare_at(
    buyer: &LocalIdentity,
    args: &PreparePhala,
    max_price_micro_usdc: u64,
    now: u64,
) -> Result<PreparedPhala, BuyerError> {
    prepare_profile_at(buyer, args, None, max_price_micro_usdc, now)
}

fn prepare_profile_at(
    buyer: &LocalIdentity,
    args: &PreparePhala,
    relay: Option<&PhalaRelayConfig>,
    max_price_micro_usdc: u64,
    now: u64,
) -> Result<PreparedPhala, BuyerError> {
    prepare_profile_with_recipient_at(buyer, args, relay, None, max_price_micro_usdc, now)
}

pub(crate) fn prepare_profile_with_recipient_at(
    buyer: &LocalIdentity,
    args: &PreparePhala,
    relay: Option<&PhalaRelayConfig>,
    recipient: Option<&ApplicationKeyRecipient>,
    max_price_micro_usdc: u64,
    now: u64,
) -> Result<PreparedPhala, BuyerError> {
    if let Some(recipient) = recipient {
        recipient.validate().map_err(invalid)?;
        if relay.is_none() {
            return Err(invalid(
                "application-key recipients require an explicit ephemeral Phala relay lease",
            ));
        }
    }
    if args.job_id.is_nil() || !(1..=3600).contains(&args.duration_secs) {
        return Err(invalid(
            "Phala preparation requires a nonnil UUID and 1..3600 seconds",
        ));
    }
    let terms = LeaseTerms {
        max_duration_secs: args.duration_secs,
        rate_micro_usdc_per_sec: args.rate_micro_usdc_per_sec,
        client_public_key: Some(args.buyer_ssh_public_key.clone()),
    };
    terms.validate().map_err(invalid)?;
    let ceiling = terms.max_price_micro_usdc().map_err(invalid)?;
    if ceiling > max_price_micro_usdc {
        return Err(invalid(format!(
            "Phala ceiling {ceiling} micro-USDC exceeds the per-call cap {max_price_micro_usdc}"
        )));
    }
    let mut payload = JobEnvelopePayload {
        job_id: args.job_id,
        buyer: buyer.agent_id(),
        kind: JobKind::LeaseSession,
        capability_requirement: CapabilityRequirement {
            inference_tariff: None,
            gpu_class: Some("cpu".into()),
            min_vram_gb: None,
            model_id: Some(
                if relay.is_some() {
                    PHALA_EPHEMERAL_CPU_MODEL
                } else {
                    PHALA_CPU_MODEL
                }
                .into(),
            ),
            kind: JobKind::LeaseSession,
            max_duration_secs: args.duration_secs as u32,
            min_reputation_bps: None,
        },
        input: vec![lease_input(terms).map_err(invalid)?],
        price_micro_usdc: ceiling,
        deadline_ms: args.duration_secs * 1000
            + if relay.is_some() {
                PHALA_EPHEMERAL_RESULT_SLACK_MS
            } else {
                LEASE_DEADLINE_SLACK_MS
            },
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, args.job_id.to_string()),
        issued_at_ms: now,
        referral_code: None,
        stream: true,
    };
    let workload = if let Some(relay) = relay {
        let (workload, spec) = phala_ephemeral_workload_with_recipient(
            &payload,
            &args.operator_pubkey_b58,
            &reviewed_phala_ephemeral_wrapper(),
            &args.image_digest,
            relay,
            recipient,
        )
        .map_err(invalid)?;
        payload
            .input
            .push(phala_ephemeral_lease_input(spec).map_err(invalid)?);
        workload
    } else {
        let (workload, spec) = phala_workload(
            &payload,
            &args.operator_pubkey_b58,
            &reviewed_phala_wrapper(),
            &args.image_digest,
        )
        .map_err(invalid)?;
        payload
            .input
            .push(phala_lease_input(spec).map_err(invalid)?);
        workload
    };
    payload.validate_input().map_err(invalid)?;
    let envelope = SignedJobEnvelope::sign(payload, buyer).map_err(invalid)?;
    Ok(PreparedPhala { envelope, workload })
}

/// Prepare a new local authorization only. This has no network or budget side
/// effects and leaves partial output in place if persistence fails.
pub fn prepare_to_directory(
    buyer: &LocalIdentity,
    args: &PreparePhala,
    max_price_micro_usdc: u64,
    output: &Path,
) -> Result<PreparedPhala, BuyerError> {
    let prepared = prepare_at(buyer, args, max_price_micro_usdc, crate::epoch_ms())?;
    persist(&prepared, output)?;
    Ok(prepared)
}

/// Explicit fresh-VM, no-KMS preparation. The relay origin and session are
/// included in the measured workload and the buyer's signed authorization.
pub fn prepare_ephemeral_to_directory(
    buyer: &LocalIdentity,
    args: &PreparePhala,
    relay: &PhalaRelayConfig,
    max_price_micro_usdc: u64,
    output: &Path,
) -> Result<PreparedPhala, BuyerError> {
    prepare_ephemeral_with_recipient_to_directory(
        buyer,
        args,
        relay,
        None,
        max_price_micro_usdc,
        output,
    )
}

/// Optional buyer-signed application custody. None preserves the original
/// no-KMS recipe and envelope bytes; preparation itself never releases a key.
pub fn prepare_ephemeral_with_recipient_to_directory(
    buyer: &LocalIdentity,
    args: &PreparePhala,
    relay: &PhalaRelayConfig,
    recipient: Option<&ApplicationKeyRecipient>,
    max_price_micro_usdc: u64,
    output: &Path,
) -> Result<PreparedPhala, BuyerError> {
    let prepared = prepare_profile_with_recipient_at(
        buyer,
        args,
        Some(relay),
        recipient,
        max_price_micro_usdc,
        crate::epoch_ms(),
    )?;
    persist(&prepared, output)?;
    Ok(prepared)
}

fn persist(prepared: &PreparedPhala, output: &Path) -> Result<(), BuyerError> {
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut directory = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory.create(output).map_err(|_| invalid(
        "preparation output must be a new directory with an existing parent; existing output is never overwritten"
    ))?;
    File::open(parent)
        .and_then(|f| f.sync_all())
        .map_err(invalid)?;
    for (name, bytes) in [
        (WORKLOAD_FILENAME, prepared.workload.as_bytes().to_vec()),
        (
            ENVELOPE_FILENAME,
            serde_json::to_vec_pretty(&prepared.envelope).map_err(invalid)?,
        ),
    ] {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(output.join(name)).map_err(invalid)?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(invalid)?;
    }
    File::open(output)
        .and_then(|f| f.sync_all())
        .map_err(invalid)?;
    Ok(())
}

pub fn read_public_key(path: &Path) -> Result<String, BuyerError> {
    let bytes = read_regular_bounded(path, covenant_compute_protocol::MAX_CLIENT_KEY_BYTES)?;
    String::from_utf8(bytes)
        .map(|text| text.trim().to_owned())
        .map_err(invalid)
}

/// Read an explicit recipient configuration before creating any authorization.
/// A mutable image tag, unknown JSON field, symlink or oversized file fails closed.
pub fn read_application_key_recipient(path: &Path) -> Result<ApplicationKeyRecipient, BuyerError> {
    let bytes = read_regular_bounded(path, 16 * 1024)?;
    let recipient: ApplicationKeyRecipient = serde_json::from_slice(&bytes).map_err(invalid)?;
    recipient.validate().map_err(invalid)?;
    Ok(recipient)
}

pub fn read_prepared_envelope(path: &Path) -> Result<SignedJobEnvelope, BuyerError> {
    let bytes = read_regular_bounded(path, MAX_ENVELOPE_BYTES)?;
    serde_json::from_slice(&bytes).map_err(invalid)
}

fn read_regular_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, BuyerError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(invalid)?;
    let metadata = file.metadata().map_err(invalid)?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err(invalid("preparation input is not a bounded regular file"));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(invalid)?;
    if bytes.len() > limit {
        return Err(invalid("preparation input exceeds its limit"));
    }
    Ok(bytes)
}

/// Validate before budget reconciliation/reservation and again just before
/// dispatch. Expired or stale authorizations are never silently re-signed.
pub fn validate_prepared(
    envelope: &SignedJobEnvelope,
    expected_buyer: &str,
    max_price_micro_usdc: u64,
    now: u64,
) -> Result<(), BuyerError> {
    envelope.verify().map_err(invalid)?;
    let job = &envelope.payload;
    job.validate_input().map_err(invalid)?;
    validate_phala_ephemeral_admission(job).map_err(invalid)?;
    let expires = match (
        parse_phala_lease_spec(&job.input).map_err(invalid)?,
        parse_phala_ephemeral_lease_spec(&job.input).map_err(invalid)?,
    ) {
        (Some(spec), None) => spec.lease_expires_at_ms,
        (None, Some(spec)) => spec.lease_expires_at_ms,
        _ => {
            return Err(invalid(
                "prepared Phala envelope requires exactly one buyer-signed workload pin",
            ))
        }
    };
    if job.job_id.is_nil() || !job.stream || job.buyer.pubkey_base58() != expected_buyer {
        return Err(invalid(
            "prepared Phala envelope belongs to another buyer or is not a streaming lease",
        ));
    }
    if job.price_micro_usdc > max_price_micro_usdc {
        return Err(invalid(format!(
            "prepared ceiling {} micro-USDC exceeds the per-call cap {max_price_micro_usdc}",
            job.price_micro_usdc
        )));
    }
    if now < job.issued_at_ms {
        return Err(invalid(
            "prepared Phala issuance is in the future; nothing was submitted",
        ));
    }
    if now >= expires {
        return Err(invalid("prepared Phala guest window has expired; reconcile the original ID, never refresh it automatically"));
    }
    if now - job.issued_at_ms > JOB_FRESHNESS_WINDOW_MS {
        return Err(invalid(
            "prepared Phala envelope is older than the 120-second submission window; nothing was submitted; preserve and reconcile the original ID before explicitly preparing a new purchase"
        ));
    }
    Ok(())
}

pub async fn submit_prepared(
    http: &SingleAttemptClient,
    config: &BuyerConfig,
    buyer: &LocalIdentity,
    envelope: SignedJobEnvelope,
    max_price_micro_usdc: u64,
) -> Result<SignedJobEnvelope, BuyerError> {
    let url = reqwest::Url::parse(&config.coordinator_url).map_err(invalid)?;
    let loopback = matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "::1" | "[::1]")
    );
    if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(
            "Phala submission requires a bare HTTPS coordinator origin (HTTP only for loopback)",
        ));
    }
    let buyer_key = buyer.agent_id().pubkey_base58();
    crate::submit_prepared_envelope_once(http, config, buyer, envelope, |env, now| {
        validate_prepared(env, &buyer_key, max_price_micro_usdc, now)
    })
    .await
}

pub fn output_paths(directory: &Path) -> (PathBuf, PathBuf) {
    (
        directory.join(WORKLOAD_FILENAME),
        directory.join(ENVELOPE_FILENAME),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use sha2::{Digest, Sha256};

    fn args() -> PreparePhala {
        let key =
            STANDARD.encode([b"\0\0\0\x0bssh-ed25519\0\0\0\x20".as_slice(), &[7; 32]].concat());
        PreparePhala {
            job_id: Uuid::new_v4(),
            operator_pubkey_b58: bs58::encode([3; 32]).into_string(),
            image_digest: format!("sha256:{}", "ab".repeat(32)),
            duration_secs: 600,
            rate_micro_usdc_per_sec: 2,
            buyer_ssh_public_key: format!("ssh-ed25519 {key}"),
        }
    }

    #[test]
    fn preparation_persists_exact_signed_workload_once_and_privately() {
        let buyer = LocalIdentity::generate("prepare@phala");
        let args = args();
        let prepared = prepare_at(&buyer, &args, 1200, 1_000_000).unwrap();
        let spec = parse_phala_lease_spec(&prepared.envelope.payload.input)
            .unwrap()
            .unwrap();
        assert_eq!(
            spec.workload_sha256,
            Sha256::digest(prepared.workload.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        assert_eq!(spec.operator_pubkey_b58, args.operator_pubkey_b58);
        assert_eq!(spec.lease_expires_at_ms, 1_600_000);
        prepared.envelope.verify().unwrap();
        prepared.envelope.payload.validate_input().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("prepared");
        persist(&prepared, &output).unwrap();
        assert_eq!(
            std::fs::read(output.join(WORKLOAD_FILENAME)).unwrap(),
            prepared.workload.as_bytes()
        );
        assert_eq!(
            read_prepared_envelope(&output.join(ENVELOPE_FILENAME)).unwrap(),
            prepared.envelope
        );
        assert!(persist(&prepared, &output).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&output).unwrap().permissions().mode() & 0o777,
                0o700
            );
            for name in [WORKLOAD_FILENAME, ENVELOPE_FILENAME] {
                assert_eq!(
                    std::fs::metadata(output.join(name))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }
    }

    #[test]
    fn stale_future_expired_foreign_over_cap_and_missing_pin_preparations_refuse() {
        let buyer = LocalIdentity::generate("prepare@phala");
        let prepared = prepare_at(&buyer, &args(), 1200, 1_000_000).unwrap();
        let owner = buyer.agent_id().pubkey_base58();
        validate_prepared(&prepared.envelope, &owner, 1200, 1_120_000).unwrap();
        for (now, expected_buyer, cap) in [
            (999_999, owner.as_str(), 1200),
            (1_120_001, owner.as_str(), 1200),
            (1_600_000, owner.as_str(), 1200),
            (1_000_000, "another-buyer", 1200),
            (1_000_000, owner.as_str(), 1199),
        ] {
            assert!(validate_prepared(&prepared.envelope, expected_buyer, cap, now).is_err());
        }
        let mut payload = prepared.envelope.payload;
        payload.input.pop();
        let missing = SignedJobEnvelope::sign(payload, &buyer).unwrap();
        assert!(validate_prepared(&missing, &owner, 1200, 1_000_000).is_err());
        assert!(prepare_at(&buyer, &args(), 1199, 1_000_000).is_err());
    }

    #[test]
    fn ephemeral_preparation_signs_exact_image_relay_and_distinct_profile() {
        let buyer = LocalIdentity::generate("ephemeral@buyer");
        let args = args();
        let relay = PhalaRelayConfig {
            url: "https://relay.example.com".into(),
            session_id: args.job_id.to_string(),
        };
        let prepared = prepare_profile_at(&buyer, &args, Some(&relay), 1200, 1_000_000).unwrap();
        let spec = parse_phala_ephemeral_lease_spec(&prepared.envelope.payload.input)
            .unwrap()
            .unwrap();
        assert!(parse_phala_lease_spec(&prepared.envelope.payload.input)
            .unwrap()
            .is_none());
        assert_eq!(
            prepared
                .envelope
                .payload
                .capability_requirement
                .model_id
                .as_deref(),
            Some(PHALA_EPHEMERAL_CPU_MODEL)
        );
        assert_eq!(spec.relay, relay);
        assert_eq!(
            prepared.envelope.payload.deadline_ms,
            600_000 + PHALA_EPHEMERAL_RESULT_SLACK_MS
        );
        assert_eq!(spec.lease_expires_at_ms, 1_600_000);
        assert_eq!(prepared.envelope.payload.price_micro_usdc, 1200);
        assert_eq!(
            spec.workload_sha256,
            Sha256::digest(prepared.workload.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        assert!(prepared.workload.contains(&args.image_digest));
        let mut other_relay = relay.clone();
        other_relay.url = "https://other.example.com".into();
        let other = prepare_profile_at(&buyer, &args, Some(&other_relay), 1200, 1_000_000).unwrap();
        assert_ne!(prepared.workload, other.workload);
        let workload: serde_json::Value = serde_json::from_str(&prepared.workload).unwrap();
        assert_eq!(workload["key_provider"], "none");
        assert_eq!(workload["kms_enabled"], false);
        let owner = buyer.agent_id().pubkey_base58();
        validate_prepared(&prepared.envelope, &owner, 1200, 1_120_000).unwrap();
        let mut legacy = prepared.envelope.payload.clone();
        legacy.deadline_ms = 600_000 + LEASE_DEADLINE_SLACK_MS;
        let legacy = SignedJobEnvelope::sign(legacy, &buyer).unwrap();
        legacy.verify().unwrap();
        legacy.payload.validate_input().unwrap();
        assert!(validate_prepared(&legacy, &owner, 1200, 1_120_000).is_err());
        for now in [999_999, 1_120_001, 1_600_000] {
            assert!(validate_prepared(&prepared.envelope, &owner, 1200, now).is_err());
        }
        let mut changed = relay.clone();
        changed.session_id = Uuid::new_v4().to_string();
        assert!(prepare_profile_at(&buyer, &args, Some(&changed), 1200, 1_000_000).is_err());
        for url in [
            "http://relay.example.com",
            "https://127.0.0.1",
            "https://relay.example.com/path",
            "https://relay.example.com/",
        ] {
            changed = relay.clone();
            changed.url = url.into();
            assert!(prepare_profile_at(&buyer, &args, Some(&changed), 1200, 1_000_000).is_err());
        }
        let mut unsigned = prepared.envelope.payload.clone();
        unsigned.capability_requirement.model_id = Some(PHALA_CPU_MODEL.into());
        let mixed = SignedJobEnvelope::sign(unsigned, &buyer).unwrap();
        assert!(validate_prepared(&mixed, &owner, 1200, 1_000_000).is_err());
    }

    #[test]
    fn optional_recipient_none_preserves_exact_legacy_bytes_and_opt_in_is_signed() {
        let buyer = LocalIdentity::generate("recipient@buyer");
        let args = args();
        let relay = PhalaRelayConfig {
            url: "https://relay.example.com".into(),
            session_id: args.job_id.to_string(),
        };
        let legacy = prepare_profile_at(&buyer, &args, Some(&relay), 1200, 1_000_000).unwrap();
        let none =
            prepare_profile_with_recipient_at(&buyer, &args, Some(&relay), None, 1200, 1_000_000)
                .unwrap();
        assert_eq!(legacy.workload.as_bytes(), none.workload.as_bytes());
        assert_eq!(
            serde_json::to_vec(&legacy.envelope).unwrap(),
            serde_json::to_vec(&none.envelope).unwrap()
        );
        let recipient = ApplicationKeyRecipient {
            protocol_version: 1,
            broker_id: "covenant-broker-test".into(),
            broker_response_pubkey_b58: LocalIdentity::generate("broker")
                .agent_id()
                .pubkey_base58(),
            tenant_id: "tenant-test".into(),
            key_id: "workspace-test".into(),
            key_version: 1,
            purpose: "workspace".into(),
            recipient_image_digest: format!("sha256:{}", "cd".repeat(32)),
        };
        let opted = prepare_profile_with_recipient_at(
            &buyer,
            &args,
            Some(&relay),
            Some(&recipient),
            1200,
            1_000_000,
        )
        .unwrap();
        opted.envelope.verify().unwrap();
        opted.envelope.payload.validate_input().unwrap();
        assert_eq!(
            parse_phala_ephemeral_lease_spec(&opted.envelope.payload.input)
                .unwrap()
                .unwrap()
                .application_key_recipient,
            Some(recipient.clone())
        );
        assert_ne!(legacy.workload, opted.workload);
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("opted");
        persist(&opted, &output).unwrap();
        assert_eq!(
            read_prepared_envelope(&output.join(ENVELOPE_FILENAME)).unwrap(),
            opted.envelope
        );
        assert!(prepare_profile_with_recipient_at(
            &buyer,
            &args,
            None,
            Some(&recipient),
            1200,
            1_000_000
        )
        .is_err());
        let mut invalid = recipient;
        invalid.recipient_image_digest = "latest".into();
        assert!(prepare_profile_with_recipient_at(
            &buyer,
            &args,
            Some(&relay),
            Some(&invalid),
            1200,
            1_000_000
        )
        .is_err());
    }

    #[test]
    fn recipient_file_is_bounded_strict_and_never_follows_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("recipient.json");
        std::fs::write(&file, b"{}").unwrap();
        assert!(read_application_key_recipient(&file).is_err());
        std::fs::write(&file, vec![b' '; 16 * 1024 + 1]).unwrap();
        assert!(read_application_key_recipient(&file).is_err());
        assert!(read_application_key_recipient(directory.path()).is_err());
        #[cfg(unix)]
        {
            let link = directory.path().join("link.json");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            assert!(read_application_key_recipient(&link).is_err());
        }
    }

    #[tokio::test]
    async fn invalid_preparation_is_refused_before_budget_or_network() {
        let buyer = LocalIdentity::generate("prepare@phala");
        let prepared = prepare_at(&buyer, &args(), 1200, 1_000_000).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = BuyerConfig {
            coordinator_url: format!("http://{}", listener.local_addr().unwrap()),
            poll_interval: std::time::Duration::from_millis(1),
            referral_code: None,
            rpc_url: None,
        };
        let error = submit_prepared(
            &SingleAttemptClient::default(),
            &config,
            &buyer,
            prepared.envelope,
            1200,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("expired"), "{error}");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), listener.accept())
                .await
                .is_err()
        );
    }
}
