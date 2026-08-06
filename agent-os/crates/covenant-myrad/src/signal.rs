//! Myrad's side of the wire: one verified contribution, as their pipeline emits
//! it.
//!
//! Myrad's enterprise pipeline produces a per-subject record whose `sellable_data`
//! is the artifact a buyer receives. This module reads that record without
//! re-modeling it. The payload stays an opaque [`serde_json::Value`] and the
//! receipt binds its RFC 8785 digest, so Myrad can add, reorder, or rename
//! fields inside `sellable_data` without invalidating a receipt schema or
//! forcing a Covenant release.
//!
//! Only the envelope is typed, because only the envelope is what provenance is
//! made of: which provider, which Reclaim proof, which cohort, when it was
//! generated, and whether the subject consented. Everything read here is
//! non-PII by construction; the raw account data never leaves the contributor's
//! device and never reaches Covenant.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// How the `reclaim_proof_id` on a record actually identifies the proof.
///
/// Myrad's samples carry both shapes under the same field name. A 24-character
/// hex id is a Reclaim proof reference; `polled_…` / `callback_…` are markers
/// the pipeline mints when it takes delivery of a proof by polling or webhook.
/// Both may well resolve to a real proof inside Myrad, but only the first is a
/// proof identifier on its face, and a receipt that flattened the difference
/// would be asserting more than it can see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofRefKind {
    /// A Reclaim proof identifier (24 hex characters).
    ReclaimProof,
    /// A pipeline delivery marker (`polled_…`, `callback_…`): resolvable to a
    /// proof only inside Myrad.
    PipelineMarker,
    /// Present but in neither known shape.
    Unrecognized,
}

