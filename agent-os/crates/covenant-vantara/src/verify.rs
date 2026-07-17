//! Verification of Vantara provenance records.
//!
//! The trustworthy proof on a record is `vantaraSignature`: an ed25519
//! signature the network places over a canonical string of the record's
//! immutable fields. The feed is self-describing — its [`SigningBlock`]
//! carries the public key, the encodings, and the canonical template — so
//! [`verify_receipt`] rebuilds the exact bytes and checks the signature with
//! nothing fetched out of band.
//!
//! `proofSignature` is a different thing: an internal node-to-orchestrator
//! MAC, verified server-side and intentionally not third-party verifiable.
//! This crate treats it as opaque and never attests over it.
//!
//! The output hash is checked structurally against the feed's declared
//! algorithm (for `sha256`, 64 lowercase hex).
//!
//! [`SigningBlock`]: crate::types::SigningBlock

use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::types::{Job, SigningBlock};

const ED25519_SIG_LEN: usize = 64;
const ED25519_KEY_LEN: usize = 32;

/// Domain-separation prefix of the canonical receipt string.
pub const RECEIPT_PREFIX: &str = "vantara-job-receipt-v1";

/// Result of checking an `outputHash` against the feed's hash algorithm.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HashCheck {
    pub algorithm: String,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Check an `outputHash` is well-formed for `algorithm`. Only `sha256` is
/// understood; any other algorithm is reported as unknown rather than
/// silently passed.
pub fn check_output_hash(algorithm: &str, hash: &str) -> HashCheck {
    let reason = match algorithm.to_ascii_lowercase().as_str() {
        "sha256" => {
            if hash.len() != 64 {
                Some(format!("expected 64 hex chars, got {}", hash.len()))
            } else if !hash
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                Some("expected lowercase hex".to_string())
            } else {
                None
            }
        }
        _ => Some(format!("unknown hash algorithm `{algorithm}`")),
    };
    HashCheck {
        algorithm: algorithm.to_string(),
        valid: reason.is_none(),
        reason,
    }
}

/// State of a record's `vantaraSignature`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SignatureStatus {
    /// The canonical string was rebuilt and the ed25519 signature holds
    /// against the feed's published key.
    Verified,
    /// A signature is present but does not check out: a decode failure, an
    /// unsupported scheme/encoding, a missing signing block, or bytes that
    /// do not match the canonical receipt.
    Invalid { reason: String },
    /// The record carries no `vantaraSignature`.
    Absent,
}

/// The canonical receipt string a `vantaraSignature` is computed over:
/// `vantara-job-receipt-v1|<jobId>|<nodeId or '-'>|<model>|<outputHash>|<completedAt>`.
/// `settlement` is deliberately excluded — it mutates when the reward settles.
pub fn canonical_receipt(job: &Job) -> String {
    let node = job.node_id.as_deref().unwrap_or("-");
    format!(
        "{RECEIPT_PREFIX}|{}|{}|{}|{}|{}",
        job.job_id, node, job.model, job.output_hash, job.completed_at
    )
}

