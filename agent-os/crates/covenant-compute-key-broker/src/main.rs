//! Bounded JSON-line transport for an enclave-owned process. The parent may
//! relay public requests and temporary credentials, never policy or plaintext.
use covenant_compute_key_broker::{Broker, BrokerPolicy, MAX_REQUEST_BYTES, WorkloadPolicy};
#[cfg(feature = "nitro")]
use std::io::Write;
use std::{
    io::{self, BufRead, Read},
    path::Path,
};

#[cfg(feature = "nitro")]
mod nitro;

const EMBEDDED_POLICY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/broker-policy.json"));

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DeploymentPolicy {
    broker: BrokerPolicy,
    signing: SigningPolicy,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SigningPolicy {
    kms_key_arn: String,
    wrapped_key_base64: String,
    public_key_b58: String,
}

impl DeploymentPolicy {
    fn validate(&self) -> Result<(), Box<dyn std::error::Error>> {
        use base64::Engine as _;
        Broker::new(self.broker.clone())?;
        let public: [u8; 32] = bs58::decode(&self.signing.public_key_b58)
            .into_vec()?
            .try_into()
            .map_err(|_| "invalid signing public key")?;
        let parsed = ed25519_dalek::VerifyingKey::from_bytes(&public)?;
        if parsed.is_weak()
            || self
                .broker
                .keys
                .iter()
                .any(|k| k.kms_key_arn != self.signing.kms_key_arn)
        {
            return Err("signing key must use the same custody root".into());
        }
        if self.broker.keys.iter().any(|key| {
            matches!(&key.workload,
            WorkloadPolicy::NativeTemplate { broker_response_pubkey_b58, .. }
            if broker_response_pubkey_b58 != &self.signing.public_key_b58)
        }) {
            return Err("native recipient must pin the enclave signing identity".into());
        }
        let blob =
            base64::engine::general_purpose::STANDARD.decode(&self.signing.wrapped_key_base64)?;
        if blob.is_empty() || blob.len() > 6144 {
            return Err("invalid signing envelope".into());
        }
        Ok(())
    }
}

fn main() {
    if run().is_err() {
        eprintln!("key broker command failed; reconcile issued requests before retrying");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [command] if command == "--help" => {
            println!(
                "covenant-compute-key-broker check-policy POLICY.json\ncovenant-compute-key-broker serve\ncovenant-compute-key-broker enroll-signing\nserve and enrollment require the nitro feature, an embedded build-time policy and Nitro hardware."
            );
            Ok(())
        }
        [command, path] if command == "check-policy" => {
            let file = std::fs::File::open(Path::new(path))?;
            let mut bytes = Vec::new();
            file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > 1024 * 1024 {
                return Err("policy too large".into());
            }
            let policy: DeploymentPolicy = serde_json::from_slice(&bytes)?;
            policy.validate()?;
            println!(
                "Policy structure valid. This does not qualify custody, hardware or deployment."
            );
            Ok(())
        }
        [command] if command == "serve" => serve(),
        [command] if command == "enroll-signing" => enroll_signing(),
        _ => Err("invalid command; use --help".into()),
    }
}

/// Take at most one bounded line. A partial or oversized line is fatal, never
/// resynchronized as another privileged request.
fn read_line(reader: &mut impl BufRead) -> io::Result<Option<zeroize::Zeroizing<Vec<u8>>>> {
    let mut line = zeroize::Zeroizing::new(Vec::new());
    reader
        .take((MAX_REQUEST_BYTES + 1) as u64)
        .read_until(b'\n', &mut line)?;
    if line.is_empty() {
        return Ok(None);
    }
    if line.len() > MAX_REQUEST_BYTES || !line.ends_with(b"\n") {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid frame"));
    }
    Ok(Some(line))
}

#[cfg(not(feature = "nitro"))]
fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let _ = EMBEDDED_POLICY;
    let _ = read_line(&mut &b""[..]);
    Err("build has no Nitro backend".into())
}

