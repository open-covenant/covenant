//! Content-free attestations over Vantara jobs.
//!
//! A [`VantaraAttestation`] is Covenant's view of one job: the cluster, the
//! job and node ids, the model, the output hash and its algorithm, the state
//! of the network receipt signature, and whether the reward settled on-chain.
//! Given a job and the feed's signing block it is deterministic, so the same
//! record always attests to the same object, safe to hash, log, or anchor.
//!
//! Nothing here carries a prompt, an output, or a wallet. The output is
//! present only as its hash; settlement is a bare flag with no amount,
//! signature, or wallet.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::types::{Job, Settlement, SigningBlock};
use crate::verify::{check_output_hash, verify_receipt, HashCheck, SignatureStatus};

/// Provider tag carried on every Vantara attestation.
pub const PROVIDER: &str = "vantara";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VantaraAttestation {
    /// Always `"vantara"`.
    pub provider: String,
    pub cluster: String,
    pub job_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    pub model: String,
    pub output_hash: String,
    pub hash: HashCheck,
    /// Verification of the record's `vantaraSignature` against the feed key.
    pub signature: SignatureStatus,
    /// Content-free on-chain payment anchor, when the feed reports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settlement: Option<Settlement>,
    pub completed_at: String,
}

impl VantaraAttestation {
    /// Build an attestation from a job, the feed-level cluster and hash
    /// algorithm, and the feed's signing block (needed to verify the receipt).
    pub fn from_job(
        cluster: &str,
        hash_algorithm: &str,
        signing: Option<&SigningBlock>,
        job: &Job,
    ) -> Self {
        Self {
            provider: PROVIDER.to_string(),
            cluster: cluster.to_string(),
            job_id: job.job_id.clone(),
            node_id: job.node_id.clone(),
            model: job.model.clone(),
            output_hash: job.output_hash.clone(),
            hash: check_output_hash(hash_algorithm, &job.output_hash),
            signature: verify_receipt(signing, job),
            settlement: job.settlement.clone(),
            completed_at: job.completed_at.clone(),
        }
    }

    /// Cryptographically verified: a well-formed output hash and a network
    /// receipt signature that holds.
    pub fn is_verified(&self) -> bool {
        self.hash.valid && self.signature == SignatureStatus::Verified
    }

    /// Whether the job's reward has settled on-chain, per the feed.
    pub fn is_settled(&self) -> bool {
        self.settlement.as_ref().is_some_and(|s| s.settled)
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

/// Lowercase hex of the SHA-256 of `bytes`. Used to hash an agent's own
/// output down to the anchor the feed joins on, without the output ever
/// leaving the host.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::canonical_receipt;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
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
            settlement: Some(Settlement {
                settled: false,
                settled_at: None,
            }),
        }
    }

    fn sign(job: &mut Job) -> SigningBlock {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let sig = sk.sign(canonical_receipt(job).as_bytes());
        job.vantara_signature = Some(STANDARD.encode(sig.to_bytes()));
        SigningBlock {
            field: "vantaraSignature".into(),
            scheme: "ed25519".into(),
            public_key: bs58::encode(sk.verifying_key().as_bytes()).into_string(),
            public_key_encoding: "base58".into(),
            signature_encoding: "base64".into(),
            canonical: "vantara-job-receipt-v1|...".into(),
            note: None,
        }
    }

    #[test]
    fn verified_attestation_is_content_free() {
        let mut j = job();
        let block = sign(&mut j);
        let a = VantaraAttestation::from_job("mainnet-beta", "sha256", Some(&block), &j);
        assert_eq!(a.provider, "vantara");
        assert!(a.hash.valid);
        assert_eq!(a.signature, SignatureStatus::Verified);
        assert!(a.is_verified());
        assert!(!a.is_settled());

        let v = a.to_json();
        assert!(v.get("nodeId").is_none(), "null node id must be omitted");
        let s = serde_json::to_string(&v).unwrap();
        assert!(
            !s.contains("prompt") && !s.contains("wallet") && !s.contains("amount"),
            "no content or payment detail leaks"
        );
    }

    #[test]
    fn attestation_is_deterministic_for_a_fixed_record() {
        let mut j = job();
        let block = sign(&mut j);
        let a = VantaraAttestation::from_job("mainnet-beta", "sha256", Some(&block), &j);
        let b = VantaraAttestation::from_job("mainnet-beta", "sha256", Some(&block), &j);
        assert_eq!(a, b);
    }

    #[test]
    fn unsigned_record_is_not_verified() {
        let a = VantaraAttestation::from_job("mainnet-beta", "sha256", None, &job());
        assert_eq!(a.signature, SignatureStatus::Absent);
        assert!(!a.is_verified());
    }

    #[test]
    fn settled_flag_reads_through() {
        let mut j = job();
        j.settlement = Some(Settlement {
            settled: true,
            settled_at: Some("2026-07-02T08:00:00.000Z".into()),
        });
        let block = sign(&mut j);
        let a = VantaraAttestation::from_job("mainnet-beta", "sha256", Some(&block), &j);
        assert!(a.is_settled());
    }

    #[test]
    fn sha256_hex_is_64_lowercase_hex() {
        let h = sha256_hex(b"covenant");
        assert_eq!(h.len(), 64);
        assert!(h
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    }
}
