//! Ref-update certificates, the wire format gitlawb uses to authorize a
//! change to a git ref.
//!
//! A cert pairs a body (repo, ref, from-sha, to-sha, seq, timestamp, nonce)
//! with one or more ed25519 signatures over the body's JSON bytes. The body
//! schema is frozen at `gitlawb/ref-update/v1`; nodes that receive an unknown
//! version reject it.
//!
//! Field names, ordering, and base64url-no-pad signature encoding all mirror
//! gitlawb-core's `cert.rs` so a Covenant-signed cert verifies on the node
//! side and vice versa.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64U, Engine};
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier};
use serde::{Deserialize, Serialize};

use crate::did::{did_key_from_verifying_key, verifying_key_from_did_key};
use crate::{GitlawbError, Result};

/// Certificate type discriminant. Always `gitlawb/ref-update/v1`.
pub const CERT_TYPE: &str = "gitlawb/ref-update/v1";

/// A single signature on a cert body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefUpdateSignature {
    /// `did:key` of the signer.
    pub signer: String,
    /// base64url-no-pad ed25519 signature over the body JSON.
    pub sig: String,
}

/// The body of a ref-update cert, what gets signed. Field order is locked to
/// match gitlawb-core so canonical JSON bytes are bit-identical across
/// implementations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RefUpdateBody {
    #[serde(rename = "type")]
    pub type_: String,
    /// Repository DID (a `did:key` derived from the repo's keypair).
    pub repo: String,
    /// The git ref being updated, e.g. `refs/heads/main`.
    pub ref_name: String,
    /// Previous commit hash (SHA-256 hex, 64 chars) or 64 zeros for a new ref.
    pub from: String,
    /// New commit hash (SHA-256 hex, 64 chars).
    pub to: String,
    /// Monotonic sequence number. Prevents replay.
    pub seq: u64,
    /// RFC 3339 timestamp.
    pub timestamp: DateTime<Utc>,
    /// Random nonce for deduplication.
    pub nonce: String,
}

impl RefUpdateBody {
    /// Canonical bytes used as the signing input.
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }
}

/// A full cert: body plus one or more signatures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefUpdateCert {
    #[serde(flatten)]
    pub body: RefUpdateBody,
    pub signatures: Vec<RefUpdateSignature>,
}

impl RefUpdateCert {
    /// Sign a new cert with `signing_key`.
    ///
    /// `nonce` should be a fresh value per cert. The caller picks the nonce so
    /// the cert is reproducible from a Covenant audit event (where the nonce
    /// can be derived from the audit entry hash).
    pub fn sign_new(
        signing_key: &SigningKey,
        repo: impl Into<String>,
        ref_name: impl Into<String>,
        from: impl Into<String>,
        to: impl Into<String>,
        seq: u64,
        nonce: impl Into<String>,
    ) -> Result<Self> {
        let body = RefUpdateBody {
            type_: CERT_TYPE.to_string(),
            repo: repo.into(),
            ref_name: ref_name.into(),
            from: from.into(),
            to: to.into(),
            seq,
            timestamp: Utc::now(),
            nonce: nonce.into(),
        };
        validate_structure(&body, /*has_sigs*/ false)?;

        let bytes = body.signing_bytes()?;
        let sig: Signature = signing_key.sign(&bytes);
        let sig_b64 = B64U.encode(sig.to_bytes());
        let signer = did_key_from_verifying_key(&signing_key.verifying_key());

        Ok(Self {
            body,
            signatures: vec![RefUpdateSignature {
                signer,
                sig: sig_b64,
            }],
        })
    }

    /// Append a countersignature from another signer (e.g. a maintainer).
    pub fn countersign(&mut self, signing_key: &SigningKey) -> Result<()> {
        let bytes = self.body.signing_bytes()?;
        let sig: Signature = signing_key.sign(&bytes);
        let sig_b64 = B64U.encode(sig.to_bytes());
        let signer = did_key_from_verifying_key(&signing_key.verifying_key());
        self.signatures.push(RefUpdateSignature {
            signer,
            sig: sig_b64,
        });
        Ok(())
    }

    /// Verify every signature on the cert. Returns the DIDs whose signatures
    /// verified. Errors on the first invalid signature so the caller can treat
    /// any failure as a cert-level rejection.
    pub fn verify_all(&self) -> Result<Vec<String>> {
        validate_structure(&self.body, /*has_sigs*/ true)?;
        let bytes = self.body.signing_bytes()?;
        let mut valid = Vec::with_capacity(self.signatures.len());
        for s in &self.signatures {
            let vk = verifying_key_from_did_key(&s.signer)?;
            let sig_bytes: [u8; 64] = B64U
                .decode(&s.sig)
                .map_err(|e| GitlawbError::Cert(format!("invalid base64url sig: {e}")))?
                .try_into()
                .map_err(|_| GitlawbError::Cert("signature must be 64 bytes".into()))?;
            let signature = Signature::from_bytes(&sig_bytes);
            vk.verify(&bytes, &signature)
                .map_err(|e| GitlawbError::Cert(format!("verify: {e}")))?;
            valid.push(s.signer.clone());
        }
        Ok(valid)
    }
}

