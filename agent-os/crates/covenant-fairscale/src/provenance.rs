//! Provenance for a FairScore read.
//!
//! FairScale's score is off-chain and a plain read is unsigned, so on its own a
//! read cannot be proven after the fact. A [`ReadProvenance`] binds the wallet
//! queried, the endpoint, and a hash of the exact response into one
//! hash-addressable record. It does not make FairScale's number trustworthy; it
//! makes the read verifiable: that this response, for this wallet, came back
//! from this endpoint. The record rides back with the tool result for the
//! caller to store or anchor. Anchoring `digest()` into the daemon's audit
//! chain, and putting x402 read spend on the settlement feed beside Hyre's,
//! is the next increment; until then spend is bounded by the per-read caps
//! and surfaced in the settle logs only.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::tools::{PROVIDER, TRUST_LABEL};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReadProvenance {
    pub provider: String,
    pub trust: String,
    pub wallet: String,
    pub endpoint: String,
    /// SHA-256 over the JCS-canonical response, so an identical response hashes
    /// identically regardless of key order or whitespace.
    #[serde(rename = "responseSha256")]
    pub response_sha256: String,
}

impl ReadProvenance {
    pub fn new(wallet: &str, endpoint: &str, response: &Value) -> Self {
        ReadProvenance {
            provider: PROVIDER.to_string(),
            trust: TRUST_LABEL.to_string(),
            wallet: wallet.to_string(),
            endpoint: endpoint.to_string(),
            response_sha256: canonical_sha256_hex(response),
        }
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// Stable hash over the record; the identity a consumer stores or anchors.
    pub fn digest(&self) -> String {
        canonical_sha256_hex(&self.to_json())
    }
}

pub fn canonical_sha256_hex(value: &Value) -> String {
    let bytes = match serde_jcs::to_vec(value) {
        Ok(b) => b,
        Err(e) => {
            // Unreachable for finite parsed JSON (serde_json rejects NaN/Inf
            // at parse), but a silent fallback would quietly break the
            // "identical response hashes identically" guarantee, so shout.
            tracing::warn!(error = %e, "JCS canonicalization failed; hashing non-canonical JSON");
            value.to_string().into_bytes()
        }
    };
    let mut h = Sha256::new();
    h.update(&bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provenance_binds_wallet_endpoint_and_response_hash() {
        let resp = json!({ "fairscore": 65.3, "tier": "gold" });
        let p = ReadProvenance::new("wallet", "/score", &resp);
        assert_eq!(p.provider, PROVIDER);
        assert_eq!(p.trust, TRUST_LABEL);
        assert_eq!(p.wallet, "wallet");
        assert_eq!(p.response_sha256.len(), 64);
        assert!(!p.digest().is_empty());
    }

    #[test]
    fn response_hash_is_stable_across_key_order() {
        let a = ReadProvenance::new("w", "/score", &json!({ "fairscore": 1, "tier": "gold" }));
        let b = ReadProvenance::new("w", "/score", &json!({ "tier": "gold", "fairscore": 1 }));
        assert_eq!(a.response_sha256, b.response_sha256);
        assert_eq!(a.digest(), b.digest());
    }
}
