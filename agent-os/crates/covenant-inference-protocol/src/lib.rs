//! Wire protocol for a Solana verifiable open-model inference network:
//! ed25519 node identity, signed telemetry, a trust-class ladder the control
//! plane can only raise on evidence it can check itself, and inference
//! receipts whose hash commits to the model and the exact turn that produced
//! an output.

use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const ENROLLMENT_SIGNATURE_DOMAIN: &[u8] = b"covenant.node-enrollment.v1\0";
const TELEMETRY_SIGNATURE_DOMAIN: &[u8] = b"covenant.telemetry.v1\0";
const TUNNEL_SIGNATURE_DOMAIN: &[u8] = b"covenant.node-tunnel.v1\0";

/// The deterministic identity of a served model. Two nodes that agree on every
/// field here will, given the same prompt and seed, produce the same tokens, so
/// this digest is what a receipt commits to when it claims what ran.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelIdentity {
    pub weights_hash: String,
    pub quantization: String,
    pub runtime: String,
    pub runtime_version: String,
    pub sampling_params: SamplingParams,
}

/// Sampling is part of a model's identity because it changes the output
/// distribution. Field order is fixed so the canonical JSON is reproducible.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SamplingParams {
    pub temperature: f64,
    pub top_p: f64,
    pub seed: u64,
    pub max_tokens: u32,
}

impl ModelIdentity {
    pub fn digest(&self) -> String {
        let json = canonical_json(self).expect("model identity is always serializable");
        hex::encode(Sha256::digest(json))
    }
}

/// What a caller can rely on for a given node, ordered from weakest to
/// strongest. Every level above `Open` has to be earned from evidence the
/// control plane can check itself; a node cannot talk its way up.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum TrustClass {
    /// Bonded identity and metered billing. The operator can read anything the
    /// workload touches.
    #[default]
    Open,
    /// Isolated VM with a digest-pinned image. Narrows the attack surface; the
    /// host is still privileged.
    Isolated,
    /// Launch measurement and device identity verified against vendor roots, so
    /// the caller can check what booted.
    Attested,
    /// Guest memory is encrypted against the host.
    Confidential,
}

/// The strongest class the network can currently substantiate. Attestation
/// evidence rides along end to end but isn't verified here, so nothing is
/// served above `Isolated` no matter what a node claims. Wiring a covenant-tee
/// DCAP verifier into the control plane is what raises this ceiling to
/// `Attested`.
pub const MAX_VERIFIABLE_TRUST_CLASS: TrustClass = TrustClass::Isolated;