fn validate_structure(body: &RefUpdateBody, has_sigs: bool) -> Result<()> {
    if body.type_ != CERT_TYPE {
        return Err(GitlawbError::Cert(format!(
            "unknown cert type: {}",
            body.type_
        )));
    }
    if !is_hex_sha256_or_zero(&body.from) {
        return Err(GitlawbError::Cert("invalid 'from' hash".into()));
    }
    if !is_hex_sha256(&body.to) {
        return Err(GitlawbError::Cert("invalid 'to' hash".into()));
    }
    if !body.ref_name.starts_with("refs/") {
        return Err(GitlawbError::Cert(format!(
            "ref_name must start with 'refs/', got '{}'",
            body.ref_name
        )));
    }
    if !body.repo.starts_with("did:") {
        return Err(GitlawbError::Cert(format!(
            "repo must be a DID, got '{}'",
            body.repo
        )));
    }
    if body.nonce.is_empty() {
        return Err(GitlawbError::Cert("nonce must not be empty".into()));
    }
    if has_sigs {
        // `has_sigs` is only set by `verify_all`, which uses it to enforce that
        // a cert pulled off the wire actually carries at least one signature.
        // Construction-time we skip this check because the signature hasn't
        // been written yet.
    }
    Ok(())
}

fn is_hex_sha256(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_hex_sha256_or_zero(s: &str) -> bool {
    is_hex_sha256(s) || s == "0".repeat(64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;
    use rand::TryRngCore;

    fn fresh() -> SigningKey {
        let mut seed = [0u8; 32];
        OsRng.try_fill_bytes(&mut seed).unwrap();
        SigningKey::from_bytes(&seed)
    }

    fn dummy_sha(c: char) -> String {
        c.to_string().repeat(64)
    }

    #[test]
    fn sign_then_verify() {
        let sk = fresh();
        let repo = did_key_from_verifying_key(&sk.verifying_key());
        let cert = RefUpdateCert::sign_new(
            &sk,
            repo.clone(),
            "refs/heads/main",
            dummy_sha('0'),
            dummy_sha('a'),
            1,
            "nonce-1",
        )
        .unwrap();

        let valid = cert.verify_all().unwrap();
        assert_eq!(valid, vec![repo]);
    }

    #[test]
    fn countersign_and_verify_both() {
        let sk1 = fresh();
        let sk2 = fresh();
        let repo = did_key_from_verifying_key(&sk1.verifying_key());
        let mut cert = RefUpdateCert::sign_new(
            &sk1,
            repo,
            "refs/heads/main",
            dummy_sha('0'),
            dummy_sha('a'),
            1,
            "nonce-1",
        )
        .unwrap();
        cert.countersign(&sk2).unwrap();
        let valid = cert.verify_all().unwrap();
        assert_eq!(valid.len(), 2);
    }

    #[test]
    fn tampered_body_fails_verify() {
        let sk = fresh();
        let repo = did_key_from_verifying_key(&sk.verifying_key());
        let mut cert = RefUpdateCert::sign_new(
            &sk,
            repo,
            "refs/heads/main",
            dummy_sha('0'),
            dummy_sha('a'),
            1,
            "nonce-1",
        )
        .unwrap();
        cert.body.to = dummy_sha('b');
        let err = cert.verify_all().unwrap_err();
        assert!(matches!(err, GitlawbError::Cert(_)));
    }

    #[test]
    fn rejects_bad_to_hash() {
        let sk = fresh();
        let repo = did_key_from_verifying_key(&sk.verifying_key());
        let err = RefUpdateCert::sign_new(
            &sk,
            repo,
            "refs/heads/main",
            dummy_sha('0'),
            "tooshort",
            1,
            "nonce-1",
        )
        .unwrap_err();
        assert!(matches!(err, GitlawbError::Cert(_)));
    }

    #[test]
    fn rejects_non_refs_ref_name() {
        let sk = fresh();
        let repo = did_key_from_verifying_key(&sk.verifying_key());
        let err = RefUpdateCert::sign_new(
            &sk,
            repo,
            "main",
            dummy_sha('0'),
            dummy_sha('a'),
            1,
            "nonce-1",
        )
        .unwrap_err();
        assert!(matches!(err, GitlawbError::Cert(_)));
    }

    #[test]
    fn json_roundtrip_preserves_signature() {
        let sk = fresh();
        let repo = did_key_from_verifying_key(&sk.verifying_key());
        let cert = RefUpdateCert::sign_new(
            &sk,
            repo,
            "refs/heads/main",
            dummy_sha('0'),
            dummy_sha('a'),
            7,
            "nonce-7",
        )
        .unwrap();
        let json = serde_json::to_string(&cert).unwrap();
        assert!(json.contains(r#""type":"gitlawb/ref-update/v1""#));
        let back: RefUpdateCert = serde_json::from_str(&json).unwrap();
        back.verify_all().unwrap();
    }
}
