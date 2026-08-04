//! `covenant.myrad.signal.v1` — the provenance receipt that travels with a
//! Myrad signal.
//!
//! Myrad's pipeline proves the source data is real: a Reclaim proof, generated
//! on the contributor's device, over the contributor's own account. What no
//! party can currently check is the step after that, where verified
//! contributions become the aggregate a buyer pays for. The receipt covers that
//! step. It binds, in one signed object:
//!
//! - **what was delivered** — a digest of the exact bytes the buyer received,
//! - **what it came from** — a Merkle root over the contributing set,
//! - **how many, and how distinct** — the contributor count and the checks that
//!   back it,
//! - **when** — the emission range the contributions actually cover,
//! - **what is soft about it** — every integrity finding, warnings included.
//!
//! A receipt that hid its warnings would be worth less than no receipt, so
//! [`IntegrityReport`] travels whole. The signature is over the RFC 8785
//! canonical form, so a buyer recomputes the digest from the bytes they hold and
//! checks it against the published attestor key. Anchoring the digest on Solana
//! is the daemon's step, the same path the other Covenant receipt kinds take,
//! and it lands in [`SignedReceipt::anchor`] outside the signed bytes.
//!
//! No field here carries behavior data, an account identifier, or anything that
//! describes a person. The receipt is derivable entirely from provenance
//! metadata and hashes.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signer as _, SigningKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::evidence::{Contribution, MerkleTree, CONTRIBUTION_SCHEMA};
use crate::integrity::{evaluate, IntegrityPolicy, IntegrityReport, Status};
use crate::signal::SignalRecord;

pub const SCHEMA: &str = "covenant.myrad.signal.v1";

/// What was sold, in Myrad's own vocabulary, plus the digest that pins it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalDescriptor {
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dataset_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_standard: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<String>,
    pub cohort_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment_id: Option<String>,
    /// sha256 over the RFC 8785 canonical form of the delivered artifact.
    pub delivered_sha256: String,
}

/// The contributing set, as commitments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub contributors: usize,
    pub merkle_root: String,
    /// Schema of the leaf the root is built over.
    pub leaf_schema: String,
    /// How a leaf is derived, so a verifier can rebuild one without reading this
    /// crate.
    pub commitment_scheme: String,
}

/// The time the contributions actually cover, measured rather than declared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Freshness {
    /// Earliest and latest emission time across the contributing set.
    pub generated_from: String,
    pub generated_to: String,
    /// The retention window the payloads declare, where they agree on one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_days_claimed: Option<u64>,
    /// Months of activity the set actually spans.
    pub observed_span_months: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalReceipt {
    pub schema: String,
    pub signal: SignalDescriptor,
    pub evidence: Evidence,
    pub freshness: Freshness,
    pub integrity: IntegrityReport,
    pub issued_at: u64,
}

impl SignalReceipt {
    /// Build a receipt for `delivered`, backed by `records`.
    ///
    /// The contributing set must be one cohort from one provider: a receipt that
    /// averaged two cohorts under one label would be the exact ambiguity it
    /// exists to remove.
    pub fn build(
        delivered: &Value,
        records: &[SignalRecord],
        policy: IntegrityPolicy,
    ) -> Result<Self> {
        let first = records
            .first()
            .ok_or_else(|| Error::Signal("no contributions".into()))?;

        let cohort_id = first
            .cohort_id()
            .ok_or_else(|| Error::Signal("contribution carries no cohort_id".into()))?
            .to_string();
        if records.iter().any(|r| r.cohort_id() != Some(&cohort_id)) {
            return Err(Error::Signal(
                "contributions span more than one cohort_id".into(),
            ));
        }
        if records.iter().any(|r| r.provider != first.provider) {
            return Err(Error::Signal(
                "contributions span more than one provider".into(),
            ));
        }

        let commitments = records
            .iter()
            .map(|r| Contribution::from_record(r)?.commitment_hex())
            .collect::<Result<Vec<_>>>()?;
        let tree = MerkleTree::build(&commitments)?;

        let delivered_sha256 = {
            let canon =
                serde_jcs::to_string(delivered).map_err(|e| Error::Decode(e.to_string()))?;
            hex::encode(Sha256::digest(canon.as_bytes()))
        };

        Ok(Self {
            schema: SCHEMA.to_string(),
            signal: SignalDescriptor {
                provider: first.provider.clone(),
                dataset_id: first.dataset_id().map(str::to_string),
                record_type: first.record_type().map(str::to_string),
                schema_standard: first.schema_standard().map(str::to_string),
                schema_version: first.schema_version().map(str::to_string),
                cohort_id,
                segment_id: first.segment_id().map(str::to_string),
                delivered_sha256,
            },
            evidence: Evidence {
                contributors: tree.len(),
                merkle_root: tree.root_hex(),
                leaf_schema: CONTRIBUTION_SCHEMA.to_string(),
                commitment_scheme: "sha256(rfc8785(contribution))".to_string(),
            },
            freshness: freshness(records),
            integrity: evaluate(records, policy),
            issued_at: crate::unix_now(),
        })
    }

