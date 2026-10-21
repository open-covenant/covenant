//! Explicit offline preparation, single-attempt submission, and verified probe.
use anyhow::{Context, Result};
use covenant_compute_attestation::vendor_types::TrustClass;
use covenant_compute_buyer::{
    hardware::{scope_for_phala_lease, HardwareClient},
    key_broker::KeyBrokerClient,
    phala::{self, PreparePhala},
    ssh::SshClient,
    SingleAttemptClient,
};
use covenant_compute_protocol::phala::{
    parse_phala_ephemeral_lease_spec, parse_phala_lease_spec, ApplicationKeyRecipient,
    PhalaRelayConfig,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

use super::{env_or, load_ctx, load_identity, parse_explicit_lease_job_id, prepared_home};

#[derive(Debug)]
struct PrepareArgs {
    job_id: Uuid,
    operator: String,
    image_digest: String,
    duration_secs: u64,
    rate: u64,
    ssh_key: PathBuf,
    output: PathBuf,
    relay: Option<PhalaRelayConfig>,
    application_key_recipient: Option<ApplicationKeyRecipient>,
}

fn flags(args: &[String], allowed: &[&str]) -> Result<BTreeMap<String, String>> {
    flags_with_optional(args, allowed, &[])
}

fn flags_with_optional(
    args: &[String],
    required: &[&str],
    optional: &[&str],
) -> Result<BTreeMap<String, String>> {
    let (pairs, remainder) = args.as_chunks::<2>();
    anyhow::ensure!(remainder.is_empty(), "every Phala flag requires one value");
    let mut values = BTreeMap::new();
    for pair in pairs {
        anyhow::ensure!(
            required.contains(&pair[0].as_str()) || optional.contains(&pair[0].as_str()),
            "unknown Phala flag {:?}",
            pair[0]
        );
        anyhow::ensure!(
            !pair[1].is_empty() && !pair[1].starts_with("--"),
            "missing value for {}",
            pair[0]
        );
        anyhow::ensure!(
            values.insert(pair[0].clone(), pair[1].clone()).is_none(),
            "duplicate flag {}",
            pair[0]
        );
    }
    for flag in required {
        anyhow::ensure!(values.contains_key(*flag), "missing {flag}");
    }
    Ok(values)
}

fn parse_prepare(args: &[String]) -> Result<PrepareArgs> {
    parse_prepare_profile(args, false)
}

fn parse_prepare_profile(args: &[String], ephemeral: bool) -> Result<PrepareArgs> {
    let mut allowed = vec![
        "--job-id",
        "--operator",
        "--image-digest",
        "--duration-secs",
        "--rate",
        "--ssh-key",
        "--output",
    ];
    if ephemeral {
        allowed.push("--relay-origin");
    }
    let optional = if ephemeral {
        vec!["--application-key-recipient"]
    } else {
        vec![]
    };
    let values = flags_with_optional(args, &allowed, &optional)?;
    let application_key_recipient = values
        .get("--application-key-recipient")
        .map(|path| phala::read_application_key_recipient(&PathBuf::from(path)))
        .transpose()?;
    let job_id = parse_explicit_lease_job_id(&values["--job-id"])?;
    let operator = values["--operator"].clone();
    covenant_compute_protocol::validate_address_b58("Phala operator", &operator)?;
    let image_digest = values["--image-digest"].clone();
    anyhow::ensure!(
        image_digest
            .strip_prefix("sha256:")
            .is_some_and(|digest| digest.len() == 64
                && digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))),
        "--image-digest must be sha256: followed by 64 lowercase hex characters"
    );
    let duration_secs: u64 = values["--duration-secs"]
        .parse()
        .context("--duration-secs must be an integer")?;
    let rate: u64 = values["--rate"]
        .parse()
        .context("--rate must be integer micro-USDC per second")?;
    anyhow::ensure!(
        (1..=3600).contains(&duration_secs) && rate > 0,
        "Phala duration must be 1..3600 seconds and rate must be positive"
    );
    anyhow::ensure!(
        duration_secs.checked_mul(rate).is_some(),
        "Phala lease ceiling overflows"
    );
    let relay = if ephemeral {
        let relay = PhalaRelayConfig {
            url: values["--relay-origin"].clone(),
            session_id: job_id.to_string(),
        };
        relay.validate()?;
        Some(relay)
    } else {
        None
    };
    Ok(PrepareArgs {
        job_id,
        operator,
        image_digest,
        duration_secs,
        rate,
        ssh_key: PathBuf::from(&values["--ssh-key"]),
        output: PathBuf::from(&values["--output"]),
        relay,
        application_key_recipient,
    })
}

