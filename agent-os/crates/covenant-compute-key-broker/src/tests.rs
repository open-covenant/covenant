//! Logic tests use a private, cfg(test)-only attestation stub. They do not
//! constitute Intel TDX, Nitro, AWS, or live lease acceptance evidence.

use std::sync::{Mutex, OnceLock};

use covenant_compute_protocol::{CapabilityRequirement, JobEnvelopePayload, JobKind, LeaseTerms};
use covenant_identity::LocalIdentity;
use ed25519_dalek::{Signer, SigningKey};
use rsa::RsaPrivateKey;

use super::*;

fn owner() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}

fn coordinator() -> SigningKey {
    SigningKey::from_bytes(&[11; 32])
}

fn pubkey(key: &SigningKey) -> String {
    bs58::encode(key.verifying_key().as_bytes()).into_string()
}

fn recipient() -> &'static RsaPrivateKey {
    static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
    KEY.get_or_init(|| RsaPrivateKey::new(&mut OsRng, 3072).unwrap())
}

fn recipient_encoded() -> String {
    STANDARD.encode(recipient().to_public_key().to_public_key_der().unwrap())
}

fn binding() -> KeyBinding {
    KeyBinding {
        tenant_id: "tenant-a".into(),
        key_id: "workspace-a".into(),
        key_version: 1,
        purpose: "workspace-snapshot".into(),
        owner_pubkey_b58: pubkey(&owner()),
        job_id: Uuid::from_u128(1),
        lease_id: "lease-a".into(),
        lease_generation: "phala-ephemeral-00000000-0000-0000-0000-000000000001-1".into(),
        operator_pubkey_b58: pubkey(&SigningKey::from_bytes(&[13; 32])),
        workload_sha256: "ab".repeat(32),
        phala_app_id: "ac".repeat(20),
        phala_instance_id: "ad".repeat(20),
    }
}

fn policy() -> BrokerPolicy {
    let b = binding();
    BrokerPolicy {
        broker_id: "covenant-broker-a".into(),
        coordinator_pubkey_b58: pubkey(&coordinator()),
        challenge_ttl_ms: 60_000,
        max_pending_challenges: 4,
        keys: vec![KeyPolicy {
            tenant_id: b.tenant_id,
            key_id: b.key_id,
            key_version: b.key_version,
            purpose: b.purpose,
            owner_pubkey_b58: b.owner_pubkey_b58,
            workload: WorkloadPolicy::FixedQualificationHash {
                workload_sha256: b.workload_sha256,
            },
            kms_key_arn:
                "arn:aws:kms:eu-west-1:123456789012:key/00000000-0000-4000-8000-000000000001".into(),
            wrapped_key_base64: STANDARD.encode(b"test-only-ciphertext"),
        }],
    }
}

fn challenge_request() -> ChallengeRequest {
    ChallengeRequest {
        binding: binding(),
        recipient_spki_der_base64: recipient_encoded(),
        signed_job_envelope: None,
    }
}

fn signed_request(challenge: BrokerChallenge) -> ReleaseRequest {
    let claims = LeaseAssertionClaims {
        challenge_sha256: challenge_digest(&challenge).unwrap(),
        binding: challenge.binding.clone(),
        state: LeaseState::Active,
        observed_at_ms: 1_800_000_000_000,
        expires_at_ms: 1_800_000_000_000 + MAX_ASSERTION_TTL_MS,
        lease_expires_at_ms: 1_800_000_600_000,
    };
    ReleaseRequest {
        buyer_authorization: BuyerAuthorization {
            owner_pubkey_b58: pubkey(&owner()),
            signature_b58: bs58::encode(
                owner()
                    .sign(&buyer_authorization_bytes(&challenge).unwrap())
                    .to_bytes(),
            )
            .into_string(),
        },
        lease_assertion: SignedLeaseAssertion {
            coordinator_pubkey_b58: pubkey(&coordinator()),
            signature_b58: bs58::encode(
                coordinator()
                    .sign(&lease_assertion_bytes(&claims).unwrap())
                    .to_bytes(),
            )
            .into_string(),
            claims,
        },
        challenge,
        evidence: TdxKeyEvidence {
            quote_base64: "".into(),
            collateral_json: "".into(),
            events: vec![],
        },
    }
}