impl ProofRefKind {
    fn classify(id: &str) -> Self {
        if id.len() == 24 && id.chars().all(|c| c.is_ascii_hexdigit()) {
            Self::ReclaimProof
        } else if id.starts_with("polled_") || id.starts_with("callback_") {
            Self::PipelineMarker
        } else {
            Self::Unrecognized
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofRef {
    pub id: String,
    pub kind: ProofRefKind,
}

/// One verified contribution: the record Myrad's pipeline emits for a single
/// subject, plus the two fields Covenant needs that live outside the payload.
#[derive(Debug, Clone)]
pub struct SignalRecord {
    pub provider: String,
    pub proof: ProofRef,
    /// Myrad's own verification verdict for the underlying Reclaim proof.
    pub verification_status: String,
    /// A stable pseudonym for the contributor, supplied by Myrad as
    /// `sha256(secret_salt || "|" || user_id)`. Covenant never sees `user_id`; the
    /// commitment exists so the same human can't be counted twice in one
    /// cohort, and so a replayed record is detectable, without identifying
    /// anyone. `None` when the source didn't supply one, which downgrades the
    /// contributor-uniqueness check rather than silently passing it.
    pub subject_commitment: Option<String>,
    /// Consent state at emission. A record marked opted out must not be sold.
    pub opt_out: bool,
    /// `sellable_data` verbatim: the bytes the buyer receives.
    pub payload: Value,
}

impl SignalRecord {
    /// Read one element of Myrad's signal sample array.
    ///
    /// `subject_commitment` and `opt_out` come from the profiles table rather
    /// than the payload, so they are passed in. Everything else is read off the
    /// record.
    pub fn from_sample(
        value: &Value,
        subject_commitment: Option<String>,
        opt_out: bool,
    ) -> Result<Self> {
        let provider = value
            .get("provider")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Signal("missing \"provider\"".into()))?
            .to_string();
        let id = value
            .get("reclaim_proof_id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Signal("missing \"reclaim_proof_id\"".into()))?
            .to_string();
        let payload = value
            .get("signal")
            .cloned()
            .ok_or_else(|| Error::Signal("missing \"signal\" payload".into()))?;

        Ok(Self {
            provider,
            proof: ProofRef {
                kind: ProofRefKind::classify(&id),
                id,
            },
            verification_status: value
                .get("verification_status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            subject_commitment,
            opt_out,
            payload,
        })
    }

    /// Compute a subject commitment the way Myrad should: salted, so the
    /// pseudonym can't be reversed by hashing a guessed user id, and stable, so
    /// the same subject collides across cohorts. Provided for the reference
    /// pipeline; in production the salt stays on Myrad's side and Covenant only
    /// ever receives the output.
    pub fn commit_subject(salt: &[u8], user_id: &str) -> String {
        let mut h = Sha256::new();
        h.update(salt);
        h.update(b"|");
        h.update(user_id.as_bytes());
        hex::encode(h.finalize())
    }

    /// sha256 over the RFC 8785 canonical form of `sellable_data`. This is what
    /// the receipt binds, and what a buyer recomputes over the bytes delivered.
    pub fn payload_sha256(&self) -> Result<String> {
        let canon =
            serde_jcs::to_string(&self.payload).map_err(|e| Error::Decode(e.to_string()))?;
        Ok(hex::encode(Sha256::digest(canon.as_bytes())))
    }

    fn meta(&self, path: &[&str]) -> Option<&Value> {
        path.iter()
            .try_fold(&self.payload, |acc, key| acc.get(*key))
    }

    fn meta_str(&self, path: &[&str]) -> Option<&str> {
        self.meta(path).and_then(Value::as_str)
    }

    pub fn dataset_id(&self) -> Option<&str> {
        self.meta_str(&["dataset_id"])
    }

    pub fn record_type(&self) -> Option<&str> {
        self.meta_str(&["record_type"])
    }

    pub fn schema_standard(&self) -> Option<&str> {
        self.meta_str(&["metadata", "schema_standard"])
    }

    pub fn schema_version(&self) -> Option<&str> {
        self.meta_str(&["schema_version"])
    }

    pub fn cohort_id(&self) -> Option<&str> {
        self.meta_str(&["metadata", "privacy_compliance", "cohort_id"])
    }

    pub fn segment_id(&self) -> Option<&str> {
        self.meta_str(&["audience_segment", "segment_id"])
    }

    /// Emission time, as the ISO-8601 string Myrad writes.
    pub fn generated_at(&self) -> Option<&str> {
        self.meta_str(&["generated_at"])
    }

    /// The retention window the record claims, in days.
    pub fn window_days_claimed(&self) -> Option<u64> {
        self.meta(&["viewing_summary", "data_window_days"])
            .and_then(Value::as_u64)
    }

    /// `pii_stripped` as asserted by the pipeline.
    pub fn claims_pii_stripped(&self) -> bool {
        self.meta(&["metadata", "privacy_compliance", "pii_stripped"])
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// Activity months present in the payload, sorted, as `YYYY-MM`. Myrad's
    /// streaming records carry these under `viewing_behavior.monthly_pattern`;
    /// other providers will land elsewhere, so a miss is empty rather than an
    /// error.
    pub fn activity_months(&self) -> Vec<String> {
        let mut months: Vec<String> = self
            .meta(&["viewing_behavior", "monthly_pattern"])
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        months.sort();
        months
    }

    /// Quasi-identifiers left in the payload while it claims to be PII-stripped.
    /// An initial is a selector, not a name: it narrows a population once a
    /// cohort label and a multi-year activity curve sit beside it.
    pub fn quasi_identifiers(&self) -> Vec<&'static str> {
        let mut found = Vec::new();
        if self
            .meta(&["user_profile", "profile_name_initial"])
            .is_some_and(|v| !v.is_null())
        {
            found.push("user_profile.profile_name_initial");
        }
        found
    }

    /// The activity count the payload reports, where it reports one. `None`
    /// means the field is absent, which is different from a reported zero and is
    /// kept distinct so a check can say it did not run rather than pass.
    pub fn activity_count(&self) -> Option<u64> {
        self.meta(&["viewing_summary", "total_titles_watched"])
            .and_then(Value::as_u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        json!({
            "sample_ref": "sample_01",
            "provider": "netflix",
            "reclaim_proof_id": "5a9f3035aa9cf93f653f4b15",
            "verification_status": "verified",
            "signal": {
                "dataset_id": "myrad_netflix_v1",
                "record_type": "streaming_behavior_intelligence",
                "generated_at": "2026-05-19T17:40:20.697Z",
                "schema_version": "2.0",
                "metadata": {
                    "schema_standard": "myrad_streaming_intelligence_v2",
                    "privacy_compliance": { "cohort_id": "netflix_high_engagement_premium_drama", "pii_stripped": true }
                },
                "user_profile": { "profile_name_initial": "K", "total_titles_watched": 4472 },
                "viewing_summary": { "data_window_days": 90, "total_titles_watched": 4472 },
                "viewing_behavior": { "monthly_pattern": { "2017-04": 10, "2026-01": 5 } },
                "audience_segment": { "segment_id": "netflix_high_engagement_premium_drama" }
            }
        })
    }

    #[test]
    fn reads_envelope() {
        let r = SignalRecord::from_sample(&sample(), None, false).unwrap();
        assert_eq!(r.provider, "netflix");
        assert_eq!(r.proof.kind, ProofRefKind::ReclaimProof);
        assert_eq!(r.dataset_id(), Some("myrad_netflix_v1"));
        assert_eq!(r.cohort_id(), Some("netflix_high_engagement_premium_drama"));
        assert_eq!(r.window_days_claimed(), Some(90));
        assert_eq!(r.activity_months(), vec!["2017-04", "2026-01"]);
        assert!(r.claims_pii_stripped());
    }

    #[test]
    fn classifies_proof_ref_shapes() {
        assert_eq!(
            ProofRefKind::classify("5a9f3035aa9cf93f653f4b15"),
            ProofRefKind::ReclaimProof
        );
        assert_eq!(
            ProofRefKind::classify("polled_1783517938566"),
            ProofRefKind::PipelineMarker
        );
        assert_eq!(
            ProofRefKind::classify("callback_1782915066350"),
            ProofRefKind::PipelineMarker
        );
        assert_eq!(ProofRefKind::classify("abc"), ProofRefKind::Unrecognized);
        // 24 chars but not hex.
        assert_eq!(
            ProofRefKind::classify("zzzzzzzzzzzzzzzzzzzzzzzz"),
            ProofRefKind::Unrecognized
        );
    }

    #[test]
    fn payload_digest_is_key_order_independent() {
        let a = SignalRecord::from_sample(&sample(), None, false).unwrap();
        let reordered: Value =
            serde_json::from_str(&serde_json::to_string(&sample()).unwrap().replace(
                "\"dataset_id\":\"myrad_netflix_v1\",\"record_type\"",
                "\"record_type\"",
            ))
            .unwrap();
        // Same content, different key order in the source text: JCS makes the
        // digest agree only when the content itself agrees.
        let mut b_value = reordered.clone();
        b_value["signal"]["dataset_id"] = json!("myrad_netflix_v1");
        let b = SignalRecord::from_sample(&b_value, None, false).unwrap();
        assert_eq!(a.payload_sha256().unwrap(), b.payload_sha256().unwrap());
    }

    #[test]
    fn flags_quasi_identifier_and_subject_commitment_is_salted() {
        let r = SignalRecord::from_sample(&sample(), None, false).unwrap();
        assert_eq!(
            r.quasi_identifiers(),
            vec!["user_profile.profile_name_initial"]
        );

        let a = SignalRecord::commit_subject(b"salt-a", "sample_user_01");
        let b = SignalRecord::commit_subject(b"salt-b", "sample_user_01");
        assert_ne!(a, b, "a different salt must give a different pseudonym");
        assert_eq!(a, SignalRecord::commit_subject(b"salt-a", "sample_user_01"));
    }
}