pub fn prepare(args: &[String], json_out: bool) -> Result<()> {
    prepare_profile(parse_prepare(args)?, json_out)
}

pub fn prepare_ephemeral(args: &[String], json_out: bool) -> Result<()> {
    prepare_profile(parse_prepare_profile(args, true)?, json_out)
}

fn prepare_profile(args: PrepareArgs, json_out: bool) -> Result<()> {
    let options = PreparePhala {
        job_id: args.job_id,
        operator_pubkey_b58: args.operator,
        image_digest: args.image_digest,
        duration_secs: args.duration_secs,
        rate_micro_usdc_per_sec: args.rate,
        buyer_ssh_public_key: phala::read_public_key(&args.ssh_key)?,
    };
    let cap = env_or(
        "COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC",
        1_000_000,
        "must be a whole number of micro-USDC",
    )?;
    // Offline deliberately bypasses load_ctx: no coordinator is required.
    let buyer = load_identity(&prepared_home()?)?;
    let prepared = if let Some(relay) = &args.relay {
        phala::prepare_ephemeral_with_recipient_to_directory(
            &buyer,
            &options,
            relay,
            args.application_key_recipient.as_ref(),
            cap,
            &args.output,
        )?
    } else {
        phala::prepare_to_directory(&buyer, &options, cap, &args.output)?
    };
    let (expires, workload_sha256, profile) =
        if let Some(spec) = parse_phala_ephemeral_lease_spec(&prepared.envelope.payload.input)? {
            (
                spec.lease_expires_at_ms,
                spec.workload_sha256,
                "phala_ephemeral_dstack061",
            )
        } else {
            let spec = parse_phala_lease_spec(&prepared.envelope.payload.input)?
                .context("missing signed Phala workload")?;
            (spec.lease_expires_at_ms, spec.workload_sha256, "standard")
        };
    let (workload, envelope) = phala::output_paths(&args.output.canonicalize()?);
    let latest_submission = prepared
        .envelope
        .payload
        .issued_at_ms
        .saturating_add(covenant_compute_protocol::JOB_FRESHNESS_WINDOW_MS)
        .min(expires.saturating_sub(1));
    let mut summary = serde_json::json!({
        "job_id": args.job_id, "status": "prepared_offline",
        "issued_at_ms": prepared.envelope.payload.issued_at_ms,
        "latest_submission_at_ms": latest_submission,
        "lease_expires_at_ms": expires,
        "hardware_profile": profile,
        "relay": args.relay,
        "ceiling_micro_usdc": prepared.envelope.payload.price_micro_usdc,
        "workload_sha256": workload_sha256,
        "workload_file": workload, "envelope_file": envelope,
        "served_trust_ceiling": "open"
    });
    if let Some(recipient) = &args.application_key_recipient {
        summary["application_key_recipient"] = serde_json::to_value(recipient)?;
    }
    if json_out {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!(
            "Prepared {} offline; ceiling {} micro-USDC.",
            args.job_id, prepared.envelope.payload.price_micro_usdc
        );
        println!("Workload: {}", workload.display());
        println!("Signed envelope: {}", envelope.display());
        if args.relay.is_some() {
            println!("Fresh VM and one SSH connection per lease; a probe consumes that connection. Relay origin is buyer-signed.");
        }
        if args.application_key_recipient.is_some() {
            println!("Application-key recipient is buyer-signed; key delivery requires a separate explicit command and qualified broker.");
        }
        println!("Submit within 120 seconds and before guest expiry; stale preparation is never refreshed automatically. Served trust remains Open.");
    }
    Ok(())
}

