//! Envelope around a `gitlawb/ref-update/v1` cert plus optional attestations,
//! and the cert-hash helper attestations bind to.
//!
//! An empty envelope serializes to the same bytes as the underlying bare cert.
//! `cert_hash` strips `signatures` and `attestations`, JCS-encodes the rest,
//! and SHA-256s; the result is what every attestation binds to.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::attestation::Attestation;
use crate::error::Result;

/// A ref-update cert with optional attestations.
///
/// The cert is flattened into the top-level JSON, so an empty envelope
/// produces the same bytes as a bare cert and existing decoders ignore the
/// extra `attestations` field. The cert is held as `serde_json::Value` so
/// this crate has no dependency on `gitlawb-core::cert::RefUpdateCert`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttestedRefUpdateCert {
    /// The cert body and signatures, flattened to the top level.
    #[serde(flatten)]
    pub cert: serde_json::Value,

    /// Attached attestations. Empty when absent on the wire.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attestations: Vec<Attestation>,
}

impl AttestedRefUpdateCert {
    /// Wrap a cert. No attestations are attached yet.
    pub fn from_cert<T: Serialize>(cert: &T) -> Result<Self> {
        Ok(Self {
            cert: serde_json::to_value(cert)?,
            attestations: Vec::new(),
        })
    }

    /// The 32-byte cert hash every attached attestation must bind to.
    pub fn cert_hash(&self) -> Result<[u8; 32]> {
        cert_hash(&self.cert)
    }

    /// Append an already-signed attestation.
    pub fn attach(&mut self, attestation: Attestation) {
        self.attestations.push(attestation);
    }
}

/// SHA-256 of the cert body. Strips `signatures` and `attestations`, then
/// JCS-encodes the remaining object (RFC 8785). JCS pins key order, number
/// formatting, and string escaping, so the hash is reproducible across
/// languages and JSON libraries.
pub fn cert_hash(cert: &serde_json::Value) -> Result<[u8; 32]> {
    let mut body = cert.clone();
    if let Some(obj) = body.as_object_mut() {
        obj.remove("signatures");
        obj.remove("attestations");
    }
    let bytes = serde_jcs::to_vec(&body)
        .map_err(|e| crate::error::AttestError::Payload(format!("JCS encode: {e}")))?;
    let mut h = Sha256::new();
    h.update(&bytes);
    let out = h.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    Ok(arr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_envelope_serializes_as_bare_cert() {
        let bare = json!({
            "type": "gitlawb/ref-update/v1",
            "repo": "did:key:z6MkRepo",
            "ref_name": "refs/heads/main",
            "from": "0".repeat(64),
            "to": "a".repeat(64),
            "seq": 1,
            "timestamp": "2026-01-01T00:00:00Z",
            "nonce": "n1",
            "signatures": [{"signer": "did:key:z6MkRepo", "sig": "abc"}]
        });

        let env = AttestedRefUpdateCert::from_cert(&bare).unwrap();
        let env_json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();

        let bare_keys: std::collections::BTreeSet<&String> =
            bare.as_object().unwrap().keys().collect();
        let env_keys: std::collections::BTreeSet<&String> =
            env_json.as_object().unwrap().keys().collect();
        assert_eq!(bare_keys, env_keys);
    }

    #[test]
    fn bare_cert_parses_as_envelope_with_no_attestations() {
        let bare = json!({
            "type": "gitlawb/ref-update/v1",
            "repo": "did:key:z6MkRepo",
            "ref_name": "refs/heads/main",
            "from": "0".repeat(64),
            "to": "a".repeat(64),
            "seq": 1,
            "timestamp": "2026-01-01T00:00:00Z",
            "nonce": "n1",
            "signatures": [{"signer": "did:key:z6MkRepo", "sig": "abc"}]
        });
        let env: AttestedRefUpdateCert = serde_json::from_value(bare.clone()).unwrap();
        assert!(env.attestations.is_empty());
        assert_eq!(env.cert, bare);
    }

    #[test]
    fn cert_hash_is_stable_across_countersignatures() {
        let base_body = json!({
            "type": "gitlawb/ref-update/v1",
            "repo": "did:key:z6MkRepo",
            "ref_name": "refs/heads/main",
            "from": "0".repeat(64),
            "to": "a".repeat(64),
            "seq": 1,
            "timestamp": "2026-01-01T00:00:00Z",
            "nonce": "n1"
        });

        let mut one_sig = base_body.clone();
        one_sig["signatures"] = json!([{"signer": "x", "sig": "y"}]);

        let mut two_sigs = base_body.clone();
        two_sigs["signatures"] = json!([
            {"signer": "x", "sig": "y"},
            {"signer": "a", "sig": "b"}
        ]);

        assert_eq!(cert_hash(&one_sig).unwrap(), cert_hash(&two_sigs).unwrap());
    }

    #[test]
    fn cert_hash_changes_when_body_changes() {
        let body_a = json!({
            "type": "gitlawb/ref-update/v1",
            "repo": "did:key:z6MkRepo",
            "ref_name": "refs/heads/main",
            "from": "0".repeat(64),
            "to": "a".repeat(64),
            "seq": 1,
            "timestamp": "2026-01-01T00:00:00Z",
            "nonce": "n1",
            "signatures": []
        });
        let mut body_b = body_a.clone();
        body_b["to"] = json!("b".repeat(64));

        assert_ne!(cert_hash(&body_a).unwrap(), cert_hash(&body_b).unwrap());
    }
}
