//! One typed, signed, cert-bound provenance blob.
//!
//! Fields: `type` (discriminator), `payload` (opaque, type-specific JSON),
//! `cert_hash` (binds to one ref-update cert), `signer` (`did:key`), and
//! `sig` (base64url-no-pad ed25519).
//!
//! The signature covers JCS-encoded `{type, payload, cert_hash}` per RFC 8785.
//! Type discriminators are slash-separated namespace + version strings
//! (`covenant/exec/v1`, `slsa/v1.0`, `sigstore/dsse/v1`); verifiers register
//! by exact match.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64U, Engine};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::error::{AttestError, Result};

/// A signed provenance blob attached to a ref-update cert.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attestation {
    /// Type discriminator, e.g. `covenant/exec/v1`.
    #[serde(rename = "type")]
    pub type_: String,

    /// Type-specific payload. The verifier reparses into its concrete shape.
    pub payload: serde_json::Value,

    /// SHA-256 hex of the cert body. Binds this attestation to one cert.
    pub cert_hash: String,

    /// `did:key` of the signer; the verifying key is recoverable from it.
    pub signer: String,

    /// base64url-no-pad ed25519 signature over the JCS signing input.
    pub sig: String,
}

/// Type-specific payload. Implement on a struct that is `Serialize +
/// DeserializeOwned` to participate.
pub trait AttestationPayload: Serialize + DeserializeOwned + Send + Sync {
    /// The discriminator string written on the wire.
    fn payload_type() -> &'static str;
}

#[derive(Serialize)]
struct SigningInput<'a> {
    #[serde(rename = "type")]
    type_: &'a str,
    payload: &'a serde_json::Value,
    cert_hash: &'a str,
}

impl Attestation {
    /// Sign a fresh attestation. `cert_hash_bytes` comes from
    /// [`crate::cert::cert_hash`].
    pub fn sign<P: AttestationPayload>(
        signing_key: &SigningKey,
        payload: P,
        cert_hash_bytes: [u8; 32],
    ) -> Result<Self> {
        let type_ = P::payload_type().to_string();
        validate_type(&type_)?;
        let payload_value = serde_json::to_value(payload)?;
        let cert_hash_hex = hex_encode(&cert_hash_bytes);

        let bytes = canonical_signing_bytes(&type_, &payload_value, &cert_hash_hex)?;
        let sig: Signature = signing_key.sign(&bytes);

        Ok(Self {
            type_,
            payload: payload_value,
            cert_hash: cert_hash_hex,
            signer: did_key_from_verifying_key(&signing_key.verifying_key()),
            sig: B64U.encode(sig.to_bytes()),
        })
    }

    /// Verify the signature and check that `cert_hash` matches
    /// `expected_cert_hash`. Returns the recovered verifying key so the caller
    /// can check it against an allowlist.
    pub fn verify_signature(&self, expected_cert_hash: [u8; 32]) -> Result<VerifyingKey> {
        let expected_hex = hex_encode(&expected_cert_hash);
        if self.cert_hash != expected_hex {
            return Err(AttestError::CertHashMismatch);
        }

        validate_type(&self.type_)?;
        let bytes = canonical_signing_bytes(&self.type_, &self.payload, &self.cert_hash)?;

        let vk = verifying_key_from_did_key(&self.signer)?;
        let sig_bytes: [u8; 64] = B64U
            .decode(&self.sig)
            .map_err(|e| AttestError::Signature(format!("base64url: {e}")))?
            .try_into()
            .map_err(|_| AttestError::Signature("signature must be 64 bytes".into()))?;
        let sig = Signature::from_bytes(&sig_bytes);
        vk.verify(&bytes, &sig)
            .map_err(|e| AttestError::Signature(format!("ed25519: {e}")))?;

        Ok(vk)
    }

    /// Reparse `payload` as `P`. Errors if the type discriminator does not
    /// match `P::payload_type()`.
    pub fn payload_as<P: AttestationPayload>(&self) -> Result<P> {
        if self.type_ != P::payload_type() {
            return Err(AttestError::Type(format!(
                "expected '{}', got '{}'",
                P::payload_type(),
                self.type_
            )));
        }
        Ok(serde_json::from_value(self.payload.clone())?)
    }
}

fn validate_type(t: &str) -> Result<()> {
    if t.is_empty() {
        return Err(AttestError::Type("empty discriminator".into()));
    }
    if t.contains(char::is_whitespace) {
        return Err(AttestError::Type(format!(
            "discriminator must not contain whitespace: '{t}'"
        )));
    }
    Ok(())
}