pub async fn submit(args: &[String], json_out: bool) -> Result<()> {
    let values = flags(args, &["--envelope"])?;
    let envelope = phala::read_prepared_envelope(&PathBuf::from(&values["--envelope"]))?;
    let job_id = envelope.payload.job_id;
    let ctx = load_ctx()?;
    let accepted = phala::submit_prepared(
        &SingleAttemptClient::default(), &ctx.config, &ctx.identity,
        envelope, ctx.max_price_micro_usdc,
    ).await.with_context(|| format!(
        "Phala job {job_id}: preserve the original signed envelope; use budget reconcile and lease view for any uncertain result, never automatically resubmit or refresh the ID"
    ))?;
    if json_out {
        println!(
            "{}",
            serde_json::json!({
                "job_id": job_id, "status": "submission_acknowledged",
                "ceiling_micro_usdc": accepted.payload.price_micro_usdc,
                "served_trust_ceiling": "open"
            })
        );
    } else {
        println!(
            "Submitted {job_id} once; ceiling {} micro-USDC. Served trust remains Open.",
            accepted.payload.price_micro_usdc
        );
        println!("Read readiness with: covenant-compute lease view {job_id}");
    }
    Ok(())
}

pub async fn probe(args: &[String], json_out: bool) -> Result<()> {
    use_channel(args, json_out, false).await
}

pub async fn transfer(args: &[String], json_out: bool) -> Result<()> {
    use_channel(args, json_out, true).await
}

const DELIVERY_FLAGS: &[&str] = &[
    "--envelope",
    "--ssh-key",
    "--broker",
    "--coordinator-pubkey",
];

pub async fn deliver_key(args: &[String], json_out: bool) -> Result<()> {
    let values = flags(args, DELIVERY_FLAGS)?;
    let envelope = phala::read_prepared_envelope(&PathBuf::from(&values["--envelope"]))?;
    envelope.verify()?;
    envelope.payload.validate_input()?;
    let spec = parse_phala_ephemeral_lease_spec(&envelope.payload.input)?
        .context("application-key delivery requires an ephemeral Phala lease")?;
    anyhow::ensure!(
        spec.application_key_recipient.is_some(),
        "the original signed lease did not opt into application keys"
    );
    let ctx = load_ctx()?;
    anyhow::ensure!(
        envelope.payload.buyer.pubkey_base58() == ctx.identity.agent_id().pubkey_base58(),
        "prepared Phala lease belongs to another buyer"
    );
    // This pin is supplied independently from the verified coordinator
    // deployment identity, never learned from the response being checked.
    let coordinator = KeyBrokerClient::new(
        &ctx.config.coordinator_url,
        &ctx.identity,
        &values["--coordinator-pubkey"],
    )?;
    let hardware = HardwareClient::new(&ctx.config.coordinator_url, &ctx.identity)?;
    let view = covenant_compute_buyer::lease_view(
        &ctx.http,
        &ctx.config,
        &ctx.identity,
        envelope.payload.job_id,
    )
    .await?;
    if !json_out {
        println!("Key delivery consumes this lease's only SSH connection. Preserve the original envelope and job ID if the result is uncertain.");
    }
    let receipt = SshClient::default()
        .deliver_phala_key(
            &hardware,
            &coordinator,
            &ctx.identity,
            &envelope,
            &view,
            &PathBuf::from(&values["--ssh-key"]),
            &values["--broker"],
        )
        .await
        .with_context(|| format!(
            "application-key delivery for {}: preserve the original job and envelope; do not retry or purchase a replacement automatically",
            envelope.payload.job_id
        ))?;
    if json_out {
        println!(
            "{}",
            serde_json::json!({
                "job_id": envelope.payload.job_id,
                "receipt": receipt,
                "single_connection_consumed": true,
                "lease_close_command": format!("covenant-compute lease close {}", envelope.payload.job_id),
                "served_trust_ceiling": "open"
            })
        );
    } else {
        println!(
            "Isolated recipient confirmed application-key delivery for {}. Served trust remains Open.",
            envelope.payload.job_id
        );
        println!(
            "Close the lease to stop further billing: covenant-compute lease close {}",
            envelope.payload.job_id
        );
    }
    Ok(())
}