fn resign_coordinator(request: &mut ReleaseRequest) {
    request.lease_assertion.signature_b58 = bs58::encode(
        coordinator()
            .sign(&lease_assertion_bytes(&request.lease_assertion.claims).unwrap())
            .to_bytes(),
    )
    .into_string();
}

/// Deliberately a test-only stub. Production `release` always invokes the real
/// compiled DCAP verifier; there is no public verifier injection interface.
fn mock_tdx_accept(_: &TdxKeyEvidence, _: &BrokerChallenge, _: u64) -> Result<()> {
    Ok(())
}

#[derive(Default)]
struct TestUnwrapper {
    contexts: Mutex<Vec<BTreeMap<String, String>>>,
    fail: bool,
}

#[async_trait]
impl KeyUnwrapper for TestUnwrapper {
    async fn unwrap_data_key(
        &self,
        _: &KeyPolicy,
        context: &BTreeMap<String, String>,
    ) -> Result<DataKey> {
        self.contexts.lock().unwrap().push(context.clone());
        if self.fail {
            return Err(BrokerError::UnwrapFailed);
        }
        Ok(Zeroizing::new([42; 32]))
    }
}

#[tokio::test]
async fn signed_mock_tdx_flow_reencrypts_real_rsa_with_exact_context() {
    let p = policy();
    let mut broker = Broker::new(p.clone()).unwrap();
    let request = signed_request(broker.issue_challenge(challenge_request()).unwrap());
    let challenge_hash = challenge_digest(&request.challenge).unwrap();
    let backend = TestUnwrapper::default();
    let response = broker
        .release_inner(request, &backend, mock_tdx_accept)
        .await
        .unwrap();
    let ciphertext = STANDARD.decode(&response.ciphertext_base64).unwrap();
    let plaintext = recipient()
        .decrypt(
            Oaep::new_with_label::<Sha256, _>(response_label(&challenge_hash)),
            &ciphertext,
        )
        .unwrap();
    assert_eq!(plaintext, [42; 32]);
    assert!(
        recipient()
            .decrypt(Oaep::new::<Sha256>(), &ciphertext)
            .is_err()
    );
    let contexts = backend.contexts.lock().unwrap();
    assert_eq!(
        contexts.as_slice(),
        &[encryption_context(&p.broker_id, &p.keys[0])]
    );
    assert_eq!(contexts[0].len(), 7);
    assert_eq!(contexts[0]["covenant:protocol"], "application-key-v1");
    assert_eq!(response.algorithm, "RSA_OAEP_SHA256");
    let wire = serde_json::to_value(response).unwrap();
    assert!(wire.get("plaintext").is_none());
    assert!(wire.get("data_key").is_none());
}