    /// Whether the delivered bytes are the ones this receipt was issued over.
    pub fn covers(&self, delivered: &Value) -> bool {
        serde_jcs::to_string(delivered)
            .map(|canon| {
                hex::encode(Sha256::digest(canon.as_bytes())) == self.signal.delivered_sha256
            })
            .unwrap_or(false)
    }

    pub fn canonical_json(&self) -> Result<String> {
        serde_jcs::to_string(self).map_err(|e| Error::Decode(e.to_string()))
    }

    pub fn root_hash_hex(&self) -> Result<String> {
        Ok(hex::encode(Sha256::digest(
            self.canonical_json()?.as_bytes(),
        )))
    }

    /// Sign with a Covenant attestor key.
    ///
    /// A failing cohort still gets a receipt. Refusing to sign one would leave
    /// the buyer with nothing to check and the failure invisible; the receipt
    /// carries [`Status::Fail`] and the reasons, and the sale decision is the
    /// seller's to make on top of it.
    pub fn attest(&self, attestor: &SigningKey) -> Result<SignedReceipt> {
        let root_hash_hex = self.root_hash_hex()?;
        let sig = attestor.sign(root_hash_hex.as_bytes());
        Ok(SignedReceipt {
            receipt: self.clone(),
            root_hash_hex,
            attestor_pubkey_b64: STANDARD.encode(attestor.verifying_key().to_bytes()),
            signature_b64: STANDARD.encode(sig.to_bytes()),
            anchor: None,
        })
    }

    pub fn sellable(&self) -> bool {
        self.integrity.status != Status::Fail
    }
}

fn freshness(records: &[SignalRecord]) -> Freshness {
    let mut times: Vec<&str> = records.iter().filter_map(|r| r.generated_at()).collect();
    times.sort_unstable();

    let mut months: Vec<String> = records.iter().flat_map(|r| r.activity_months()).collect();
    months.sort();
    months.dedup();
    let span = match (months.first(), months.last()) {
        (Some(a), Some(b)) => month_span(a, b),
        _ => 0,
    };

    let claimed: Vec<u64> = {
        let mut v: Vec<u64> = records
            .iter()
            .filter_map(|r| r.window_days_claimed())
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    };

    Freshness {
        generated_from: times.first().copied().unwrap_or_default().to_string(),
        generated_to: times.last().copied().unwrap_or_default().to_string(),
        window_days_claimed: (claimed.len() == 1).then(|| claimed[0]),
        observed_span_months: span,
    }
}

