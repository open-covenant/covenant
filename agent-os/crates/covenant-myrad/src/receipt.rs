//! `covenant.myrad.signal.v1`: the provenance receipt that travels with a
//! Myrad signal.
//!
//! Myrad's pipeline proves the source data is real: a Reclaim proof, generated
//! on the contributor's device, over the contributor's own account. What no
//! party can currently check is the step after that, where verified
//! contributions become the aggregate a buyer pays for. The receipt covers that
//! step. One signed object holds the digest of the delivered bytes, a Merkle
//! root over the contributing set, the contributor count, the emission range
//! those contributions cover, and every integrity finding.
//!
//! Warnings travel with the rest: a buyer told only the good news is back to
//! trusting the seller. The signature is over the RFC 8785
//! canonical form, so a buyer recomputes the digest from the bytes they hold and
//! checks it against the published attestor key. Anchoring the digest on Solana
//! is the daemon's step, on the path the other Covenant receipt kinds already
//! take; [`SignedReceipt::anchor`] is where it would land, outside the signed
//! bytes, and stays empty until an issuer fills it.
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
use crate::integrity::{evaluate, month_index, IntegrityPolicy, IntegrityReport};
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

/// Unknown fields are refused rather than dropped. A verifier that ignored them
/// would recompute the digest over less than it was handed, and whatever it
/// discarded would still be sitting next to a valid signature wherever the raw
/// receipt is rendered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignalReceipt {
    pub schema: String,
    pub signal: SignalDescriptor,
    pub evidence: Evidence,
    pub freshness: Freshness,
    pub integrity: IntegrityReport,
    pub issued_at: u64,
}

impl SignalReceipt {
    /// One cohort, one provider. A set spanning two would be labeled with the
    /// ambiguity this receipt exists to remove.
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
        let delivered_sha256 = digest_artifact(delivered)?;

        // A descriptor field only describes the cohort if every contribution
        // agrees on it. Where they don't, the receipt omits it instead of
        // labeling the set from whichever record sorted first.
        let agreed = |read: fn(&SignalRecord) -> Option<&str>| -> Option<String> {
            let value = read(first)?;
            records
                .iter()
                .all(|r| read(r) == Some(value))
                .then(|| value.to_string())
        };

