//! Shared plumbing for the examples: read a Myrad export, or stand one in.
//!
//! Not part of the crate's API. Both examples compile this in so they read the
//! same export the same way.

use std::collections::BTreeMap;
use std::fs;

use covenant_myrad::SignalRecord;
use serde_json::{json, Value};

/// Stands in for the salt Myrad holds. In production the pseudonym arrives
/// already computed and Covenant never holds a salt or a user id.
pub const DEMO_SALT: &[u8] = b"myrad-demo-salt";

pub fn by_cohort(records: Vec<SignalRecord>) -> BTreeMap<String, Vec<SignalRecord>> {
    let mut cohorts: BTreeMap<String, Vec<SignalRecord>> = BTreeMap::new();
    for record in records {
        let cohort = record.cohort_id().unwrap_or("(none)").to_string();
        cohorts.entry(cohort).or_default().push(record);
    }
    cohorts
}

/// A stand-in aggregate: the shape of thing a buyer pays for. Real aggregation
/// is Myrad's; Covenant binds the artifact rather than computing it.
pub fn aggregate(cohort_id: &str, records: &[SignalRecord]) -> Value {
    json!({
        "cohort_id": cohort_id,
        "contributors": records.len(),
        "provider": records.first().map(|r| r.provider.clone()).unwrap_or_default(),
    })
}

pub fn load(signals_path: &str, profiles_path: Option<&str>) -> Vec<SignalRecord> {
    let raw =
        fs::read_to_string(signals_path).unwrap_or_else(|e| panic!("read {signals_path}: {e}"));
    let values: Vec<Value> = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("{signals_path} is not an array of signal records: {e}"));

    let subjects = profiles_path.map(subjects_by_proof_id).unwrap_or_default();

    values
        .iter()
        .filter_map(|value| {
            let proof_id = value.get("reclaim_proof_id")?.as_str()?;
            let (subject, opt_out) = match subjects.get(proof_id) {
                Some((user, out)) => (Some(SignalRecord::commit_subject(DEMO_SALT, user)), *out),
                None => (None, false),
            };
            SignalRecord::from_sample(value, subject, opt_out).ok()
        })
        .collect()
}

/// proof id -> (user id, opted out), from the profiles table. The payload
/// carries neither, which is the point: consent and subject identity live
/// outside the artifact being sold.
fn subjects_by_proof_id(path: &str) -> BTreeMap<String, (String, bool)> {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let mut rows = csv_rows(&text);
    if rows.is_empty() {
        return BTreeMap::new();
    }
    let header = rows.remove(0);
    let col = |name: &str| header.iter().position(|h| h == name);
    let (Some(proof), Some(user)) = (col("reclaim_proof_id"), col("user_id")) else {
        panic!("{path} has no reclaim_proof_id / user_id column");
    };
    let opt_out = col("opt_out");

    rows.into_iter()
        .filter_map(|row| {
            let id = row.get(proof)?.clone();
            let user = row.get(user)?.clone();
            let out = opt_out
                .and_then(|i| row.get(i))
                .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "t" | "1"));
            Some((id, (user, out)))
        })
        .collect()
}

/// RFC 4180 rows: quoted fields, doubled quotes, embedded newlines and commas.
/// Myrad's export puts a whole JSON document inside one field, so splitting on
/// commas would shred it.
fn csv_rows(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => row.push(std::mem::take(&mut field)),
            '\r' if !quoted => {}
            '\n' if !quoted => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

/// A clean six-contributor cohort, so the examples run without an export.
pub fn synthetic() -> Vec<SignalRecord> {
    (0..6)
        .map(|i| {
            let value = json!({
                "provider": "netflix",
                "reclaim_proof_id": format!("{:024x}", i + 1),
                "verification_status": "verified",
                "signal": {
                    "dataset_id": "myrad_netflix_v1",
                    "record_type": "streaming_behavior_intelligence",
                    "schema_version": "2.0",
                    "generated_at": "2026-05-19T17:40:20.697Z",
                    "metadata": {
                        "schema_standard": "myrad_streaming_intelligence_v2",
                        "privacy_compliance": { "cohort_id": "netflix_high_engagement_drama", "pii_stripped": true }
                    },
                    "user_profile": { "total_titles_watched": 40 + i },
                    "viewing_summary": { "data_window_days": 90, "total_titles_watched": 40 + i },
                    "viewing_behavior": { "monthly_pattern": { "2026-04": 3 + i, "2026-05": 7 } },
                    "audience_segment": { "segment_id": "netflix_high_engagement_drama" }
                }
            });
            let subject = SignalRecord::commit_subject(DEMO_SALT, &format!("sample_user_{i:02}"));
            SignalRecord::from_sample(&value, Some(subject), false).expect("synthetic record")
        })
        .collect()
}