/// Verify a job's `vantaraSignature`. Decodes the key and signature under the
/// encodings the feed's signing block declares, rebuilds the canonical receipt
/// from the record's own fields, and checks the ed25519 signature.
///
/// When `pinned_key` is set (an operator-configured key, or the one resolved
/// from the MPP discovery doc), the feed's signing key must equal it, so
/// `verified` means verified against a key sanctioned out of band, not one the
/// feed asserted about itself. With no pin, verification falls back to the
/// self-describing feed key.
pub fn verify_receipt(
    signing: Option<&SigningBlock>,
    job: &Job,
    pinned_key: Option<&str>,
) -> SignatureStatus {
    let Some(sig_encoded) = job.vantara_signature.as_deref() else {
        return SignatureStatus::Absent;
    };
    let invalid = |r: &str| SignatureStatus::Invalid {
        reason: r.to_string(),
    };
    let Some(signing) = signing else {
        return invalid("feed carries no signing block");
    };
    if !signing.scheme.eq_ignore_ascii_case("ed25519") {
        return SignatureStatus::Invalid {
            reason: format!("unsupported scheme `{}`", signing.scheme),
        };
    }
    if let Some(pinned) = pinned_key {
        if pinned != signing.public_key {
            return SignatureStatus::Invalid {
                reason: format!(
                    "feed signing key {} does not match the pinned provider key {pinned}",
                    signing.public_key
                ),
            };
        }
    }
    let Some(public_key) = decode(&signing.public_key, &signing.public_key_encoding) else {
        return invalid("public key does not decode");
    };
    let Some(signature) = decode(sig_encoded, &signing.signature_encoding) else {
        return invalid("signature does not decode");
    };
    if verify_ed25519(&public_key, canonical_receipt(job).as_bytes(), &signature) {
        SignatureStatus::Verified
    } else {
        invalid("signature does not verify against the canonical receipt")
    }
}

/// Decode `s` under a named encoding. `base58` and `base64` are understood;
/// anything else returns `None`.
fn decode(s: &str, encoding: &str) -> Option<Vec<u8>> {
    match encoding.to_ascii_lowercase().as_str() {
        "base58" => bs58::decode(s).into_vec().ok(),
        "base64" => STANDARD.decode(s).ok(),
        _ => None,
    }
}

