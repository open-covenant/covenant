//! Explicit buyer authorization for application-key delivery. This module never
//! creates a lease, funds a hold, refreshes an envelope, or retries a request.
use crate::{
    hardware::{scope_for_phala_lease, HardwareClient},
    BuyerError,
};
use covenant_compute_attestation::binding::{HardwareProfile, PhalaEphemeralIdentity};
use covenant_compute_key_broker::{
    buyer_authorization_bytes, verify_lease_authorization, verify_owner_authorization,
    BrokerChallenge, BuyerAuthorization, ChallengeRequest, KeyBinding, LeaseAssertionRequest,
    SignedLeaseAssertion,
};
use covenant_compute_protocol::{
    phala::parse_phala_ephemeral_lease_spec, LeaseView, SignedJobEnvelope,
};
use covenant_identity::LocalIdentity;

const MAX_ASSERTION_REQUEST_BYTES: usize = 16 * 1024;

fn invalid(message: impl ToString) -> BuyerError {
    BuyerError::Protocol(message.to_string())
}

/// Derive the request from the original purchase and live lease. `identity`
/// must come from locally re-verified hardware evidence, and `recipient_spki`
/// from that verified guest's isolated recipient process, never broker input.
pub fn challenge_request_for_lease(
    envelope: &SignedJobEnvelope,
    view: &LeaseView,
    identity: &PhalaEphemeralIdentity,
    recipient_spki: &str,
    expected_buyer: &str,
    now: u64,
) -> Result<ChallengeRequest, BuyerError> {
    let scope = scope_for_phala_lease(envelope, view, expected_buyer, now)?;
    if scope.profile != HardwareProfile::PhalaEphemeralDstack061 {
        return Err(invalid(
            "application-key delivery requires an ephemeral Phala lease",
        ));
    }
    let spec = parse_phala_ephemeral_lease_spec(&envelope.payload.input)
        .map_err(invalid)?
        .ok_or_else(|| invalid("signed ephemeral lease missing"))?;
    let recipient = spec
        .application_key_recipient
        .as_ref()
        .ok_or_else(|| invalid("original lease has no application-key recipient authorization"))?;
    recipient.validate().map_err(invalid)?;
    if recipient_spki.is_empty() || recipient_spki.len() > 1024 {
        return Err(invalid("recipient public key exceeds its bound"));
    }
    Ok(ChallengeRequest {
        binding: KeyBinding {
            tenant_id: recipient.tenant_id.clone(),
            key_id: recipient.key_id.clone(),
            key_version: recipient.key_version,
            purpose: recipient.purpose.clone(),
            owner_pubkey_b58: expected_buyer.into(),
            job_id: envelope.payload.job_id,
            lease_id: envelope.payload.job_id.to_string(),
            lease_generation: spec.lease_generation,
            operator_pubkey_b58: spec.operator_pubkey_b58,
            workload_sha256: spec.workload_sha256,
            phala_app_id: identity.app_id.clone(),
            phala_instance_id: identity.instance_id.clone(),
        },
        recipient_spki_der_base64: recipient_spki.into(),
        signed_job_envelope: Some(envelope.clone()),
    })
}