impl TrustClass {
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Isolated => "isolated",
            Self::Attested => "attested",
            Self::Confidential => "confidential",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IsolationMode {
    #[default]
    Shared,
    Isolated,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttestationKind {
    SevSnp,
    Tdx,
    NvidiaCc,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttestationRef {
    pub kind: AttestationKind,
    pub quote_sha256: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodePosture {
    pub isolation: IsolationMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attestation: Option<AttestationRef>,
}

impl NodePosture {
    /// The class this posture would justify if every piece of evidence in it
    /// checked out. Callers must clamp it with [`MAX_VERIFIABLE_TRUST_CLASS`].
    pub fn claimed_class(&self) -> TrustClass {
        match (self.isolation, self.attestation.as_ref()) {
            (IsolationMode::Shared, _) => TrustClass::Open,
            (IsolationMode::Isolated, None) => TrustClass::Isolated,
            (IsolationMode::Isolated, Some(attestation)) => match attestation.kind {
                AttestationKind::NvidiaCc => TrustClass::Confidential,
                AttestationKind::SevSnp | AttestationKind::Tdx => TrustClass::Attested,
            },
        }
    }

    pub fn effective_class(&self) -> TrustClass {
        self.claimed_class().min(MAX_VERIFIABLE_TRUST_CLASS)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeEnrollment {
    pub node_id: String,
    pub device_public_key: String,
    pub operator_wallet: String,
    pub payout_wallet: String,
    pub served_models: Vec<ModelIdentity>,
    pub max_tokens_per_second: u32,
    pub rate_per_second: u64,
    pub issued_at: DateTime<Utc>,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnsignedNodeEnrollment {
    pub node_id: String,
    pub device_public_key: String,
    pub operator_wallet: String,
    pub payout_wallet: String,
    pub served_models: Vec<ModelIdentity>,
    pub max_tokens_per_second: u32,
    pub rate_per_second: u64,
    pub issued_at: DateTime<Utc>,
}

impl NodeEnrollment {
    pub fn sign(unsigned: UnsignedNodeEnrollment, key: &SigningKey) -> Result<Self, ProtocolError> {
        let payload = signature_payload(ENROLLMENT_SIGNATURE_DOMAIN, &unsigned)?;
        let signature = key.sign(&payload);
        Ok(Self {
            node_id: unsigned.node_id,
            device_public_key: unsigned.device_public_key,
            operator_wallet: unsigned.operator_wallet,
            payout_wallet: unsigned.payout_wallet,
            served_models: unsigned.served_models,
            max_tokens_per_second: unsigned.max_tokens_per_second,
            rate_per_second: unsigned.rate_per_second,
            issued_at: unsigned.issued_at,
            signature: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
        })
    }

    pub fn verify(&self, key: &VerifyingKey) -> Result<(), ProtocolError> {
        verify_signature(
            &UnsignedNodeEnrollment {
                node_id: self.node_id.clone(),
                device_public_key: self.device_public_key.clone(),
                operator_wallet: self.operator_wallet.clone(),
                payout_wallet: self.payout_wallet.clone(),
                served_models: self.served_models.clone(),
                max_tokens_per_second: self.max_tokens_per_second,
                rate_per_second: self.rate_per_second,
                issued_at: self.issued_at,
            },
            &self.signature,
            key,
            ENROLLMENT_SIGNATURE_DOMAIN,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeTelemetry {
    pub node_id: String,
    pub sequence: u64,
    pub observed_at: DateTime<Utc>,
    pub active_sessions: u32,
    pub tokens_per_second: u32,
    pub tunnel_connected: bool,
    pub served_model_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posture: Option<NodePosture>,
    pub signature: String,
}

/// Skipping `posture` when absent keeps the canonical payload byte-identical to
/// the pre-posture format, so nodes on older builds still verify.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnsignedTelemetry {
    pub node_id: String,
    pub sequence: u64,
    pub observed_at: DateTime<Utc>,
    pub active_sessions: u32,
    pub tokens_per_second: u32,
    pub tunnel_connected: bool,
    pub served_model_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posture: Option<NodePosture>,
}

impl NodeTelemetry {
    pub fn sign(unsigned: UnsignedTelemetry, key: &SigningKey) -> Result<Self, ProtocolError> {
        let payload = signature_payload(TELEMETRY_SIGNATURE_DOMAIN, &unsigned)?;
        let signature = key.sign(&payload);
        Ok(Self {
            node_id: unsigned.node_id,
            sequence: unsigned.sequence,
            observed_at: unsigned.observed_at,
            active_sessions: unsigned.active_sessions,
            tokens_per_second: unsigned.tokens_per_second,
            tunnel_connected: unsigned.tunnel_connected,
            served_model_digest: unsigned.served_model_digest,
            posture: unsigned.posture,
            signature: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
        })
    }

    pub fn verify(&self, key: &VerifyingKey) -> Result<(), ProtocolError> {
        verify_signature(
            &UnsignedTelemetry {
                node_id: self.node_id.clone(),
                sequence: self.sequence,
                observed_at: self.observed_at,
                active_sessions: self.active_sessions,
                tokens_per_second: self.tokens_per_second,
                tunnel_connected: self.tunnel_connected,
                served_model_digest: self.served_model_digest.clone(),
                posture: self.posture.clone(),
            },
            &self.signature,
            key,
            TELEMETRY_SIGNATURE_DOMAIN,
        )
    }
}

/// Per-node replay guard. A stream of telemetry is only accepted while its
/// sequence strictly advances, which drops both replays and reordered
/// deliveries without keeping unbounded history.
#[derive(Debug, Default, Clone)]
pub struct SequenceGuard {
    last: Option<u64>,
}

impl SequenceGuard {
    pub fn admit(&mut self, telemetry: &NodeTelemetry) -> bool {
        if self.last.is_some_and(|last| telemetry.sequence <= last) {
            return false;
        }
        self.last = Some(telemetry.sequence);
        true
    }

    pub fn last(&self) -> Option<u64> {
        self.last
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TunnelRegistration {
    pub node_id: String,
    pub device_public_key: String,
    pub connection_id: String,
    pub issued_at: DateTime<Utc>,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnsignedTunnelRegistration {
    pub node_id: String,
    pub device_public_key: String,
    pub connection_id: String,
    pub issued_at: DateTime<Utc>,
}

impl TunnelRegistration {
    pub fn sign(
        unsigned: UnsignedTunnelRegistration,
        key: &SigningKey,
    ) -> Result<Self, ProtocolError> {
        let payload = signature_payload(TUNNEL_SIGNATURE_DOMAIN, &unsigned)?;
        let signature = key.sign(&payload);
        Ok(Self {
            node_id: unsigned.node_id,
            device_public_key: unsigned.device_public_key,
            connection_id: unsigned.connection_id,
            issued_at: unsigned.issued_at,
            signature: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
        })
    }

    pub fn verify(&self, key: &VerifyingKey) -> Result<(), ProtocolError> {
        verify_signature(
            &UnsignedTunnelRegistration {
                node_id: self.node_id.clone(),
                device_public_key: self.device_public_key.clone(),
                connection_id: self.connection_id.clone(),
                issued_at: self.issued_at,
            },
            &self.signature,
            key,
            TUNNEL_SIGNATURE_DOMAIN,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptOutcome {
    Finalized,
    Refunded,
    Disputed,
}

/// The hashed core of a receipt: which model ran, over which prompt, producing
/// which output, on which node, and when. The receipt hash is taken over this
/// alone, so settlement fields and the on-chain signature can be filled in
/// afterwards without disturbing it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InferenceReceiptPayload {
    pub model_identity_digest: String,
    pub prompt_hash: String,
    pub output_hash: String,
    pub node_id: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicReceipt {
    pub payload: InferenceReceiptPayload,
    pub charged_base_units: u64,
    pub refunded_base_units: u64,
    pub provider_paid_base_units: u64,
    pub outcome: ReceiptOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_class: Option<TrustClass>,
    pub receipt_hash: String,
    /// Solana settlement signature (base58). Deliberately outside the hash so a
    /// receipt can be published before it is anchored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction_hash: Option<String>,
}

pub fn receipt_hash(payload: &InferenceReceiptPayload) -> Result<String, ProtocolError> {
    Ok(hex::encode(Sha256::digest(canonical_json(payload)?)))
}

pub fn receipt_hash_matches(receipt: &PublicReceipt) -> Result<bool, ProtocolError> {
    Ok(receipt.receipt_hash == receipt_hash(&receipt.payload)?)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EncryptedSecret {
    pub nonce: String,
    pub ciphertext: String,
}

#[derive(Clone)]
pub struct CredentialCipher(Aes256Gcm);

impl CredentialCipher {
    pub fn from_hex(value: &str) -> Result<Self, ProtocolError> {
        let bytes = hex::decode(value).map_err(|_| ProtocolError::InvalidEncryptionKey)?;
        if bytes.len() != 32 {
            return Err(ProtocolError::InvalidEncryptionKey);
        }
        Ok(Self(
            Aes256Gcm::new_from_slice(&bytes).map_err(|_| ProtocolError::InvalidEncryptionKey)?,
        ))
    }

    pub fn encrypt(&self, value: &str) -> Result<EncryptedSecret, ProtocolError> {
        let mut nonce = [0_u8; 12];
        getrandom::getrandom(&mut nonce).map_err(|_| ProtocolError::Encryption)?;
        let nonce_value = nonce.into();
        let ciphertext = self
            .0
            .encrypt(&nonce_value, value.as_bytes())
            .map_err(|_| ProtocolError::Encryption)?;
        Ok(EncryptedSecret {
            nonce: URL_SAFE_NO_PAD.encode(nonce),
            ciphertext: URL_SAFE_NO_PAD.encode(ciphertext),
        })
    }

    pub fn decrypt(&self, secret: &EncryptedSecret) -> Result<String, ProtocolError> {
        let nonce = URL_SAFE_NO_PAD
            .decode(&secret.nonce)
            .map_err(|_| ProtocolError::Encryption)?;
        let ciphertext = URL_SAFE_NO_PAD
            .decode(&secret.ciphertext)
            .map_err(|_| ProtocolError::Encryption)?;
        if nonce.len() != 12 {
            return Err(ProtocolError::Encryption);
        }
        let nonce_value: [u8; 12] = nonce.try_into().map_err(|_| ProtocolError::Encryption)?;
        let nonce_value = Nonce::from(nonce_value);
        let plaintext = self
            .0
            .decrypt(&nonce_value, ciphertext.as_ref())
            .map_err(|_| ProtocolError::Encryption)?;
        String::from_utf8(plaintext).map_err(|_| ProtocolError::Encryption)
    }
}

pub fn node_id(device_public_key: &VerifyingKey) -> String {
    let digest = Sha256::digest(device_public_key.as_bytes());
    format!("0x{}", hex::encode(digest))
}

pub fn verifying_key(encoded: &str) -> Result<VerifyingKey, ProtocolError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| ProtocolError::InvalidPublicKey)?;
    VerifyingKey::from_bytes(
        bytes
            .as_slice()
            .try_into()
            .map_err(|_| ProtocolError::InvalidPublicKey)?,
    )
    .map_err(|_| ProtocolError::InvalidPublicKey)
}

fn canonical_json<T: Serialize>(value: &T) -> Result<String, ProtocolError> {
    serde_json::to_string(value).map_err(ProtocolError::Serialization)
}

fn signature_payload<T: Serialize>(domain: &[u8], value: &T) -> Result<Vec<u8>, ProtocolError> {
    let encoded = canonical_json(value)?;
    let mut payload = Vec::with_capacity(domain.len() + encoded.len());
    payload.extend_from_slice(domain);
    payload.extend_from_slice(encoded.as_bytes());
    Ok(payload)
}

fn verify_signature<T: Serialize>(
    value: &T,
    encoded_signature: &str,
    key: &VerifyingKey,
    domain: &[u8],
) -> Result<(), ProtocolError> {
    let signature = URL_SAFE_NO_PAD
        .decode(encoded_signature)
        .map_err(|_| ProtocolError::InvalidSignature)?;
    let signature =
        Signature::from_slice(&signature).map_err(|_| ProtocolError::InvalidSignature)?;
    let payload = signature_payload(domain, value)?;
    key.verify(&payload, &signature)
        .map_err(|_| ProtocolError::InvalidSignature)
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("invalid signature")]
    InvalidSignature,
    #[error("invalid public key")]
    InvalidPublicKey,
    #[error("serialization failed")]
    Serialization(#[source] serde_json::Error),
    #[error("credential encryption key is invalid")]
    InvalidEncryptionKey,
    #[error("credential encryption failed")]
    Encryption,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generate_key() -> SigningKey {
        let mut seed = [0_u8; 32];
        getrandom::getrandom(&mut seed).expect("os rng");
        SigningKey::from_bytes(&seed)
    }

    fn sample_model() -> ModelIdentity {
        ModelIdentity {
            weights_hash: "sha256:0f0f".to_owned(),
            quantization: "q4_k_m".to_owned(),
            runtime: "llama.cpp".to_owned(),
            runtime_version: "b3200".to_owned(),
            sampling_params: SamplingParams {
                temperature: 0.7,
                top_p: 0.95,
                seed: 42,
                max_tokens: 512,
            },
        }
    }

    fn signed_telemetry(key: &SigningKey, sequence: u64) -> NodeTelemetry {
        NodeTelemetry::sign(
            UnsignedTelemetry {
                node_id: node_id(&key.verifying_key()),
                sequence,
                observed_at: Utc::now(),
                active_sessions: 3,
                tokens_per_second: 120,
                tunnel_connected: true,
                served_model_digest: Some(sample_model().digest()),
                posture: None,
            },
            key,
        )
        .unwrap()
    }

    #[test]
    fn trust_classes_order_weakest_first() {
        assert!(TrustClass::Open < TrustClass::Isolated);
        assert!(TrustClass::Isolated < TrustClass::Attested);
        assert!(TrustClass::Attested < TrustClass::Confidential);
        assert_eq!(TrustClass::default(), TrustClass::Open);
    }

    #[test]
    fn posture_claims_are_clamped_to_what_the_network_can_verify() {
        let confidential = NodePosture {
            isolation: IsolationMode::Isolated,
            attestation: Some(AttestationRef {
                kind: AttestationKind::NvidiaCc,
                quote_sha256: "0".repeat(64),
            }),
        };
        assert_eq!(confidential.claimed_class(), TrustClass::Confidential);
        assert_eq!(confidential.effective_class(), MAX_VERIFIABLE_TRUST_CLASS);
        assert_eq!(confidential.effective_class(), TrustClass::Isolated);

        let shared = NodePosture::default();
        assert_eq!(shared.claimed_class(), TrustClass::Open);
        assert_eq!(shared.effective_class(), TrustClass::Open);
    }

    #[test]
    fn telemetry_round_trip_verifies() {
        let key = generate_key();
        let telemetry = signed_telemetry(&key, 1);
        assert!(telemetry.verify(&key.verifying_key()).is_ok());
    }

    #[test]
    fn tampered_telemetry_is_rejected() {
        let key = generate_key();
        let mut telemetry = signed_telemetry(&key, 1);
        telemetry.tokens_per_second += 1;
        assert!(telemetry.verify(&key.verifying_key()).is_err());
    }

    #[test]
    fn telemetry_without_posture_signs_the_legacy_payload() {
        let unsigned = UnsignedTelemetry {
            node_id: "0xabc".to_owned(),
            sequence: 1,
            observed_at: Utc::now(),
            active_sessions: 0,
            tokens_per_second: 0,
            tunnel_connected: true,
            served_model_digest: None,
            posture: None,
        };
        assert!(!canonical_json(&unsigned).unwrap().contains("posture"));
    }

    #[test]
    fn telemetry_posture_is_covered_by_the_signature() {
        let key = generate_key();
        let mut telemetry = NodeTelemetry::sign(
            UnsignedTelemetry {
                node_id: node_id(&key.verifying_key()),
                sequence: 1,
                observed_at: Utc::now(),
                active_sessions: 0,
                tokens_per_second: 0,
                tunnel_connected: true,
                served_model_digest: None,
                posture: Some(NodePosture {
                    isolation: IsolationMode::Isolated,
                    attestation: None,
                }),
            },
            &key,
        )
        .unwrap();

        assert!(telemetry.verify(&key.verifying_key()).is_ok());
        telemetry.posture = None;
        assert!(telemetry.verify(&key.verifying_key()).is_err());
    }

    #[test]
    fn sequence_guard_drops_replays_and_reorderings() {
        let key = generate_key();
        let mut guard = SequenceGuard::default();
        assert!(guard.admit(&signed_telemetry(&key, 1)));
        assert!(guard.admit(&signed_telemetry(&key, 2)));
        assert!(!guard.admit(&signed_telemetry(&key, 2)));
        assert!(!guard.admit(&signed_telemetry(&key, 1)));
        assert!(guard.admit(&signed_telemetry(&key, 3)));
        assert_eq!(guard.last(), Some(3));
    }

    #[test]
    fn enrollment_round_trip_verifies_and_rejects_tampering() {
        let key = generate_key();
        let mut enrollment = NodeEnrollment::sign(
            UnsignedNodeEnrollment {
                node_id: node_id(&key.verifying_key()),
                device_public_key: URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes()),
                operator_wallet: "8xbXHAh15QHqvS8Hh7cGxtVQD1TKtqZgQ2n7bK9K1Zop".to_owned(),
                payout_wallet: "5vJRzKtcpwzT8Vq3fY8f7pQe9uW1cDp2b6z3s4t5u6v7".to_owned(),
                served_models: vec![sample_model()],
                max_tokens_per_second: 200,
                rate_per_second: 1_000,
                issued_at: Utc::now(),
            },
            &key,
        )
        .unwrap();

        assert!(enrollment.verify(&key.verifying_key()).is_ok());
        enrollment.max_tokens_per_second += 1;
        assert!(enrollment.verify(&key.verifying_key()).is_err());
    }

    #[test]
    fn tunnel_registration_is_bound_to_node_and_connection() {
        let key = generate_key();
        let registration = TunnelRegistration::sign(
            UnsignedTunnelRegistration {
                node_id: node_id(&key.verifying_key()),
                device_public_key: URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes()),
                connection_id: "connection-1".to_owned(),
                issued_at: Utc::now(),
            },
            &key,
        )
        .unwrap();
        assert!(registration.verify(&key.verifying_key()).is_ok());

        let mut wrong_node = registration.clone();
        wrong_node.node_id = "0xdeadbeef".to_owned();
        assert!(wrong_node.verify(&key.verifying_key()).is_err());

        let mut stale = registration;
        stale.issued_at -= chrono::Duration::seconds(90);
        assert!(stale.verify(&key.verifying_key()).is_err());
    }

    #[test]
    fn receipt_hash_is_stable_and_ignores_the_transaction() {
        let payload = InferenceReceiptPayload {
            model_identity_digest: sample_model().digest(),
            prompt_hash: "sha256:1111".to_owned(),
            output_hash: "sha256:2222".to_owned(),
            node_id: "0x1234".to_owned(),
            timestamp: 1_723_000_000,
        };
        let mut receipt = PublicReceipt {
            payload,
            charged_base_units: 1_000,
            refunded_base_units: 0,
            provider_paid_base_units: 900,
            outcome: ReceiptOutcome::Finalized,
            trust_class: Some(TrustClass::Open),
            receipt_hash: String::new(),
            transaction_hash: None,
        };
        receipt.receipt_hash = receipt_hash(&receipt.payload).unwrap();
        assert!(receipt_hash_matches(&receipt).unwrap());

        receipt.transaction_hash = Some("5wHu1qHkP3nqs8yqjR8mF6mQ9nS7dV2cX4bZ1aA3eE5f".to_owned());
        assert!(receipt_hash_matches(&receipt).unwrap());

        receipt.payload.output_hash = "sha256:3333".to_owned();
        assert!(!receipt_hash_matches(&receipt).unwrap());
    }

    #[test]
    fn model_identity_digest_is_deterministic_and_field_sensitive() {
        let model = sample_model();
        assert_eq!(model.digest(), model.digest());

        let mut reseeded = model.clone();
        reseeded.sampling_params.seed = 43;
        assert_ne!(model.digest(), reseeded.digest());

        let mut requantized = model.clone();
        requantized.quantization = "q8_0".to_owned();
        assert_ne!(model.digest(), requantized.digest());
    }

    #[test]
    fn credential_cipher_round_trips_and_rejects_tampering() {
        let cipher = CredentialCipher::from_hex(&"11".repeat(32)).unwrap();
        let mut secret = cipher.encrypt("temporary credential").unwrap();
        assert_eq!(cipher.decrypt(&secret).unwrap(), "temporary credential");
        secret.ciphertext.push('A');
        assert!(cipher.decrypt(&secret).is_err());
    }
}