#[cfg(feature = "nitro")]
fn serve() -> Result<(), Box<dyn std::error::Error>> {
    use covenant_compute_key_broker::{
        ChallengeRequest, KeyPolicy, ReleaseRequest, SignedReleaseResponse, encryption_context,
        release_response_bytes,
    };
    use ed25519_dalek::{Signer, SigningKey};
    #[derive(serde::Deserialize)]
    #[serde(
        tag = "operation",
        content = "request",
        rename_all = "snake_case",
        deny_unknown_fields
    )]
    enum Command {
        Credentials(nitro::SessionCredentials),
        Challenge(Box<ChallengeRequest>),
        Release(Box<ReleaseRequest>),
    }
    if EMBEDDED_POLICY.is_empty() || !Path::new("/dev/nsm").exists() {
        return Err("embedded policy and Nitro hardware required".into());
    }
    let policy: DeploymentPolicy = serde_json::from_slice(EMBEDDED_POLICY)?;
    policy.validate()?;
    let region = policy
        .broker
        .keys
        .first()
        .ok_or("no keys")?
        .kms_key_arn
        .split(':')
        .nth(3)
        .ok_or("invalid key ARN")?
        .to_owned();
    if policy
        .broker
        .keys
        .iter()
        .any(|k| k.kms_key_arn.split(':').nth(3) != Some(region.as_str()))
    {
        return Err("all keys must use the embedded region".into());
    }
    let signing_context = encryption_context(
        &policy.broker.broker_id,
        &KeyPolicy {
            tenant_id: policy.broker.broker_id.clone(),
            key_id: "broker-response-signing".into(),
            key_version: 1,
            purpose: "broker-response-signing".into(),
            // The authority is known before ciphertext-only seed generation. Using
            // the derived signing public key here would create a bootstrap cycle.
            owner_pubkey_b58: policy.broker.coordinator_pubkey_b58.clone(),
            workload: WorkloadPolicy::FixedQualificationHash {
                workload_sha256: "0".repeat(64),
            },
            kms_key_arn: policy.signing.kms_key_arn.clone(),
            wrapped_key_base64: policy.signing.wrapped_key_base64.clone(),
        },
    );
    let mut broker = Broker::new(policy.broker)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut kms = None;
    let mut signing_key: Option<SigningKey> = None;
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    while let Some(line) = read_line(&mut reader)? {
        // Deliberately do not print parse errors: malformed credential input
        // could otherwise be echoed to the untrusted parent or log collector.
        let command: Command = serde_json::from_slice(&line).map_err(|_| "invalid frame")?;
        let response = match command {
            Command::Credentials(credentials) => {
                let backend = nitro::NitroKms::new(&region, credentials)?;
                kms = Some(backend);
                serde_json::json!({"ok":true})
            }
            Command::Challenge(request) => match broker.issue_challenge(*request) {
                Ok(value) => serde_json::json!({"ok":true,"result":value}),
                Err(_) => serde_json::json!({"ok":false,"error":"challenge_rejected"}),
            },
            Command::Release(request) => {
                let backend = kms.as_ref().ok_or("credentials required")?;
                match broker.authorize_release(*request) {
                    Ok(authorized) => {
                        // Invalid public challenges must not cause billable KMS
                        // calls. Only a fully authorized release reaches this
                        // signing bootstrap; any failure remains consumed.
                        if signing_key.is_none() {
                            let seed = runtime.block_on(backend.decrypt(
                                &policy.signing.kms_key_arn,
                                &policy.signing.wrapped_key_base64,
                                &signing_context,
                            ))?;
                            let key = SigningKey::from_bytes(&seed);
                            if bs58::encode(key.verifying_key().as_bytes()).into_string()
                                != policy.signing.public_key_b58
                            {
                                return Err("signing identity mismatch".into());
                            }
                            signing_key = Some(key);
                        }
                        // The private permit retains the original monotonic
                        // deadlines across the signing-key unwrap and rechecks
                        // them before application-key decryption or delivery.
                        match runtime.block_on(authorized.release(backend)) {
                            Ok(response) => {
                                let key =
                                    signing_key.as_ref().ok_or("signing identity required")?;
                                let signature_b58 = bs58::encode(
                                    key.sign(&release_response_bytes(&response)?).to_bytes(),
                                )
                                .into_string();
                                serde_json::json!({"ok":true,"result":SignedReleaseResponse {
                                    response, broker_pubkey_b58:policy.signing.public_key_b58.clone(), signature_b58,
                                }})
                            }
                            Err(_) => {
                                serde_json::json!({"ok":false,"error":"release_rejected_challenge_consumed"})
                            }
                        }
                    }
                    Err(_) => {
                        serde_json::json!({"ok":false,"error":"release_rejected_challenge_consumed"})
                    }
                }
            }
        };
        serde_json::to_writer(&mut output, &response)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(())
}

#[cfg(not(feature = "nitro"))]
fn enroll_signing() -> Result<(), Box<dyn std::error::Error>> {
    Err("build has no Nitro backend".into())
}