fn validate_challenge_scope(
    expected: &ChallengeRequest,
    challenge: &BrokerChallenge,
    expected_buyer: &str,
    now: u64,
) -> Result<u64, BuyerError> {
    if challenge.binding != expected.binding
        || challenge.recipient_spki_der_base64 != expected.recipient_spki_der_base64
        || challenge.binding.owner_pubkey_b58 != expected_buyer
    {
        return Err(invalid(
            "broker challenge differs from the retained guest request",
        ));
    }
    let envelope = expected
        .signed_job_envelope
        .as_ref()
        .ok_or_else(|| invalid("original buyer-signed envelope is required"))?;
    envelope.verify().map_err(invalid)?;
    envelope.payload.validate_input().map_err(invalid)?;
    let spec = parse_phala_ephemeral_lease_spec(&envelope.payload.input)
        .map_err(invalid)?
        .ok_or_else(|| invalid("signed ephemeral lease missing"))?;
    spec.validate_for_job(&envelope.payload).map_err(invalid)?;
    let recipient = spec
        .application_key_recipient
        .as_ref()
        .ok_or_else(|| invalid("original lease did not authorize application custody"))?;
    let binding = &challenge.binding;
    if envelope.payload.buyer.pubkey_base58() != expected_buyer
        || envelope.payload.job_id != binding.job_id
        || binding.lease_id != binding.job_id.to_string()
        || recipient.broker_id != challenge.broker_id
        || recipient.tenant_id != binding.tenant_id
        || recipient.key_id != binding.key_id
        || recipient.key_version != binding.key_version
        || recipient.purpose != binding.purpose
        || spec.operator_pubkey_b58 != binding.operator_pubkey_b58
        || spec.lease_generation != binding.lease_generation
        || spec.workload_sha256 != binding.workload_sha256
        || now < envelope.payload.issued_at_ms
        || now >= spec.lease_expires_at_ms
    {
        return Err(invalid(
            "broker challenge is outside the signed live application-key lease",
        ));
    }
    // Bound before using the buyer's signing key or allocating canonical bytes.
    if challenge.recipient_spki_der_base64.len() > 1024
        || challenge.broker_id.len() > 128
        || challenge.nonce_hex.len() > 64
        || binding.phala_app_id.len() != 40
        || binding.phala_instance_id.len() != 40
    {
        return Err(invalid("broker challenge exceeds its bound"));
    }
    Ok(spec.lease_expires_at_ms)
}

/// Sign only the exact challenge for the retained, independently constructed
/// request. No authorization is returned until the core wire validation passes.
pub fn authorize_challenge(
    buyer: &LocalIdentity,
    expected: &ChallengeRequest,
    challenge: &BrokerChallenge,
    now: u64,
) -> Result<BuyerAuthorization, BuyerError> {
    let owner = buyer.agent_id().pubkey_base58();
    validate_challenge_scope(expected, challenge, &owner, now)?;
    let authorization = BuyerAuthorization {
        owner_pubkey_b58: owner,
        signature_b58: bs58::encode(
            buyer
                .sign(&buyer_authorization_bytes(challenge).map_err(invalid)?)
                .to_bytes(),
        )
        .into_string(),
    };
    verify_owner_authorization(challenge, &authorization).map_err(invalid)?;
    Ok(authorization)
}

pub struct KeyBrokerClient<'a> {
    transport: HardwareClient<'a>,
    buyer: &'a LocalIdentity,
    coordinator_pubkey_b58: String,
}

impl<'a> KeyBrokerClient<'a> {
    /// The coordinator key must be pinned from the verified deployment identity,
    /// never copied from the assertion currently being checked.
    pub fn new(
        coordinator_url: &str,
        buyer: &'a LocalIdentity,
        coordinator_pubkey_b58: &str,
    ) -> Result<Self, BuyerError> {
        covenant_compute_protocol::validate_address_b58(
            "key-broker coordinator",
            coordinator_pubkey_b58,
        )
        .map_err(invalid)?;
        Ok(Self {
            transport: HardwareClient::new(coordinator_url, buyer)?,
            buyer,
            coordinator_pubkey_b58: coordinator_pubkey_b58.into(),
        })
    }

    pub async fn request_lease_assertion(
        &self,
        expected: &ChallengeRequest,
        challenge: &BrokerChallenge,
    ) -> Result<(BuyerAuthorization, SignedLeaseAssertion), BuyerError> {
        let now = crate::epoch_ms();
        let authorization = authorize_challenge(self.buyer, expected, challenge, now)?;
        let lease_expires = validate_challenge_scope(
            expected,
            challenge,
            &self.buyer.agent_id().pubkey_base58(),
            now,
        )?;
        let request = LeaseAssertionRequest {
            challenge: challenge.clone(),
            buyer_authorization: authorization.clone(),
        };
        let body = serde_json::to_vec(&request).map_err(invalid)?;
        if body.len() > MAX_ASSERTION_REQUEST_BYTES {
            return Err(invalid("lease assertion request exceeds limit"));
        }
        let assertion: SignedLeaseAssertion = self
            .transport
            .request(
                reqwest::Method::POST,
                &format!(
                    "/federation/key-broker/{}/lease-assertion",
                    challenge.binding.job_id
                ),
                body,
            )
            .await?;
        verify_received_assertion(
            challenge,
            &assertion,
            &self.coordinator_pubkey_b58,
            lease_expires,
            crate::epoch_ms(),
        )?;
        Ok((authorization, assertion))
    }
}