fn month_span(first: &str, last: &str) -> u32 {
    let parse = |s: &str| -> Option<i32> {
        let (y, m) = s.get(..7)?.split_once('-')?;
        Some(y.parse::<i32>().ok()? * 12 + m.parse::<i32>().ok()?)
    };
    match (parse(first), parse(last)) {
        (Some(a), Some(b)) if b >= a => (b - a + 1) as u32,
        _ => 0,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedReceipt {
    pub receipt: SignalReceipt,
    pub root_hash_hex: String,
    pub attestor_pubkey_b64: String,
    pub signature_b64: String,
    /// On-chain anchor reference, set once anchored. Sits outside the signed
    /// bytes, so it never affects [`Self::verify`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
}

impl SignedReceipt {
    pub fn verify(&self) -> bool {
        use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};

        let Ok(recomputed) = self.receipt.root_hash_hex() else {
            return false;
        };
        if recomputed != self.root_hash_hex {
            return false;
        }
        let (Ok(key_bytes), Ok(sig_bytes)) = (
            STANDARD.decode(&self.attestor_pubkey_b64),
            STANDARD.decode(&self.signature_b64),
        ) else {
            return false;
        };
        let (Ok(key_bytes), Ok(sig_bytes)): (
            std::result::Result<[u8; 32], _>,
            std::result::Result<[u8; 64], _>,
        ) = (key_bytes.try_into(), sig_bytes.try_into()) else {
            return false;
        };
        let Ok(key) = VerifyingKey::from_bytes(&key_bytes) else {
            return false;
        };
        key.verify(
            self.root_hash_hex.as_bytes(),
            &Signature::from_bytes(&sig_bytes),
        )
        .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(cohort: &str, subject: &str, proof: &str) -> SignalRecord {
        let value = json!({
            "provider": "netflix",
            "reclaim_proof_id": proof,
            "verification_status": "verified",
            "signal": {
                "dataset_id": "myrad_netflix_v1",
                "record_type": "streaming_behavior_intelligence",
                "schema_version": "2.0",
                "generated_at": "2026-05-19T17:40:20.697Z",
                "metadata": {
                    "schema_standard": "myrad_streaming_intelligence_v2",
                    "privacy_compliance": { "cohort_id": cohort, "pii_stripped": true }
                },
                "user_profile": { "total_titles_watched": 40 },
                "viewing_summary": { "data_window_days": 90, "total_titles_watched": 40 },
                "viewing_behavior": { "monthly_pattern": { "2026-04": 3, "2026-05": 7 } },
                "audience_segment": { "segment_id": "seg-1" }
            }
        });
        SignalRecord::from_sample(&value, Some(subject.to_string()), false).unwrap()
    }

    fn cohort(n: usize) -> Vec<SignalRecord> {
        (0..n)
            .map(|i| {
                record(
                    "netflix_drama",
                    &format!("subject-{i}"),
                    &format!("{:024x}", i + 1),
                )
            })
            .collect()
    }

    fn delivered() -> Value {
        json!({ "cohort_id": "netflix_drama", "binge_score_mean": 61.4, "contributors": 5 })
    }

    #[test]
    fn builds_and_verifies() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let receipt =
            SignalReceipt::build(&delivered(), &cohort(5), IntegrityPolicy::default()).unwrap();

        assert_eq!(receipt.schema, SCHEMA);
        assert_eq!(receipt.evidence.contributors, 5);
        assert_eq!(receipt.signal.cohort_id, "netflix_drama");
        assert_eq!(receipt.freshness.observed_span_months, 2);
        assert_eq!(receipt.freshness.window_days_claimed, Some(90));
        assert!(receipt.sellable());

        let signed = receipt.attest(&key).unwrap();
        assert!(signed.verify());
    }

    #[test]
    fn a_tampered_receipt_fails_verification() {
        let key = SigningKey::from_bytes(&[9u8; 32]);
        let mut signed = SignalReceipt::build(&delivered(), &cohort(5), IntegrityPolicy::default())
            .unwrap()
            .attest(&key)
            .unwrap();
        assert!(signed.verify());

        signed.receipt.evidence.contributors = 500;
        assert!(
            !signed.verify(),
            "inflating the contributor count must break the signature"
        );
    }

    #[test]
    fn an_anchor_added_after_signing_does_not_break_verification() {
        let key = SigningKey::from_bytes(&[3u8; 32]);
        let mut signed = SignalReceipt::build(&delivered(), &cohort(5), IntegrityPolicy::default())
            .unwrap()
            .attest(&key)
            .unwrap();
        signed.anchor = Some("5jQ1Pt33…".into());
        assert!(signed.verify());
    }

    #[test]
    fn covers_the_bytes_it_was_issued_over() {
        let receipt =
            SignalReceipt::build(&delivered(), &cohort(5), IntegrityPolicy::default()).unwrap();
        assert!(receipt.covers(&delivered()));

        let mut altered = delivered();
        altered["binge_score_mean"] = json!(99.9);
        assert!(
            !receipt.covers(&altered),
            "a changed aggregate must not match"
        );
    }

    #[test]
    fn key_order_in_the_delivered_artifact_does_not_change_the_digest() {
        let receipt =
            SignalReceipt::build(&delivered(), &cohort(5), IntegrityPolicy::default()).unwrap();
        let reordered: Value = serde_json::from_str(
            r#"{"contributors":5,"binge_score_mean":61.4,"cohort_id":"netflix_drama"}"#,
        )
        .unwrap();
        assert!(receipt.covers(&reordered));
    }

    #[test]
    fn mixed_cohorts_are_refused() {
        let mut records = cohort(4);
        records.push(record("netflix_other", "subject-9", &format!("{:024x}", 9)));
        let err = SignalReceipt::build(&delivered(), &records, IntegrityPolicy::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("more than one cohort_id"), "{err}");
    }

    #[test]
    fn a_failing_cohort_still_gets_a_signed_receipt_that_says_so() {
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let receipt =
            SignalReceipt::build(&delivered(), &cohort(2), IntegrityPolicy::default()).unwrap();
        assert!(!receipt.sellable());
        assert_eq!(receipt.integrity.status, Status::Fail);
        assert!(receipt.attest(&key).unwrap().verify());
    }

    #[test]
    fn the_receipt_carries_no_payload_content() {
        let receipt =
            SignalReceipt::build(&delivered(), &cohort(5), IntegrityPolicy::default()).unwrap();
        let canon = receipt.canonical_json().unwrap();
        for leaked in [
            "monthly_pattern",
            "total_titles_watched",
            "profile_name_initial",
        ] {
            assert!(!canon.contains(leaked), "receipt leaked {leaked}");
        }
    }
}