async fn use_channel(args: &[String], json_out: bool, transfer: bool) -> Result<()> {
    let allowed = if transfer {
        vec!["--envelope", "--ssh-key", "--source", "--basename"]
    } else {
        vec!["--envelope", "--ssh-key"]
    };
    let values = flags(args, &allowed)?;
    let envelope = phala::read_prepared_envelope(&PathBuf::from(&values["--envelope"]))?;
    let ctx = load_ctx()?;
    envelope.verify()?;
    envelope.payload.validate_input()?;
    anyhow::ensure!(
        envelope.payload.buyer.pubkey_base58() == ctx.identity.agent_id().pubkey_base58(),
        "prepared Phala lease belongs to another buyer"
    );
    let hardware = HardwareClient::new(&ctx.config.coordinator_url, &ctx.identity)?;
    let view = covenant_compute_buyer::lease_view(
        &ctx.http,
        &ctx.config,
        &ctx.identity,
        envelope.payload.job_id,
    )
    .await?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
    let scope = scope_for_phala_lease(
        &envelope,
        &view,
        &ctx.identity.agent_id().pubkey_base58(),
        now,
    )?;
    let ephemeral = scope.profile
        == covenant_compute_attestation::binding::HardwareProfile::PhalaEphemeralDstack061;
    if ephemeral && !json_out {
        println!("This operation consumes the ephemeral lease's only SSH connection; do not probe before a workspace transfer.");
    }
    let client = SshClient::default();
    let key = PathBuf::from(&values["--ssh-key"]);
    let output = if transfer {
        client
            .upload_file(
                &hardware,
                &scope,
                TrustClass::Open,
                &key,
                &PathBuf::from(&values["--source"]),
                &values["--basename"],
            )
            .await?
    } else {
        client
            .probe(&hardware, &scope, TrustClass::Open, &key)
            .await?
    };
    if json_out {
        println!(
            "{}",
            serde_json::json!({
                "job_id": envelope.payload.job_id,
                "status": if transfer { "diagnostic_transfer_succeeded" } else { "diagnostic_probe_succeeded" },
                "single_connection_consumed": ephemeral,
                "lease_close_command": format!("covenant-compute lease close {}", envelope.payload.job_id),
                "served_trust_ceiling": "open",
                "stdout": String::from_utf8_lossy(&output.stdout),
                "stderr": String::from_utf8_lossy(&output.stderr)
            })
        );
    } else {
        println!(
            "Diagnostic SSH {} succeeded for {}; served trust remains Open.",
            if transfer { "transfer" } else { "probe" },
            envelope.payload.job_id
        );
        print!("{}", String::from_utf8_lossy(&output.stdout));
        if !output.stderr.is_empty() {
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
        }
        println!(
            "Close the lease to stop further billing: covenant-compute lease close {}",
            envelope.payload.job_id
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn good() -> Vec<String> {
        vec![
            "--job-id".into(),
            "05446ce4-557a-4ee0-9e47-f47228bb703c".into(),
            "--operator".into(),
            bs58::encode([3; 32]).into_string(),
            "--image-digest".into(),
            format!("sha256:{}", "ab".repeat(32)),
            "--duration-secs".into(),
            "600".into(),
            "--rate".into(),
            "2".into(),
            "--ssh-key".into(),
            "/private/buyer.pub".into(),
            "--output".into(),
            "/private/prepared".into(),
        ]
    }
    #[test]
    fn phala_flags_are_explicit_and_ambiguous_or_unsafe_values_refuse() {
        let parsed = parse_prepare(&good()).unwrap();
        assert_eq!(parsed.duration_secs, 600);
        assert_eq!(parsed.rate, 2);
        for args in [
            Vec::new(),
            good()[..good().len() - 1].to_vec(),
            [good(), vec!["--rate".into(), "3".into()]].concat(),
            [good(), vec!["--force".into(), "true".into()]].concat(),
        ] {
            assert!(parse_prepare(&args).is_err());
        }
        for (flag, value) in [
            ("--job-id", "00000000-0000-0000-0000-000000000000"),
            ("--operator", "not-a-public-key"),
            ("--image-digest", "latest"),
            ("--duration-secs", "3601"),
            ("--duration-secs", "0"),
            ("--rate", "0"),
        ] {
            let mut args = good();
            let index = args.iter().position(|s| s == flag).unwrap();
            args[index + 1] = value.into();
            assert!(parse_prepare(&args).is_err(), "{flag}/{value}");
        }
    }
    #[test]
    fn ephemeral_requires_explicit_canonical_relay_and_cannot_alias_standard_prepare() {
        let args = [
            good(),
            vec!["--relay-origin".into(), "https://relay.example.com".into()],
        ]
        .concat();
        let parsed = parse_prepare_profile(&args, true).unwrap();
        assert_eq!(parsed.relay.unwrap().session_id, parsed.job_id.to_string());
        assert!(parse_prepare(&args).is_err());
        assert!(parse_prepare_profile(&good(), true).is_err());
        for origin in [
            "http://relay.example.com",
            "https://relay.example.com/",
            "https://u:p@relay.example.com",
            "https://relay.example.com:443",
        ] {
            let args = [good(), vec!["--relay-origin".into(), origin.into()]].concat();
            assert!(parse_prepare_profile(&args, true).is_err());
        }
        assert!(flags(
            &["--source".into(), "file".into()],
            &["--envelope", "--ssh-key", "--source", "--basename"]
        )
        .is_err());
    }

    #[test]
    fn recipient_flag_is_optional_only_for_ephemeral_and_strictly_read() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("recipient.json");
        let recipient = ApplicationKeyRecipient {
            protocol_version: 1,
            broker_id: "broker-test".into(),
            broker_response_pubkey_b58: covenant_identity::LocalIdentity::generate("broker")
                .agent_id()
                .pubkey_base58(),
            tenant_id: "tenant-test".into(),
            key_id: "application-test".into(),
            key_version: 1,
            purpose: "workspace".into(),
            recipient_image_digest: format!("sha256:{}", "ab".repeat(32)),
        };
        std::fs::write(&file, serde_json::to_vec(&recipient).unwrap()).unwrap();
        let base = [
            good(),
            vec!["--relay-origin".into(), "https://relay.example.com".into()],
        ]
        .concat();
        assert!(parse_prepare_profile(&base, true)
            .unwrap()
            .application_key_recipient
            .is_none());
        let optional = vec![
            "--application-key-recipient".into(),
            file.to_string_lossy().into_owned(),
        ];
        let args = [base.clone(), optional.clone()].concat();
        assert_eq!(
            parse_prepare_profile(&args, true)
                .unwrap()
                .application_key_recipient,
            Some(recipient)
        );
        assert!(parse_prepare(&[good(), optional.clone()].concat()).is_err());
        assert!(parse_prepare_profile(&[args.clone(), optional].concat(), true).is_err());
        std::fs::write(&file, b"{}").unwrap();
        assert!(parse_prepare_profile(&args, true).is_err());
        assert!(parse_prepare_profile(&args[..args.len() - 1], true).is_err());
    }

    #[test]
    fn submit_and_probe_cannot_infer_missing_paths_or_accept_extra_actions() {
        assert!(flags(
            &["--envelope".into(), "original.json".into()],
            &["--envelope"]
        )
        .is_ok());
        assert!(flags(&["original.json".into()], &["--envelope"]).is_err());
        assert!(flags(&["--envelope".into(), "--ssh-key".into()], &["--envelope"]).is_err());
        assert!(flags(
            &[
                "--envelope".into(),
                "original.json".into(),
                "--retry".into(),
                "yes".into()
            ],
            &["--envelope"]
        )
        .is_err());
        assert!(flags(
            &["--envelope".into(), "original.json".into()],
            &["--envelope", "--ssh-key"]
        )
        .is_err());
    }

    #[test]
    fn key_delivery_requires_explicit_paths_broker_and_independent_coordinator_pin() {
        let args: Vec<String> = [
            "--envelope",
            "original.json",
            "--ssh-key",
            "buyer.key",
            "--broker",
            "https://broker.example.com",
            "--coordinator-pubkey",
            "independently-pinned-public-key",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        assert!(flags(&args, DELIVERY_FLAGS).is_ok());
        for missing in 0..4 {
            let mut incomplete = args.clone();
            incomplete.drain(missing * 2..missing * 2 + 2);
            assert!(flags(&incomplete, DELIVERY_FLAGS).is_err());
        }
        for extra in [
            vec!["--retry".into(), "yes".into()],
            vec!["--coordinator-pubkey".into(), "replacement".into()],
            vec!["--job-id".into(), Uuid::new_v4().to_string()],
        ] {
            assert!(flags(&[args.clone(), extra].concat(), DELIVERY_FLAGS).is_err());
        }
        assert!(flags(&args[..args.len() - 1], DELIVERY_FLAGS).is_err());
    }
}