fn verify_received_assertion(
    challenge: &BrokerChallenge,
    assertion: &SignedLeaseAssertion,
    coordinator: &str,
    lease_expires: u64,
    now: u64,
) -> Result<(), BuyerError> {
    verify_lease_authorization(challenge, assertion, coordinator).map_err(invalid)?;
    if assertion.claims.observed_at_ms > now
        || assertion.claims.expires_at_ms <= now
        || assertion.claims.lease_expires_at_ms != lease_expires
        || now >= lease_expires
    {
        return Err(invalid(
            "coordinator assertion is stale, future-dated or outside the original lease",
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use covenant_compute_key_broker::{
        challenge_digest, lease_assertion_bytes, LeaseAssertionClaims, LeaseState,
    };
    use covenant_compute_protocol::{
        phala::{ApplicationKeyRecipient, PhalaRelayConfig},
        ssh::{SshAccess, SshHostKey, SshTransport},
        LeaseAccess,
    };
    use uuid::Uuid;

    pub(crate) struct NativeFixture {
        pub buyer: LocalIdentity,
        pub broker: LocalIdentity,
        pub envelope: SignedJobEnvelope,
        pub view: LeaseView,
        pub identity: PhalaEphemeralIdentity,
        pub expected: ChallengeRequest,
        pub challenge: BrokerChallenge,
    }

    pub(crate) fn native_fixture(now: u64) -> NativeFixture {
        let buyer = LocalIdentity::generate("key-test@buyer");
        let operator = LocalIdentity::generate("key-test@operator");
        let broker = LocalIdentity::generate("key-test@broker");
        let job_id = Uuid::new_v4();
        let host_key = SshHostKey {
            algorithm: "ssh-ed25519".into(),
            key_base64: STANDARD
                .encode([b"\0\0\0\x0bssh-ed25519\0\0\0\x20".as_slice(), &[7; 32]].concat()),
        };
        let args = crate::phala::PreparePhala {
            job_id,
            operator_pubkey_b58: operator.agent_id().pubkey_base58(),
            image_digest: format!("sha256:{}", "ab".repeat(32)),
            duration_secs: 600,
            rate_micro_usdc_per_sec: 1,
            buyer_ssh_public_key: format!("{} {}", host_key.algorithm, host_key.key_base64),
        };
        let recipient = ApplicationKeyRecipient {
            protocol_version: 1,
            broker_id: "covenant-broker-test".into(),
            broker_response_pubkey_b58: broker.agent_id().pubkey_base58(),
            tenant_id: "tenant-test".into(),
            key_id: "workspace-test".into(),
            key_version: 1,
            purpose: "workspace".into(),
            recipient_image_digest: format!("sha256:{}", "cd".repeat(32)),
        };
        let relay = PhalaRelayConfig {
            url: "https://relay.example.com".into(),
            session_id: job_id.to_string(),
        };
        let prepared = crate::phala::prepare_profile_with_recipient_at(
            &buyer,
            &args,
            Some(&relay),
            Some(&recipient),
            600,
            now,
        )
        .unwrap();
        let spec = parse_phala_ephemeral_lease_spec(&prepared.envelope.payload.input)
            .unwrap()
            .unwrap();
        let view = LeaseView {
            job_id,
            status: "accepted".into(),
            close_requested: false,
            rate_micro_usdc_per_sec: 1,
            max_duration_secs: 600,
            elapsed_ms: 0,
            charged_micro_usdc: 0,
            access: Some(LeaseAccess {
                job_id,
                endpoint: "description is not authenticated".into(),
                ready_at_ms: now,
                note: None,
                source_price: None,
                ssh: Some(SshAccess {
                    host: "relay.example.com".into(),
                    port: 443,
                    username: "covenant".into(),
                    lease_generation: spec.lease_generation,
                    transport: Some(SshTransport::OutboundRelay),
                    observed_host_keys: vec![host_key],
                }),
            }),
        };
        let identity = PhalaEphemeralIdentity {
            app_id: "ce".repeat(20),
            instance_id: "ef".repeat(20),
        };
        let vector: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../deploy/compute-key-broker/guest/challenge-vector.json"
        ))
        .unwrap();
        let spki = vector["challenge"]["recipient_spki_der_base64"]
            .as_str()
            .unwrap();
        let expected = challenge_request_for_lease(
            &prepared.envelope,
            &view,
            &identity,
            spki,
            &buyer.agent_id().pubkey_base58(),
            now,
        )
        .unwrap();
        let challenge = BrokerChallenge {
            protocol_version: 1,
            broker_id: recipient.broker_id,
            challenge_id: Uuid::new_v4(),
            nonce_hex: "01".repeat(32),
            ttl_ms: 60_000,
            binding: expected.binding.clone(),
            recipient_spki_der_base64: spki.into(),
        };
        NativeFixture {
            buyer,
            broker,
            envelope: prepared.envelope,
            view,
            identity,
            expected,
            challenge,
        }
    }

    pub(crate) fn signed_assertion(
        f: &NativeFixture,
        coordinator: &LocalIdentity,
        now: u64,
    ) -> SignedLeaseAssertion {
        let claims = LeaseAssertionClaims {
            challenge_sha256: challenge_digest(&f.challenge).unwrap(),
            binding: f.challenge.binding.clone(),
            state: LeaseState::Active,
            observed_at_ms: now,
            expires_at_ms: now + 30_000,
            lease_expires_at_ms: f.envelope.payload.issued_at_ms + 600_000,
        };
        SignedLeaseAssertion {
            signature_b58: bs58::encode(
                coordinator
                    .sign(&lease_assertion_bytes(&claims).unwrap())
                    .to_bytes(),
            )
            .into_string(),
            coordinator_pubkey_b58: coordinator.agent_id().pubkey_base58(),
            claims,
        }
    }

    #[test]
    fn authorization_binds_original_purchase_and_exact_guest_request() {
        let f = native_fixture(1_000_000);
        let authorization =
            authorize_challenge(&f.buyer, &f.expected, &f.challenge, 1_000_001).unwrap();
        verify_owner_authorization(&f.challenge, &authorization).unwrap();
        for case in 0..16 {
            let mut changed = f.challenge.clone();
            match case {
                0 => changed.broker_id.push('x'),
                1 => changed.binding.tenant_id.push('x'),
                2 => changed.binding.key_id.push('x'),
                3 => changed.binding.key_version += 1,
                4 => changed.binding.purpose.push('x'),
                5 => {
                    changed.binding.owner_pubkey_b58 =
                        LocalIdentity::generate("other").agent_id().pubkey_base58()
                }
                6 => changed.binding.job_id = Uuid::new_v4(),
                7 => changed.binding.lease_id.push('x'),
                8 => {
                    changed.binding.operator_pubkey_b58 =
                        LocalIdentity::generate("other").agent_id().pubkey_base58()
                }
                9 => changed.binding.lease_generation.push('x'),
                10 => changed.binding.workload_sha256 = "00".repeat(32),
                11 => changed.binding.phala_app_id = "00".repeat(20),
                12 => changed.binding.phala_instance_id = "00".repeat(20),
                13 => changed.recipient_spki_der_base64.push('x'),
                14 => changed.protocol_version = 2,
                _ => changed.challenge_id = Uuid::nil(),
            }
            assert!(
                authorize_challenge(&f.buyer, &f.expected, &changed, 1_000_001).is_err(),
                "case {case}"
            );
        }
        for now in [999_999, 1_600_000] {
            assert!(authorize_challenge(&f.buyer, &f.expected, &f.challenge, now).is_err());
        }
        let other = LocalIdentity::generate("different-buyer");
        assert!(authorize_challenge(&other, &f.expected, &f.challenge, 1_000_001).is_err());
        let mut unsigned = f.expected.clone();
        unsigned.signed_job_envelope = None;
        assert!(authorize_challenge(&f.buyer, &unsigned, &f.challenge, 1_000_001).is_err());
    }

    #[test]
    fn cannot_construct_delivery_for_closed_or_foreign_lease() {
        let f = native_fixture(1_000_000);
        let mut closed = f.view.clone();
        closed.close_requested = true;
        assert!(challenge_request_for_lease(
            &f.envelope,
            &closed,
            &f.identity,
            &f.expected.recipient_spki_der_base64,
            &f.buyer.agent_id().pubkey_base58(),
            1_000_001
        )
        .is_err());
        assert!(challenge_request_for_lease(
            &f.envelope,
            &f.view,
            &f.identity,
            &f.expected.recipient_spki_der_base64,
            &LocalIdentity::generate("foreign")
                .agent_id()
                .pubkey_base58(),
            1_000_001
        )
        .is_err());
    }

    #[test]
    fn assertion_requires_pinned_signer_signature_and_local_freshness() {
        let f = native_fixture(1_000_000);
        let coordinator = LocalIdentity::generate("coordinator");
        let assertion = signed_assertion(&f, &coordinator, 1_000_001);
        let pin = coordinator.agent_id().pubkey_base58();
        verify_received_assertion(&f.challenge, &assertion, &pin, 1_600_000, 1_000_002).unwrap();
        for now in [1_000_000, 1_030_001, 1_600_000] {
            assert!(
                verify_received_assertion(&f.challenge, &assertion, &pin, 1_600_000, now).is_err()
            );
        }
        assert!(verify_received_assertion(
            &f.challenge,
            &assertion,
            &f.buyer.agent_id().pubkey_base58(),
            1_600_000,
            1_000_002
        )
        .is_err());
        assert!(
            verify_received_assertion(&f.challenge, &assertion, &pin, 1_600_001, 1_000_002)
                .is_err()
        );
        let mut forged = assertion.clone();
        forged.claims.binding.key_id.push('x');
        assert!(
            verify_received_assertion(&f.challenge, &forged, &pin, 1_600_000, 1_000_002).is_err()
        );
    }

    #[tokio::test]
    async fn assertion_http_is_typed_signed_bounded_and_single_attempt() {
        use axum::{
            body::Bytes,
            http::{HeaderMap, StatusCode},
            routing::post,
            Json, Router,
        };
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let f = native_fixture(crate::epoch_ms());
        let coordinator = LocalIdentity::generate("coordinator");
        let pin = coordinator.agent_id().pubkey_base58();
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let response = signed_assertion(&f, &coordinator, crate::epoch_ms());
        let path = format!(
            "/federation/key-broker/{}/lease-assertion",
            f.challenge.binding.job_id
        );
        let route = Router::new().route(
            &path,
            post(move |headers: HeaderMap, bytes: Bytes| {
                let seen = seen.clone();
                let response = response.clone();
                async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    let request: LeaseAssertionRequest = serde_json::from_slice(&bytes).unwrap();
                    verify_owner_authorization(&request.challenge, &request.buyer_authorization)
                        .unwrap();
                    assert!(headers.contains_key("x-covenant-hardware-signature"));
                    Json(response)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, route).await.unwrap();
        });
        let client = KeyBrokerClient::new(&url, &f.buyer, &pin).unwrap();
        client
            .request_lease_assertion(&f.expected, &f.challenge)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        server.abort();
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let route = Router::new().route(
            &path,
            post(move || {
                let seen = seen.clone();
                async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    StatusCode::SERVICE_UNAVAILABLE
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, route).await.unwrap();
        });
        let client = KeyBrokerClient::new(&url, &f.buyer, &pin).unwrap();
        assert!(client
            .request_lease_assertion(&f.expected, &f.challenge)
            .await
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        server.abort();
    }
}