#[cfg(feature = "nitro")]
fn enroll_signing() -> Result<(), Box<dyn std::error::Error>> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use covenant_compute_key_broker::{KeyPolicy, encryption_context};
    use sha2::{Digest, Sha256, Sha384};
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct EnrollmentPolicy {
        protocol_version: u32,
        broker_id: String,
        coordinator_pubkey_b58: String,
        kms_key_arn: String,
        wrapped_key_base64: String,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct EnrollmentRequest {
        credentials: nitro::SessionCredentials,
        nonce_hex: String,
    }
    if EMBEDDED_POLICY.is_empty() || !Path::new("/dev/nsm").exists() {
        return Err("embedded enrollment policy and Nitro hardware required".into());
    }
    // This shape cannot parse as a service policy and contains no application
    // keys. Its temporary PCR authorization must permit signing purpose only.
    let policy: EnrollmentPolicy = serde_json::from_slice(EMBEDDED_POLICY)?;
    if policy.protocol_version != 1
        || policy.broker_id.is_empty()
        || policy.broker_id.len() > 128
        || !policy
            .broker_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err("invalid enrollment policy".into());
    }
    let authority: [u8; 32] = bs58::decode(&policy.coordinator_pubkey_b58)
        .into_vec()?
        .try_into()
        .map_err(|_| "invalid authority")?;
    if ed25519_dalek::VerifyingKey::from_bytes(&authority)?.is_weak() {
        return Err("weak authority".into());
    }
    let region = policy
        .kms_key_arn
        .split(':')
        .nth(3)
        .ok_or("invalid key ARN")?;
    let line = read_line(&mut io::stdin().lock())?.ok_or("request required")?;
    let request: EnrollmentRequest =
        serde_json::from_slice(&line).map_err(|_| "invalid enrollment request")?;
    let nonce: [u8; 32] = hex::decode(&request.nonce_hex)?
        .try_into()
        .map_err(|_| "invalid nonce")?;
    if nonce == [0; 32] || request.nonce_hex != hex::encode(nonce) {
        return Err("invalid nonce".into());
    }
    let context = encryption_context(
        &policy.broker_id,
        &KeyPolicy {
            tenant_id: policy.broker_id.clone(),
            key_id: "broker-response-signing".into(),
            key_version: 1,
            purpose: "broker-response-signing".into(),
            owner_pubkey_b58: policy.coordinator_pubkey_b58.clone(),
            workload: WorkloadPolicy::FixedQualificationHash {
                workload_sha256: "0".repeat(64),
            },
            kms_key_arn: policy.kms_key_arn.clone(),
            wrapped_key_base64: policy.wrapped_key_base64.clone(),
        },
    );
    let backend = nitro::NitroKms::new(region, request.credentials)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let seed = runtime.block_on(backend.decrypt(
        &policy.kms_key_arn,
        &policy.wrapped_key_base64,
        &context,
    ))?;
    let key = ed25519_dalek::SigningKey::from_bytes(&seed);
    let public = key.verifying_key().to_bytes();
    let blob = STANDARD.decode(&policy.wrapped_key_base64)?;
    let enrollment = serde_json::json!({
        "protocol_version":1, "broker_id":policy.broker_id,
        "coordinator_pubkey_b58":policy.coordinator_pubkey_b58, "kms_key_arn":policy.kms_key_arn,
        "wrapped_key_sha256":hex::encode(Sha256::digest(blob)),
        "public_key_b58":bs58::encode(public).into_string(), "nonce_hex":request.nonce_hex,
    });
    let mut binding = Sha384::new();
    binding.update(b"covenant.compute.key-broker.signing-enrollment.v1\0");
    binding.update(serde_jcs::to_vec(&enrollment)?);
    let document =
        nitro::attest_binding(public.to_vec(), nonce, Some(binding.finalize().to_vec()))?;
    let mut output = io::stdout().lock();
    serde_json::to_writer(
        &mut output,
        &serde_json::json!({
            "enrollment":enrollment,"attestation_document_base64":STANDARD.encode(document),
        }),
    )?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_recipient_pin_must_match_the_actual_enclave_signer() {
        let raw = include_str!("../../../../deploy/compute-key-broker/test-build-policy.json");
        let mut policy: DeploymentPolicy = serde_json::from_str(raw).unwrap();
        policy.validate().unwrap();
        policy.signing.public_key_b58 = policy.broker.coordinator_pubkey_b58.clone();
        assert!(policy.validate().is_err());
    }
    #[test]
    fn malformed_or_oversized_frames_are_fatal() {
        assert!(read_line(&mut &b"{}"[..]).is_err());
        assert!(read_line(&mut vec![b'x'; MAX_REQUEST_BYTES + 1].as_slice()).is_err());
        assert!(read_line(&mut &b""[..]).unwrap().is_none());
        let mut input = &b"{}\n{}\n"[..];
        assert_eq!(read_line(&mut input).unwrap().unwrap().as_slice(), b"{}\n");
        assert_eq!(read_line(&mut input).unwrap().unwrap().as_slice(), b"{}\n");
    }
}
