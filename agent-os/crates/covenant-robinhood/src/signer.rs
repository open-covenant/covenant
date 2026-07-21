//! Ed25519 request signing for the Robinhood Crypto Trading API.
//!
//! Each authenticated request carries `x-api-key`, `x-timestamp`, and
//! `x-signature`. The signature is Ed25519 over
//! `api_key + timestamp + path + method + body`, base64 encoded. The signed
//! `path` includes the query string, so we sign exactly the bytes we send.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signer as _, SigningKey};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Clone)]
pub struct RobinhoodSigner {
    api_key: String,
    key: SigningKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignRequest {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedHeaders {
    #[serde(rename = "x-api-key")]
    pub api_key: String,
    #[serde(rename = "x-timestamp")]
    pub timestamp: String,
    #[serde(rename = "x-signature")]
    pub signature: String,
}

impl RobinhoodSigner {
    pub fn new(api_key: impl Into<String>, key: SigningKey) -> Self {
        Self {
            api_key: api_key.into(),
            key,
        }
    }

    /// Build from the API key and the base64 private key from Robinhood's
    /// credentials portal. Accepts a 32-byte seed or a 64-byte libsodium
    /// secret key (seed followed by public key); only the seed half is used.
    pub fn from_base64_key(api_key: impl Into<String>, private_key_b64: &str) -> Result<Self> {
        let raw = STANDARD
            .decode(private_key_b64.trim())
            .map_err(|e| Error::Credential(format!("base64 private key: {e}")))?;
        let seed: [u8; 32] = match raw.len() {
            32 | 64 => raw[..32].try_into().unwrap(),
            n => {
                return Err(Error::Credential(format!(
                    "expected 32 or 64 key bytes, got {n}"
                )))
            }
        };
        Ok(Self::new(api_key, SigningKey::from_bytes(&seed)))
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// Base64 public key, for registering credentials with Robinhood.
    pub fn public_key_b64(&self) -> String {
        STANDARD.encode(self.key.verifying_key().to_bytes())
    }

    /// Sign a request. `path` must already include the query string; `body` is
    /// the exact serialized body, empty for GET.
    pub fn sign(&self, method: &str, path: &str, body: &str, timestamp: u64) -> SignedHeaders {
        let ts = timestamp.to_string();
        let message = format!("{}{}{}{}{}", self.api_key, ts, path, method, body);
        let signature = self.key.sign(message.as_bytes());
        SignedHeaders {
            api_key: self.api_key.clone(),
            timestamp: ts,
            signature: STANDARD.encode(signature.to_bytes()),
        }
    }

    pub fn sign_request(&self, req: &SignRequest) -> SignedHeaders {
        let ts = req.timestamp.unwrap_or_else(crate::unix_now);
        self.sign(&req.method, &req.path, &req.body, ts)
    }
}

/// Produces the signed headers for a request. Implemented in-process by
/// [`RobinhoodSigner`], or out-of-process by a sidecar that holds the key so
/// the daemon never sees the seed.
#[async_trait::async_trait]
pub trait RequestSigner: Send + Sync {
    async fn signed_headers(
        &self,
        method: &str,
        path: &str,
        body: &str,
        timestamp: u64,
    ) -> Result<SignedHeaders>;
}

#[async_trait::async_trait]
impl RequestSigner for RobinhoodSigner {
    async fn signed_headers(
        &self,
        method: &str,
        path: &str,
        body: &str,
        timestamp: u64,
    ) -> Result<SignedHeaders> {
        Ok(self.sign(method, path, body, timestamp))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};

    fn signer() -> RobinhoodSigner {
        RobinhoodSigner::new("api-key-123", SigningKey::from_bytes(&[7u8; 32]))
    }

    #[test]
    fn signature_verifies_over_canonical_message() {
        let h = signer().sign("GET", "/api/v1/crypto/trading/accounts/", "", 1_700_000_000);
        assert_eq!(h.api_key, "api-key-123");
        assert_eq!(h.timestamp, "1700000000");

        let vk: VerifyingKey = SigningKey::from_bytes(&[7u8; 32]).verifying_key();
        let msg = "api-key-1231700000000/api/v1/crypto/trading/accounts/GET";
        let sig = Signature::from_slice(&STANDARD.decode(&h.signature).unwrap()).unwrap();
        assert!(vk.verify(msg.as_bytes(), &sig).is_ok());
    }

    #[test]
    fn tampered_body_fails_verification() {
        let h = signer().sign(
            "POST",
            "/api/v1/crypto/trading/orders/",
            "{\"symbol\":\"BTC-USD\"}",
            1,
        );
        let vk = SigningKey::from_bytes(&[7u8; 32]).verifying_key();
        let tampered = "api-key-1231/api/v1/crypto/trading/orders/POST{\"symbol\":\"ETH-USD\"}";
        let sig = Signature::from_slice(&STANDARD.decode(&h.signature).unwrap()).unwrap();
        assert!(vk.verify(tampered.as_bytes(), &sig).is_err());
    }

    #[test]
    fn from_base64_key_accepts_seed_and_libsodium_key() {
        let a = RobinhoodSigner::from_base64_key("k", &STANDARD.encode([9u8; 32])).unwrap();
        let b = RobinhoodSigner::from_base64_key("k", &STANDARD.encode([9u8; 64])).unwrap();
        assert_eq!(a.public_key_b64(), b.public_key_b64());
    }
}