/// Verify an ed25519 signature over `message` with `public_key`. Both
/// `public_key` (32 bytes) and `signature` (64 bytes) are raw bytes. Any
/// malformed input, or a signature that does not hold, returns `false`.
pub fn verify_ed25519(public_key: &[u8], message: &[u8], signature: &[u8]) -> bool {
    if public_key.len() != ED25519_KEY_LEN || signature.len() != ED25519_SIG_LEN {
        return false;
    }
    let key_bytes: [u8; ED25519_KEY_LEN] = public_key.try_into().expect("length checked above");
    let Ok(vk) = VerifyingKey::from_bytes(&key_bytes) else {
        return false;
    };
    let Ok(sig) = Signature::from_slice(signature) else {
        return false;
    };
    vk.verify_strict(message, &sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Job;
    use ed25519_dalek::{Signer, SigningKey};

    fn job() -> Job {
        Job {
            job_id: "8e71e03f-d994-4b09-8444-53039ca2839e".into(),
            node_id: None,
            model: "claude".into(),
            output_hash: "881f47e19aac2cdddbc30b1fa29284158ee680c1c2f84ddc73b5fa3606c9b3b5".into(),
            proof_signature: None,
            vantara_signature: None,
            completed_at: "2026-07-01T19:19:04.083Z".into(),
            settlement: None,
        }
    }

    /// A signing block over a locally-generated key, plus the job signed under
    /// it. Mirrors the live feed's block exactly (base58 key, base64 sig).
    fn signed(job: &Job) -> (SigningBlock, String) {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let sig = sk.sign(canonical_receipt(job).as_bytes());
        let block = SigningBlock {
            field: "vantaraSignature".into(),
            scheme: "ed25519".into(),
            public_key: bs58::encode(sk.verifying_key().as_bytes()).into_string(),
            public_key_encoding: "base58".into(),
            signature_encoding: "base64".into(),
            canonical: "vantara-job-receipt-v1|<jobId>|<nodeId or '-'>|<model>|<outputHash>|<completedAt ISO>".into(),
            note: None,
        };
        (block, STANDARD.encode(sig.to_bytes()))
    }

    #[test]
    fn canonical_uses_dash_for_null_node() {
        assert_eq!(
            canonical_receipt(&job()),
            "vantara-job-receipt-v1|8e71e03f-d994-4b09-8444-53039ca2839e|-|claude|\
             881f47e19aac2cdddbc30b1fa29284158ee680c1c2f84ddc73b5fa3606c9b3b5|2026-07-01T19:19:04.083Z"
        );
        let mut with_node = job();
        with_node.node_id = Some("5c2f8ab1".into());
        assert!(canonical_receipt(&with_node).contains("|5c2f8ab1|"));
    }

    #[test]
    fn verifies_a_real_receipt_signature() {
        let mut j = job();
        let (block, sig) = signed(&j);
        j.vantara_signature = Some(sig);
        assert_eq!(
            verify_receipt(Some(&block), &j, None),
            SignatureStatus::Verified
        );
    }

    #[test]
    fn pinned_matching_key_verifies() {
        let mut j = job();
        let (block, sig) = signed(&j);
        j.vantara_signature = Some(sig);
        let pin = block.public_key.clone();
        assert_eq!(
            verify_receipt(Some(&block), &j, Some(&pin)),
            SignatureStatus::Verified
        );
    }

    #[test]
    fn pinned_mismatched_key_is_invalid() {
        let mut j = job();
        let (block, sig) = signed(&j);
        j.vantara_signature = Some(sig);
        // A valid signature under the feed's key, but the operator pinned a
        // different provider key: refuse it before touching the crypto.
        match verify_receipt(Some(&block), &j, Some("11111111111111111111111111111111")) {
            SignatureStatus::Invalid { reason } => assert!(reason.contains("does not match")),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn rejects_a_tampered_field() {
        let mut j = job();
        let (block, sig) = signed(&j);
        j.vantara_signature = Some(sig);
        j.model = "hermes".into(); // canonical no longer matches the signed bytes
        assert!(matches!(
            verify_receipt(Some(&block), &j, None),
            SignatureStatus::Invalid { .. }
        ));
    }

    #[test]
    fn absent_signature_and_missing_block() {
        assert_eq!(verify_receipt(None, &job(), None), SignatureStatus::Absent);
        let mut j = job();
        j.vantara_signature = Some("AAAA".into());
        assert!(matches!(
            verify_receipt(None, &j, None),
            SignatureStatus::Invalid { .. }
        ));
    }

    #[test]
    fn rejects_unsupported_scheme_and_bad_encodings() {
        let mut j = job();
        let (mut block, sig) = signed(&j);
        j.vantara_signature = Some(sig);
        block.scheme = "secp256k1".into();
        assert!(matches!(
            verify_receipt(Some(&block), &j, None),
            SignatureStatus::Invalid { .. }
        ));
        let (mut block2, _) = signed(&j);
        block2.public_key = "not base58 !!!".into();
        assert!(matches!(
            verify_receipt(Some(&block2), &j, None),
            SignatureStatus::Invalid { .. }
        ));
    }

    #[test]
    fn sha256_hash_must_be_64_lowercase_hex() {
        assert!(check_output_hash("sha256", &"a".repeat(64)).valid);
        assert!(check_output_hash("sha256", "abc")
            .reason
            .unwrap()
            .contains("64"));
        assert!(check_output_hash("sha256", &"A".repeat(64))
            .reason
            .unwrap()
            .contains("lowercase"));
    }

    #[test]
    fn algorithm_match_tolerates_casing_but_flags_unknown() {
        assert!(check_output_hash("SHA256", &"a".repeat(64)).valid);
        let c = check_output_hash("blake3", &"a".repeat(64));
        assert!(!c.valid);
        assert!(c.reason.unwrap().contains("unknown"));
    }

    #[test]
    fn ed25519_verifies_and_rejects_tampering() {
        let sk = SigningKey::from_bytes(&[3u8; 32]);
        let vk = sk.verifying_key();
        let msg = b"covenant";
        let sig = sk.sign(msg);
        assert!(verify_ed25519(vk.as_bytes(), msg, &sig.to_bytes()));
        assert!(!verify_ed25519(vk.as_bytes(), b"other", &sig.to_bytes()));
        assert!(!verify_ed25519(&[0u8; 31], msg, &sig.to_bytes()));
    }
}