#[tokio::test]
async fn release_challenge_cannot_be_replayed() {
    let mut broker = Broker::new(policy()).unwrap();
    let request = signed_request(broker.issue_challenge(challenge_request()).unwrap());
    let backend = TestUnwrapper::default();
    broker
        .release_inner(request.clone(), &backend, mock_tdx_accept)
        .await
        .unwrap();
    assert_eq!(
        broker
            .release_inner(request, &backend, mock_tdx_accept)
            .await,
        Err(BrokerError::ChallengeRejected)
    );
    assert_eq!(backend.contexts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn ambiguous_unwrap_failure_consumes_challenge_permanently() {
    let mut broker = Broker::new(policy()).unwrap();
    let request = signed_request(broker.issue_challenge(challenge_request()).unwrap());
    let backend = TestUnwrapper {
        fail: true,
        ..Default::default()
    };
    assert_eq!(
        broker
            .release_inner(request.clone(), &backend, mock_tdx_accept)
            .await,
        Err(BrokerError::UnwrapFailed)
    );
    assert_eq!(
        broker
            .release_inner(request, &backend, mock_tdx_accept)
            .await,
        Err(BrokerError::ChallengeRejected)
    );
    assert_eq!(backend.contexts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn process_reset_invalidates_all_issued_challenges() {
    let mut first = Broker::new(policy()).unwrap();
    let request = signed_request(first.issue_challenge(challenge_request()).unwrap());
    let mut restarted = Broker::new(policy()).unwrap();
    assert_eq!(
        restarted
            .release_inner(request, &TestUnwrapper::default(), mock_tdx_accept)
            .await,
        Err(BrokerError::ChallengeRejected)
    );
}

#[tokio::test]
async fn real_production_verifier_rejects_mock_hardware_before_unwrap() {
    let mut broker = Broker::new(policy()).unwrap();
    let request = signed_request(broker.issue_challenge(challenge_request()).unwrap());
    let backend = TestUnwrapper::default();
    assert_eq!(
        broker.release(request, &backend).await,
        Err(BrokerError::AttestationRejected)
    );
    assert!(backend.contexts.lock().unwrap().is_empty());
}

#[test]
fn key_policy_rejects_wrong_tenant_owner_version_purpose_and_workload() {
    for field in 0..6 {
        let mut broker = Broker::new(policy()).unwrap();
        let mut request = challenge_request();
        match field {
            0 => request.binding.tenant_id = "tenant-b".into(),
            1 => request.binding.owner_pubkey_b58 = pubkey(&coordinator()),
            2 => request.binding.key_version = 2,
            3 => request.binding.purpose = "other-purpose".into(),
            4 => request.binding.workload_sha256 = "ff".repeat(32),
            _ => request.binding.key_id = "other-key".into(),
        }
        assert_eq!(
            broker.issue_challenge(request),
            Err(BrokerError::BindingRejected)
        );
    }
}

#[tokio::test]
async fn modified_challenge_fields_fail_before_backend() {
    for field in 0..7 {
        let mut broker = Broker::new(policy()).unwrap();
        let challenge = broker.issue_challenge(challenge_request()).unwrap();
        let mut request = signed_request(challenge);
        match field {
            0 => request.challenge.binding.lease_id = "other-lease".into(),
            1 => request.challenge.binding.lease_generation = "generation-2".into(),
            2 => request.challenge.binding.phala_app_id = "00".repeat(20),
            3 => request.challenge.binding.phala_instance_id = "00".repeat(20),
            4 => request.challenge.broker_id = "other-broker".into(),
            5 => request.challenge.recipient_spki_der_base64.push('A'),
            _ => request.challenge.nonce_hex = "00".repeat(32),
        }
        let backend = TestUnwrapper::default();
        assert_eq!(
            broker
                .release_inner(request, &backend, mock_tdx_accept)
                .await,
            Err(BrokerError::ChallengeRejected)
        );
        assert!(backend.contexts.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn buyer_signature_and_signer_are_both_required() {
    for wrong_signer in [false, true] {
        let mut broker = Broker::new(policy()).unwrap();
        let mut request = signed_request(broker.issue_challenge(challenge_request()).unwrap());
        if wrong_signer {
            request.buyer_authorization.owner_pubkey_b58 = pubkey(&coordinator());
        } else {
            request.buyer_authorization.signature_b58 = bs58::encode([0; 64]).into_string();
        }
        let backend = TestUnwrapper::default();
        assert_eq!(
            broker
                .release_inner(request, &backend, mock_tdx_accept)
                .await,
            Err(BrokerError::OwnerAuthorizationRejected)
        );
        assert!(backend.contexts.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn coordinator_signature_and_pinned_signer_are_both_required() {
    for wrong_signer in [false, true] {
        let mut broker = Broker::new(policy()).unwrap();
        let mut request = signed_request(broker.issue_challenge(challenge_request()).unwrap());
        if wrong_signer {
            request.lease_assertion.coordinator_pubkey_b58 = pubkey(&owner());
        } else {
            request.lease_assertion.signature_b58 = bs58::encode([0; 64]).into_string();
        }
        let backend = TestUnwrapper::default();
        assert_eq!(
            broker
                .release_inner(request, &backend, mock_tdx_accept)
                .await,
            Err(BrokerError::LeaseAuthorizationRejected)
        );
        assert!(backend.contexts.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn valid_coordinator_signature_cannot_authorize_different_lease_or_challenge() {
    for field in 0..5 {
        let mut broker = Broker::new(policy()).unwrap();
        let mut request = signed_request(broker.issue_challenge(challenge_request()).unwrap());
        match field {
            0 => request.lease_assertion.claims.binding.lease_id = "other-lease".into(),
            1 => request.lease_assertion.claims.binding.tenant_id = "other-tenant".into(),
            2 => request.lease_assertion.claims.binding.operator_pubkey_b58 = pubkey(&owner()),
            3 => request.lease_assertion.claims.binding.job_id = Uuid::from_u128(2),
            _ => request.lease_assertion.claims.challenge_sha256 = "00".repeat(32),
        }
        resign_coordinator(&mut request);
        let backend = TestUnwrapper::default();
        assert_eq!(
            broker
                .release_inner(request, &backend, mock_tdx_accept)
                .await,
            Err(BrokerError::LeaseAuthorizationRejected)
        );
        assert!(backend.contexts.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn assertion_expiry_and_maximum_lifetime_are_enforced() {
    for case in 0..4 {
        let mut broker = Broker::new(policy()).unwrap();
        let mut request = signed_request(broker.issue_challenge(challenge_request()).unwrap());
        let claims = &mut request.lease_assertion.claims;
        match case {
            0 => claims.expires_at_ms = claims.observed_at_ms,
            1 => claims.expires_at_ms = claims.observed_at_ms + MAX_ASSERTION_TTL_MS + 1,
            2 => claims.lease_expires_at_ms = claims.observed_at_ms,
            _ => claims.observed_at_ms = 0,
        }
        resign_coordinator(&mut request);
        assert_eq!(
            broker
                .release_inner(request, &TestUnwrapper::default(), mock_tdx_accept)
                .await,
            Err(BrokerError::LeaseAuthorizationRejected)
        );
    }
}

#[tokio::test]
async fn monotonic_challenge_and_assertion_deadlines_do_not_use_wall_clock() {
    for challenge_expired in [false, true] {
        let mut broker = Broker::new(policy()).unwrap();
        let request = signed_request(broker.issue_challenge(challenge_request()).unwrap());
        broker
            .pending
            .get_mut(&request.challenge.challenge_id)
            .unwrap()
            .issued = Instant::now() - Duration::from_secs(if challenge_expired { 61 } else { 31 });
        let expected = if challenge_expired {
            BrokerError::ChallengeRejected
        } else {
            BrokerError::LeaseAuthorizationRejected
        };
        assert_eq!(
            broker
                .release_inner(request, &TestUnwrapper::default(), mock_tdx_accept)
                .await,
            Err(expected)
        );
    }
}

#[test]
fn full_pending_capacity_is_bounded_and_expired_entries_are_pruned() {
    let mut p = policy();
    p.max_pending_challenges = 1;
    let mut broker = Broker::new(p).unwrap();
    let first = broker.issue_challenge(challenge_request()).unwrap();
    assert_eq!(
        broker.issue_challenge(challenge_request()),
        Err(BrokerError::Capacity)
    );
    broker.pending.get_mut(&first.challenge_id).unwrap().issued =
        Instant::now() - Duration::from_secs(61);
    let second = broker.issue_challenge(challenge_request()).unwrap();
    assert_ne!(first.challenge_id, second.challenge_id);
    assert_ne!(first.nonce_hex, second.nonce_hex);
    assert_eq!(broker.pending.len(), 1);
}

#[test]
fn malformed_weak_and_noncanonical_recipients_are_rejected() {
    assert_eq!(
        decode_recipient("not-base64").unwrap_err(),
        BrokerError::RecipientRejected
    );
    let small = RsaPrivateKey::new(&mut OsRng, 1024).unwrap();
    let encoded = STANDARD.encode(small.to_public_key().to_public_key_der().unwrap());
    assert_eq!(
        decode_recipient(&encoded).unwrap_err(),
        BrokerError::RecipientRejected
    );
    assert_eq!(
        decode_recipient(&(recipient_encoded() + "\n")).unwrap_err(),
        BrokerError::RecipientRejected
    );
    let wrong_exponent =
        RsaPublicKey::new(recipient().n().clone(), rsa::BigUint::from(3u32)).unwrap();
    let encoded = STANDARD.encode(wrong_exponent.to_public_key_der().unwrap());
    assert_eq!(
        decode_recipient(&encoded).unwrap_err(),
        BrokerError::RecipientRejected
    );
}

#[test]
fn reports_and_authorizations_have_separate_domains_and_bind_recipient() {
    let mut broker = Broker::new(policy()).unwrap();
    let challenge = broker.issue_challenge(challenge_request()).unwrap();
    let report = report_data(&challenge).unwrap();
    let owner_bytes = buyer_authorization_bytes(&challenge).unwrap();
    assert_ne!(report.as_slice(), Sha512::digest(owner_bytes).as_slice());
    let mut changed = challenge.clone();
    changed.recipient_spki_der_base64.push('a');
    assert_ne!(report, report_data(&changed).unwrap());
    assert_ne!(
        challenge_digest(&challenge).unwrap(),
        challenge_digest(&changed).unwrap()
    );
}

#[test]
fn invalid_policy_duplicate_keys_and_unknown_wire_fields_fail_closed() {
    for case in 0..7 {
        let mut p = policy();
        match case {
            0 => p.keys.push(p.keys[0].clone()),
            1 => p.coordinator_pubkey_b58 = bs58::encode([0; 32]).into_string(),
            2 => p.challenge_ttl_ms = MAX_CHALLENGE_TTL_MS + 1,
            3 => p.keys[0].wrapped_key_base64 = "".into(),
            4 => p.max_pending_challenges = 4097,
            5 => p.keys[0].key_id = "broker-response-signing".into(),
            _ => p.keys[0].purpose = "broker-response-signing".into(),
        }
        assert!(matches!(Broker::new(p), Err(BrokerError::InvalidPolicy)));
    }
    let mut wire = serde_json::to_value(challenge_request()).unwrap();
    wire["verified"] = serde_json::Value::Bool(true);
    assert!(serde_json::from_value::<ChallengeRequest>(wire).is_err());
}

#[tokio::test]
async fn oversized_evidence_consumes_challenge_without_backend_call() {
    let mut broker = Broker::new(policy()).unwrap();
    let mut request = signed_request(broker.issue_challenge(challenge_request()).unwrap());
    request.evidence.collateral_json = "x".repeat(MAX_REQUEST_BYTES);
    let backend = TestUnwrapper::default();
    assert_eq!(
        broker
            .release_inner(request.clone(), &backend, mock_tdx_accept)
            .await,
        Err(BrokerError::InvalidRequest)
    );
    assert_eq!(
        broker
            .release_inner(request, &backend, mock_tdx_accept)
            .await,
        Err(BrokerError::ChallengeRejected)
    );
    assert!(backend.contexts.lock().unwrap().is_empty());
}

#[test]
fn python_and_rust_share_the_exact_challenge_canonicalization_vector() {
    let vector: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../deploy/compute-key-broker/guest/challenge-vector.json"
    ))
    .unwrap();
    let challenge: BrokerChallenge = serde_json::from_value(vector["challenge"].clone()).unwrap();
    assert_eq!(
        String::from_utf8(canonical(&challenge).unwrap()).unwrap(),
        vector["canonical_json"].as_str().unwrap()
    );
    assert_eq!(
        challenge_digest(&challenge).unwrap(),
        vector["challenge_sha256"].as_str().unwrap()
    );
    assert_eq!(
        hex::encode(report_data(&challenge).unwrap()),
        vector["report_data_hex"].as_str().unwrap()
    );
}

#[tokio::test]
async fn signed_response_rejects_host_injected_ciphertext_and_wrong_broker_key() {
    let mut broker = Broker::new(policy()).unwrap();
    let challenge = broker.issue_challenge(challenge_request()).unwrap();
    let response = broker
        .release_inner(
            signed_request(challenge.clone()),
            &TestUnwrapper::default(),
            mock_tdx_accept,
        )
        .await
        .unwrap();
    let signing = SigningKey::from_bytes(&[43; 32]);
    let mut signed = SignedReleaseResponse {
        broker_pubkey_b58: pubkey(&signing),
        signature_b58: bs58::encode(
            signing
                .sign(&release_response_bytes(&response).unwrap())
                .to_bytes(),
        )
        .into_string(),
        response,
    };
    assert!(verify_signed_response(&pubkey(&signing), &challenge, &signed).is_ok());
    assert_eq!(
        verify_signed_response(&pubkey(&owner()), &challenge, &signed),
        Err(BrokerError::ResponseAuthenticationRejected)
    );
    let attacker_ciphertext = recipient()
        .to_public_key()
        .encrypt(
            &mut OsRng,
            Oaep::new_with_label::<Sha256, _>(response_label(&signed.response.challenge_sha256)),
            &[99; 32],
        )
        .unwrap();
    signed.response.ciphertext_base64 = STANDARD.encode(attacker_ciphertext);
    assert_eq!(
        verify_signed_response(&pubkey(&signing), &challenge, &signed),
        Err(BrokerError::ResponseAuthenticationRejected)
    );
}

fn native_fixture(buyer: &LocalIdentity, id: Uuid) -> (BrokerPolicy, ChallengeRequest) {
    let mut p = policy();
    p.keys[0].owner_pubkey_b58 = buyer.agent_id().pubkey_base58();
    p.keys[0].workload = WorkloadPolicy::NativeTemplate {
        guest_image_digest: format!("sha256:{}", "cd".repeat(32)),
        recipient_image_digest: format!("sha256:{}", "ef".repeat(32)),
        broker_response_pubkey_b58: pubkey(&coordinator()),
    };
    let mut ssh = vec![0, 0, 0, 11];
    ssh.extend_from_slice(b"ssh-ed25519");
    ssh.extend_from_slice(&[0, 0, 0, 32]);
    ssh.extend_from_slice(&buyer.pubkey_bytes());
    let mut job = JobEnvelopePayload {
        job_id: id,
        buyer: buyer.agent_id(),
        kind: JobKind::LeaseSession,
        capability_requirement: CapabilityRequirement {
            inference_tariff: None,
            gpu_class: Some("cpu".into()),
            min_vram_gb: None,
            model_id: Some(phala::PHALA_EPHEMERAL_CPU_MODEL.into()),
            kind: JobKind::LeaseSession,
            max_duration_secs: 600,
            min_reputation_bps: None,
        },
        input: vec![
            covenant_compute_protocol::lease_input(LeaseTerms {
                max_duration_secs: 600,
                rate_micro_usdc_per_sec: 1,
                client_public_key: Some(format!("ssh-ed25519 {}", STANDARD.encode(ssh))),
            })
            .unwrap(),
        ],
        price_micro_usdc: 600,
        deadline_ms: 600_000 + phala::PHALA_EPHEMERAL_RESULT_SLACK_MS,
        idempotency: covenant_a2a::A2AIdempotency::new(
            covenant_a2a::A2ADuplicateSafety::Idempotent,
            "key-broker-native-test",
        ),
        issued_at_ms: 1_800_000_000_000,
        referral_code: None,
        stream: false,
    };
    let native = match &p.keys[0].workload {
        WorkloadPolicy::NativeTemplate {
            guest_image_digest,
            recipient_image_digest,
            broker_response_pubkey_b58,
        } => (
            guest_image_digest,
            recipient_image_digest,
            broker_response_pubkey_b58,
        ),
        _ => unreachable!(),
    };
    let recipient_config = phala::ApplicationKeyRecipient {
        protocol_version: 1,
        broker_id: p.broker_id.clone(),
        broker_response_pubkey_b58: native.2.clone(),
        tenant_id: p.keys[0].tenant_id.clone(),
        key_id: p.keys[0].key_id.clone(),
        key_version: p.keys[0].key_version,
        purpose: p.keys[0].purpose.clone(),
        recipient_image_digest: native.1.clone(),
    };
    let operator = binding().operator_pubkey_b58;
    let (_, spec) = phala::phala_ephemeral_workload_with_recipient(
        &job,
        &operator,
        &phala::reviewed_phala_ephemeral_wrapper(),
        native.0,
        &phala::PhalaRelayConfig {
            url: "https://relay.example.com".into(),
            session_id: id.to_string(),
        },
        Some(&recipient_config),
    )
    .unwrap();
    job.input
        .push(phala::phala_ephemeral_lease_input(spec.clone()).unwrap());
    let envelope = SignedJobEnvelope::sign(job, buyer).unwrap();
    let mut b = binding();
    b.owner_pubkey_b58 = buyer.agent_id().pubkey_base58();
    b.job_id = id;
    b.lease_id = id.to_string();
    b.lease_generation = spec.lease_generation;
    b.workload_sha256 = spec.workload_sha256;
    (
        p,
        ChallengeRequest {
            binding: b,
            recipient_spki_der_base64: recipient_encoded(),
            signed_job_envelope: Some(envelope),
        },
    )
}

#[test]
fn native_policy_reuses_one_key_across_independently_reconstructed_lease_manifests() {
    let buyer = LocalIdentity::generate("buyer@native-key-policy-test");
    let (policy, first) = native_fixture(&buyer, Uuid::new_v4());
    let (_, second) = native_fixture(&buyer, Uuid::new_v4());
    assert_ne!(
        first.binding.workload_sha256,
        second.binding.workload_sha256
    );
    let mut broker = Broker::new(policy).unwrap();
    assert!(broker.issue_challenge(first).is_ok());
    assert!(broker.issue_challenge(second).is_ok());
}

#[test]
fn native_policy_requires_authentic_buyer_envelope_and_exact_manifest_pin() {
    let buyer = LocalIdentity::generate("buyer@native-key-policy-test");
    for attack in 0..7 {
        let (mut policy, mut request) = native_fixture(&buyer, Uuid::new_v4());
        match attack {
            0 => request.signed_job_envelope = None,
            1 => {
                request.signed_job_envelope.as_mut().unwrap().signature_b58 =
                    bs58::encode([0; 64]).into_string()
            }
            2 => request.binding.workload_sha256 = "00".repeat(32),
            3 => request.binding.lease_generation.push_str("-replacement"),
            4 => request.binding.lease_id = Uuid::new_v4().to_string(),
            5 => {
                if let WorkloadPolicy::NativeTemplate {
                    guest_image_digest, ..
                } = &mut policy.keys[0].workload
                {
                    *guest_image_digest = format!("sha256:{}", "12".repeat(32));
                }
            }
            _ => {
                if let WorkloadPolicy::NativeTemplate {
                    recipient_image_digest,
                    ..
                } = &mut policy.keys[0].workload
                {
                    *recipient_image_digest = format!("sha256:{}", "34".repeat(32));
                }
            }
        }
        let mut broker = Broker::new(policy).unwrap();
        assert_eq!(
            broker.issue_challenge(request),
            Err(BrokerError::BindingRejected),
            "attack {attack}"
        );
    }
}

#[test]
fn native_recipient_policy_must_match_broker_key_scope_and_response_identity() {
    let buyer = LocalIdentity::generate("buyer@native-key-policy-test");
    for attack in 0..6 {
        let (mut policy, mut request) = native_fixture(&buyer, Uuid::new_v4());
        match attack {
            0 => {
                policy.keys[0].tenant_id = "other-tenant".into();
                request.binding.tenant_id = "other-tenant".into();
            }
            1 => {
                policy.keys[0].key_id = "other-key".into();
                request.binding.key_id = "other-key".into();
            }
            2 => {
                policy.keys[0].key_version = 2;
                request.binding.key_version = 2;
            }
            3 => {
                policy.keys[0].purpose = "other-purpose".into();
                request.binding.purpose = "other-purpose".into();
            }
            4 => policy.broker_id = "other-broker".into(),
            _ => {
                if let WorkloadPolicy::NativeTemplate {
                    broker_response_pubkey_b58,
                    ..
                } = &mut policy.keys[0].workload
                {
                    *broker_response_pubkey_b58 = pubkey(&owner());
                }
            }
        }
        let mut broker = Broker::new(policy).unwrap();
        assert_eq!(
            broker.issue_challenge(request),
            Err(BrokerError::BindingRejected),
            "attack {attack}"
        );
    }
}

#[tokio::test]
async fn signing_initialization_cannot_extend_authorized_release_or_trigger_late_unwrap() {
    let mut broker = Broker::new(policy()).unwrap();
    let request = signed_request(broker.issue_challenge(challenge_request()).unwrap());
    let mut authorized = broker
        .authorize_inner(request.clone(), mock_tdx_accept)
        .unwrap();
    // Model signing-key initialization taking the remaining assertion lifetime.
    authorized.pending.issued = Instant::now() - Duration::from_secs(31);
    let backend = TestUnwrapper::default();
    assert_eq!(
        authorized.release(&backend).await,
        Err(BrokerError::LeaseAuthorizationRejected)
    );
    assert!(backend.contexts.lock().unwrap().is_empty());
    assert_eq!(
        broker
            .release_inner(request, &backend, mock_tdx_accept)
            .await,
        Err(BrokerError::ChallengeRejected)
    );
}

#[tokio::test]
async fn backend_completing_after_expiry_returns_no_recipient_ciphertext() {
    struct DelayedUnwrapper(TestUnwrapper);
    #[async_trait]
    impl KeyUnwrapper for DelayedUnwrapper {
        async fn unwrap_data_key(
            &self,
            key: &KeyPolicy,
            context: &BTreeMap<String, String>,
        ) -> Result<DataKey> {
            let key = self.0.unwrap_data_key(key, context).await?;
            tokio::time::sleep(Duration::from_millis(1100)).await;
            Ok(key)
        }
    }
    let mut broker = Broker::new(policy()).unwrap();
    let request = signed_request(broker.issue_challenge(challenge_request()).unwrap());
    let mut authorized = broker.authorize_inner(request, mock_tdx_accept).unwrap();
    authorized.pending.issued = Instant::now() - Duration::from_millis(MAX_ASSERTION_TTL_MS - 1000);
    let backend = DelayedUnwrapper(TestUnwrapper::default());
    assert_eq!(
        authorized.release(&backend).await,
        Err(BrokerError::LeaseAuthorizationRejected)
    );
    assert_eq!(backend.0.contexts.lock().unwrap().len(), 1);
}
