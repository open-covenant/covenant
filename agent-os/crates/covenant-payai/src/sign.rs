use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use covenant_identity::{verify_b58, LocalIdentity};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::{PayaiError, Result};

/// A signed credential envelope. `payload` is the exact JSON string that was
/// signed; verifiers re-check the signature over these bytes and parse them
/// verbatim, never re-serializing. Matches Covenant's escrow completion proofs
/// and dodges cross-language canonicalization drift.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedEnvelope {
    pub payload: String,
    pub signature_b58: String,
    pub signer_pubkey_b58: String,
}

impl SignedEnvelope {
    /// Sign a value: the payload is its canonical (declaration-order) JSON.
    pub fn sign<T: Serialize>(identity: &LocalIdentity, value: &T) -> Result<SignedEnvelope> {
        let payload = serde_json::to_string(value)?;
        let sig = identity.sign(payload.as_bytes());
        Ok(SignedEnvelope {
            payload,
            signature_b58: bs58::encode(sig.to_bytes()).into_string(),
            signer_pubkey_b58: bs58::encode(identity.pubkey_bytes()).into_string(),
        })
    }

    /// Sign the SAME payload bytes as an existing envelope: a counter-signature
    /// (e.g. a buyer accepting a seller's receipt). Payload is byte-identical;
    /// only the signer differs.
    pub fn countersign(identity: &LocalIdentity, other: &SignedEnvelope) -> SignedEnvelope {
        let sig = identity.sign(other.payload.as_bytes());
        SignedEnvelope {
            payload: other.payload.clone(),
            signature_b58: bs58::encode(sig.to_bytes()).into_string(),
            signer_pubkey_b58: bs58::encode(identity.pubkey_bytes()).into_string(),
        }
    }

    /// Verify the signature over `payload` against `signer_pubkey_b58`.
    pub fn verify(&self) -> Result<()> {
        verify_b58(
            &self.signer_pubkey_b58,
            self.payload.as_bytes(),
            &self.signature_b58,
        )
        .map_err(|e| PayaiError::Verify(e.to_string()))
    }

    /// Parse the payload back into a typed value. Does NOT verify; call
    /// [`Self::verify`] first.
    pub fn deserialize<T: DeserializeOwned>(&self) -> Result<T> {
        Ok(serde_json::from_str(&self.payload)?)
    }

    /// Base64 wire form, for embedding in an HTTP header or response field.
    pub fn to_blob_b64(&self) -> Result<String> {
        let json = serde_json::to_string(self)?;
        Ok(BASE64.encode(json.as_bytes()))
    }

    pub fn from_blob_b64(blob: &str) -> Result<SignedEnvelope> {
        let bytes = BASE64
            .decode(blob)
            .map_err(|e| PayaiError::Encoding(e.to_string()))?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Doc {
        a: u32,
        b: String,
    }

    #[test]
    fn sign_then_verify_round_trips() {
        let id = LocalIdentity::generate("seller@local");
        let doc = Doc {
            a: 7,
            b: "hi".into(),
        };
        let env = SignedEnvelope::sign(&id, &doc).unwrap();
        env.verify().unwrap();
        assert_eq!(env.deserialize::<Doc>().unwrap(), doc);
        assert_eq!(
            env.signer_pubkey_b58,
            bs58::encode(id.pubkey_bytes()).into_string()
        );
    }

    #[test]
    fn verify_rejects_tampered_payload() {
        let id = LocalIdentity::generate("seller@local");
        let mut env = SignedEnvelope::sign(
            &id,
            &Doc {
                a: 1,
                b: "x".into(),
            },
        )
        .unwrap();
        env.payload = env.payload.replace("\"a\":1", "\"a\":2");
        assert!(env.verify().is_err());
    }

    #[test]
    fn blob_round_trips() {
        let id = LocalIdentity::generate("seller@local");
        let env = SignedEnvelope::sign(
            &id,
            &Doc {
                a: 3,
                b: "y".into(),
            },
        )
        .unwrap();
        let blob = env.to_blob_b64().unwrap();
        let back = SignedEnvelope::from_blob_b64(&blob).unwrap();
        assert_eq!(env, back);
        back.verify().unwrap();
    }

    #[test]
    fn countersign_over_same_payload_verifies() {
        let seller = LocalIdentity::generate("seller@local");
        let buyer = LocalIdentity::generate("buyer@local");
        let env = SignedEnvelope::sign(
            &seller,
            &Doc {
                a: 9,
                b: "z".into(),
            },
        )
        .unwrap();
        let cs = SignedEnvelope::countersign(&buyer, &env);
        assert_eq!(cs.payload, env.payload);
        cs.verify().unwrap();
        assert_ne!(cs.signer_pubkey_b58, env.signer_pubkey_b58);
    }
}