        Ok(Self {
            schema: SCHEMA.to_string(),
            signal: SignalDescriptor {
                provider: first.provider.clone(),
                dataset_id: agreed(SignalRecord::dataset_id),
                record_type: agreed(SignalRecord::record_type),
                schema_standard: agreed(SignalRecord::schema_standard),
                schema_version: agreed(SignalRecord::schema_version),
                cohort_id,
                segment_id: agreed(SignalRecord::segment_id),
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
    /// Fails closed: an artifact this crate would refuse to issue over is not
    /// one it will confirm either.
    pub fn covers(&self, delivered: &Value) -> bool {
        digest_artifact(delivered).is_ok_and(|d| d == self.signal.delivered_sha256)
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
    /// A failing cohort still gets a receipt, carrying `fail` and the reasons.
    /// Withholding it would hide the failure and leave the buyer nothing to
    /// check; the sale decision is the seller's on top of it.
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
        self.integrity.sellable()
    }
}

/// Largest integer an IEEE-754 double represents exactly. RFC 8785 serializes
/// numbers through the ECMAScript rule, so anything past this is rounded before
/// it is hashed.
const MAX_EXACT_INTEGER: u64 = 1 << 53;

/// Digest the delivered artifact, refusing one whose numbers canonicalize
/// ambiguously.
///
/// Two artifacts differing only above 2^53 canonicalize to the same bytes and
/// would share a digest, so a receipt over one would "cover" the other. That is
/// the single property the receipt exists to provide, so the ambiguity is
/// rejected at the door rather than inherited. Anything at that magnitude (a
/// 64-bit id, a nanosecond timestamp, a token amount in base units) belongs in
/// the artifact as a string.
fn digest_artifact(delivered: &Value) -> Result<String> {
    if let Some(found) = unrepresentable_integer(delivered) {
        return Err(Error::Signal(format!(
            "delivered artifact carries {found}, which exceeds the 2^53 exact-integer range RFC 8785 canonicalizes within; encode it as a string"
        )));
    }
    let canon = serde_jcs::to_string(delivered).map_err(|e| Error::Decode(e.to_string()))?;
    Ok(hex::encode(Sha256::digest(canon.as_bytes())))
}

fn unrepresentable_integer(value: &Value) -> Option<String> {
    match value {
        Value::Number(n) => {
            let past_range = n
                .as_u64()
                .map(|u| u > MAX_EXACT_INTEGER)
                .or_else(|| n.as_i64().map(|i| i.unsigned_abs() > MAX_EXACT_INTEGER))
                .unwrap_or(false);
            past_range.then(|| n.to_string())
        }
        Value::Array(items) => items.iter().find_map(unrepresentable_integer),
        Value::Object(fields) => fields.values().find_map(unrepresentable_integer),
        _ => None,
    }
}

fn freshness(records: &[SignalRecord]) -> Freshness {
    let mut times: Vec<&str> = records.iter().filter_map(|r| r.generated_at()).collect();
    times.sort_unstable();

    // Ordered by calendar position, not by text: "10000-01" sorts before
    // "9999-01" as a string and would invert the span.
    let months: Vec<i32> = records
        .iter()
        .flat_map(|r| r.activity_months())
        .filter_map(|m| month_index(&m))
        .collect();
    let span = match (months.iter().min(), months.iter().max()) {
        (Some(a), Some(b)) => (b - a + 1) as u32,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

        // A signature over some other schema's object is a valid signature over
        // the wrong thing.
        if self.receipt.schema != SCHEMA {
            return false;
        }
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
    use crate::integrity::Status;
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
    fn an_artifact_whose_integers_canonicalize_ambiguously_is_refused() {
        // These two differ, but RFC 8785 rounds both to the same double, so one
        // receipt would cover both artifacts.
        let a = json!({ "cohort_id": "netflix_drama", "impressions": 9007199254740993u64 });
        let b = json!({ "cohort_id": "netflix_drama", "impressions": 9007199254740992u64 });
        assert_ne!(a, b);
        assert_eq!(
            serde_jcs::to_string(&a).unwrap(),
            serde_jcs::to_string(&b).unwrap()
        );

        let err = SignalReceipt::build(&a, &cohort(5), IntegrityPolicy::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("2^53"), "{err}");

        // Nested and negative land the same way, and a verifier fails closed.
        assert!(SignalReceipt::build(
            &json!({ "rows": [{ "id": -9007199254740993i64 }] }),
            &cohort(5),
            IntegrityPolicy::default()
        )
        .is_err());
        let receipt =
            SignalReceipt::build(&delivered(), &cohort(5), IntegrityPolicy::default()).unwrap();
        assert!(!receipt.covers(&a));
    }

    #[test]
    fn a_receipt_relabeled_to_another_schema_does_not_verify() {
        let key = SigningKey::from_bytes(&[11u8; 32]);
        let mut signed = SignalReceipt::build(&delivered(), &cohort(5), IntegrityPolicy::default())
            .unwrap()
            .attest(&key)
            .unwrap();
        signed.receipt.schema = "covenant.myrad.signal.v9".into();
        signed.root_hash_hex = signed.receipt.root_hash_hex().unwrap();
        signed.signature_b64 =
            STANDARD.encode(key.sign(signed.root_hash_hex.as_bytes()).to_bytes());
        assert!(
            !signed.verify(),
            "a valid signature over the wrong schema is not a signal receipt"
        );
    }

    #[test]
    fn descriptor_fields_the_set_disagrees_on_are_omitted() {
        let mut records = cohort(5);
        records[2].payload["dataset_id"] = json!("myrad_hulu_v1");
        let receipt =
            SignalReceipt::build(&delivered(), &records, IntegrityPolicy::default()).unwrap();
        assert_eq!(receipt.signal.dataset_id, None);
        assert_eq!(
            receipt.signal.record_type.as_deref(),
            Some("streaming_behavior_intelligence")
        );
    }

    #[test]
    fn an_unknown_field_in_a_receipt_is_refused_rather_than_dropped() {
        let key = SigningKey::from_bytes(&[13u8; 32]);
        let signed = SignalReceipt::build(&delivered(), &cohort(5), IntegrityPolicy::default())
            .unwrap()
            .attest(&key)
            .unwrap();
        let mut raw: Value = serde_json::to_value(&signed).unwrap();
        raw["receipt"]["contributors_marketing_claim"] = json!(50_000);
        assert!(serde_json::from_value::<SignedReceipt>(raw).is_err());
    }

    #[test]
    fn a_cohort_with_no_emission_time_cannot_be_receipted() {
        let mut records = cohort(5);
        records[0].payload["generated_at"] = Value::Null;
        let err = SignalReceipt::build(&delivered(), &records, IntegrityPolicy::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("generated_at"), "{err}");
    }

    #[test]
    fn one_contribution_copied_cannot_pass_as_a_cohort() {
        let one = cohort(1);
        let five: Vec<SignalRecord> = (0..5).map(|_| one[0].clone()).collect();
        let err = SignalReceipt::build(&delivered(), &five, IntegrityPolicy::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("duplicate contribution commitment"), "{err}");
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