fn canonical_signing_bytes(
    type_: &str,
    payload: &serde_json::Value,
    cert_hash_hex: &str,
) -> Result<Vec<u8>> {
    let input = SigningInput {
        type_,
        payload,
        cert_hash: cert_hash_hex,
    };
    serde_jcs::to_vec(&input).map_err(|e| AttestError::Payload(format!("JCS encode: {e}")))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

const ED25519_MULTICODEC: [u8; 2] = [0xed, 0x01];

fn did_key_from_verifying_key(key: &VerifyingKey) -> String {
    let mut buf = Vec::with_capacity(ED25519_MULTICODEC.len() + 32);
    buf.extend_from_slice(&ED25519_MULTICODEC);
    buf.extend_from_slice(&key.to_bytes());
    format!("did:key:z{}", bs58::encode(&buf).into_string())
}

fn verifying_key_from_did_key(did: &str) -> Result<VerifyingKey> {
    let method_id = did
        .strip_prefix("did:key:")
        .ok_or_else(|| AttestError::Did(format!("not a did:key: {did}")))?;
    let encoded = method_id
        .strip_prefix('z')
        .ok_or_else(|| AttestError::Did("missing base58btc 'z' prefix".into()))?;
    let bytes = bs58::decode(encoded)
        .into_vec()
        .map_err(|e| AttestError::Did(format!("base58 decode: {e}")))?;
    if bytes.len() != ED25519_MULTICODEC.len() + 32
        || bytes[..ED25519_MULTICODEC.len()] != ED25519_MULTICODEC
    {
        return Err(AttestError::Did("not an ed25519 did:key".into()));
    }
    let key_bytes: [u8; 32] = bytes[ED25519_MULTICODEC.len()..]
        .try_into()
        .expect("length checked above");
    VerifyingKey::from_bytes(&key_bytes)
        .map_err(|e| AttestError::Did(format!("invalid ed25519 key: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;
    use rand::TryRngCore;
    use serde::{Deserialize, Serialize};

    fn fresh() -> SigningKey {
        let mut seed = [0u8; 32];
        OsRng.try_fill_bytes(&mut seed).unwrap();
        SigningKey::from_bytes(&seed)
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    struct DummyPayload {
        agent: String,
        commit: String,
    }

    impl AttestationPayload for DummyPayload {
        fn payload_type() -> &'static str {
            "test/dummy/v1"
        }
    }

    fn sample_cert_hash() -> [u8; 32] {
        let mut h = [0u8; 32];
        for (i, b) in h.iter_mut().enumerate() {
            *b = i as u8;
        }
        h
    }

    #[test]
    fn sign_then_verify_roundtrip() {
        let sk = fresh();
        let cert_hash = sample_cert_hash();
        let payload = DummyPayload {
            agent: "did:key:z6MkTest".into(),
            commit: "deadbeef".into(),
        };
        let att = Attestation::sign(&sk, payload.clone(), cert_hash).unwrap();
        let vk = att.verify_signature(cert_hash).unwrap();
        assert_eq!(vk.to_bytes(), sk.verifying_key().to_bytes());
        assert_eq!(att.payload_as::<DummyPayload>().unwrap(), payload);
    }

    #[test]
    fn cross_cert_replay_fails() {
        let sk = fresh();
        let cert_a = sample_cert_hash();
        let mut cert_b = sample_cert_hash();
        cert_b[0] ^= 0xff;
        let payload = DummyPayload {
            agent: "a".into(),
            commit: "b".into(),
        };
        let att = Attestation::sign(&sk, payload, cert_a).unwrap();
        let err = att.verify_signature(cert_b).unwrap_err();
        assert!(matches!(err, AttestError::CertHashMismatch));
    }

    #[test]
    fn tampered_payload_fails_verify() {
        let sk = fresh();
        let cert_hash = sample_cert_hash();
        let payload = DummyPayload {
            agent: "a".into(),
            commit: "b".into(),
        };
        let mut att = Attestation::sign(&sk, payload, cert_hash).unwrap();
        att.payload = serde_json::json!({ "agent": "evil", "commit": "b" });
        let err = att.verify_signature(cert_hash).unwrap_err();
        assert!(matches!(err, AttestError::Signature(_)));
    }

    #[test]
    fn payload_as_wrong_type_errors() {
        let sk = fresh();
        let cert_hash = sample_cert_hash();
        let att = Attestation::sign(
            &sk,
            DummyPayload {
                agent: "a".into(),
                commit: "b".into(),
            },
            cert_hash,
        )
        .unwrap();

        #[derive(Debug, Serialize, Deserialize)]
        struct Wrong {
            #[allow(dead_code)]
            x: String,
        }
        impl AttestationPayload for Wrong {
            fn payload_type() -> &'static str {
                "test/other/v1"
            }
        }
        let err = att.payload_as::<Wrong>().unwrap_err();
        assert!(matches!(err, AttestError::Type(_)));
    }

    #[test]
    fn json_roundtrip_preserves_signature() {
        let sk = fresh();
        let cert_hash = sample_cert_hash();
        let att = Attestation::sign(
            &sk,
            DummyPayload {
                agent: "a".into(),
                commit: "b".into(),
            },
            cert_hash,
        )
        .unwrap();
        let json = serde_json::to_string(&att).unwrap();
        let back: Attestation = serde_json::from_str(&json).unwrap();
        back.verify_signature(cert_hash).unwrap();
    }

    #[test]
    fn empty_type_rejected_at_signing() {
        #[derive(Serialize, Deserialize)]
        struct Empty {}
        impl AttestationPayload for Empty {
            fn payload_type() -> &'static str {
                ""
            }
        }
        let sk = fresh();
        let err = Attestation::sign(&sk, Empty {}, sample_cert_hash()).unwrap_err();
        assert!(matches!(err, AttestError::Type(_)));
    }
}
