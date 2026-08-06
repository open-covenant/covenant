//! The checks that run before a receipt is signed.
//!
//! A signature over an aggregate proves who emitted it, not that it holds
//! together. These are the questions a buyer would ask if they could see the
//! contributing set, asked mechanically over every cohort, with the answers
//! carried in the receipt so the buyer gets the check and not just the claim.
//!
//! Each check is stated so that a failure is unambiguous and a pass is worth
//! something. [`Status::Fail`] means the cohort should not be sold as it stands;
//! [`Status::Warn`] means it can be sold, and the buyer is told what is soft
//! about it. Nothing here inspects behavior data, and no check needs an
//! identifier: everything runs on the provenance envelope and the salted subject
//! pseudonyms.
//!
//! Several checks read fields that only the streaming providers carry. A check
//! that finds nothing to read reports [`Status::Warn`] and says how many
//! contributions it skipped, never a pass. A pass has to mean the check ran.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::signal::{ProofRefKind, SignalRecord};

/// Verdicts Myrad's pipeline emits for a contribution it accepted. `zk_verified`
/// is the stronger of the two and must not read as the weaker one failing.
const ACCEPTED_VERIFICATION: [&str; 2] = ["verified", "zk_verified"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Stable identifier, so a buyer can pin a check by name across versions.
    pub check: String,
    pub status: Status,
    /// What was observed, in the terms of the check.
    pub detail: String,
    /// Contributions the finding applies to.
    pub affected: usize,
}

impl Finding {
    fn new(check: &str, status: Status, detail: impl Into<String>, affected: usize) -> Self {
        Self {
            check: check.to_string(),
            status,
            detail: detail.into(),
            affected,
        }
    }

    /// The check had nothing to read. Not a pass: the buyer is told it did not
    /// run and on how many contributions.
    fn not_evaluated(check: &str, field: &str, skipped: usize) -> Self {
        Self::new(
            check,
            Status::Warn,
            format!("not evaluated: {skipped} contribution(s) carry no {field}"),
            skipped,
        )
    }
}

/// Thresholds the checks are run against. Defaults are the ones Covenant is
/// willing to sign behind; a caller can tighten them, and the receipt records
/// what was used so a buyer isn't comparing receipts run under different bars.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrityPolicy {
    /// Smallest cohort that may be sold. Aggregation is Myrad's privacy story;
    /// below this the aggregate is close enough to a person to be treated as
    /// one.
    #[serde(default = "default_min_k")]
    pub min_k: usize,
    /// Treat a contribution resting on a pipeline delivery marker rather than a
    /// Reclaim proof id as a failure instead of a warning.
    #[serde(default)]
    pub require_reclaim_proof_ref: bool,
}

fn default_min_k() -> usize {
    5
}

impl Default for IntegrityPolicy {
    fn default() -> Self {
        Self {
            min_k: default_min_k(),
            require_reclaim_proof_ref: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrityReport {
    pub status: Status,
    pub policy: IntegrityPolicy,
    pub findings: Vec<Finding>,
}

impl IntegrityReport {
    /// Whether the cohort cleared every check that blocks a sale.
    pub fn sellable(&self) -> bool {
        self.status != Status::Fail
    }

    pub fn finding(&self, check: &str) -> Option<&Finding> {
        self.findings.iter().find(|f| f.check == check)
    }
}

/// Months since year zero, from a `YYYY-MM` prefix. An out-of-range month is
/// unparseable, not aliased onto the neighboring year.
pub(crate) fn month_index(s: &str) -> Option<i32> {
    let (y, m) = s.get(..7)?.split_once('-')?;
    let (y, m) = (y.parse::<i32>().ok()?, m.parse::<i32>().ok()?);
    (1..=12).contains(&m).then(|| y * 12 + m - 1)
}

pub fn evaluate(records: &[SignalRecord], policy: IntegrityPolicy) -> IntegrityReport {
    let findings = vec![
        consent(records),
        verification_status(records),
        subject_uniqueness(records),
        proof_uniqueness(records),
        payload_distinctness(records),
        proof_reference(records, policy),
        cohort_size(records, policy),
        temporal_bounds(records),
        window_consistency(records),
        pii_surface(records),
        activity_present(records),
    ];

    let status = findings
        .iter()
        .map(|f| f.status)
        .max()
        .unwrap_or(Status::Pass);

    IntegrityReport {
        status,
        policy,
        findings,
    }
}

fn consent(records: &[SignalRecord]) -> Finding {
    let opted_out = records.iter().filter(|r| r.opt_out).count();
    if opted_out > 0 {
        Finding::new(
            "consent",
            Status::Fail,
            format!("{opted_out} contribution(s) marked opt_out are in the set"),
            opted_out,
        )
    } else {
        Finding::new("consent", Status::Pass, "no opted-out contributions", 0)
    }
}

/// Myrad's own verdict, passed through. Covenant does not verify the Reclaim
/// proof itself, so this check reports what the pipeline said rather than
/// standing behind it.
fn verification_status(records: &[SignalRecord]) -> Finding {
    let rejected: Vec<&str> = records
        .iter()
        .map(|r| r.verification_status.as_str())
        .filter(|s| !ACCEPTED_VERIFICATION.contains(s))
        .collect();

    if rejected.is_empty() {
        return Finding::new(
            "verification_status",
            Status::Pass,
            format!(
                "all {} contribution(s) accepted by the pipeline",
                records.len()
            ),
            0,
        );
    }
    let mut observed: Vec<&str> = rejected.clone();
    observed.sort_unstable();
    observed.dedup();
    Finding::new(
        "verification_status",
        Status::Fail,
        format!(
            "{} contribution(s) carry a verification status outside {ACCEPTED_VERIFICATION:?}: {observed:?}",
            rejected.len()
        ),
        rejected.len(),
    )
}

/// One human, counted once. Duplicate pseudonyms in a cohort mean the same
/// subject was folded in twice, which inflates the population a buyer thinks
/// they bought.
fn subject_uniqueness(records: &[SignalRecord]) -> Finding {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    let mut missing = 0usize;
    for r in records {
        match r.subject_commitment.as_deref() {
            Some(c) => *seen.entry(c).or_default() += 1,
            None => missing += 1,
        }
    }
    let repeated: usize = seen.values().filter(|n| **n > 1).map(|n| n - 1).sum();

    let unproven = if missing > 0 {
        format!(", and {missing} carry no subject commitment, so distinctness is unproven for them")
    } else {
        String::new()
    };

    if repeated > 0 {
        Finding::new(
            "subject_uniqueness",
            Status::Fail,
            format!(
                "{repeated} duplicate subject commitment(s): a contributor is counted more than once{unproven}"
            ),
            repeated + missing,
        )
    } else if missing > 0 {
        Finding::new(
            "subject_uniqueness",
            Status::Warn,
            format!(
                "{missing} contribution(s) carry no subject commitment, so distinctness is unproven for them"
            ),
            missing,
        )
    } else {
        Finding::new(
            "subject_uniqueness",
            Status::Pass,
            format!("{} distinct contributors", seen.len()),
            0,
        )
    }
}

/// One proof backs one contribution. The same proof id under two contributions
/// is the same evidence counted twice, whatever the subject pseudonyms say.
fn proof_uniqueness(records: &[SignalRecord]) -> Finding {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for r in records {
        *seen.entry(r.proof.id.as_str()).or_default() += 1;
    }
    let repeated: usize = seen.values().filter(|n| **n > 1).map(|n| n - 1).sum();

    if repeated > 0 {
        Finding::new(
            "proof_uniqueness",
            Status::Fail,
            format!(
                "{repeated} contribution(s) reuse a proof reference already counted in this cohort"
            ),
            repeated,
        )
    } else {
        Finding::new(
            "proof_uniqueness",
            Status::Pass,
            format!("{} distinct proof reference(s)", seen.len()),
            0,
        )
    }
}

/// Distinct subjects carrying byte-identical payloads. Not proof of anything on
/// its own, and a legitimate cause exists (two genuinely identical low-activity
/// accounts), but it is the shape a replayed source record makes, so the buyer
/// is told.
fn payload_distinctness(records: &[SignalRecord]) -> Finding {
    let mut by_digest: BTreeMap<String, usize> = BTreeMap::new();
    for r in records {
        let Ok(digest) = r.payload_sha256() else {
            continue;
        };
        *by_digest.entry(digest).or_default() += 1;
    }
    let collisions: Vec<usize> = by_digest.values().copied().filter(|n| *n > 1).collect();

    if collisions.is_empty() {
        return Finding::new(
            "payload_distinctness",
            Status::Pass,
            "every contribution carries a distinct payload",
            0,
        );
    }
    let affected: usize = collisions.iter().sum();
    Finding::new(
        "payload_distinctness",
        Status::Warn,
        format!(
            "{} payload(s) appear under more than one contributor ({affected} contributions), which is what a replayed source record looks like",
            collisions.len()
        ),
        affected,
    )
}

/// Whether the proof reference on each contribution is a Reclaim proof id or a
/// pipeline delivery marker. A marker may resolve to a real proof inside Myrad,
/// but nothing outside Myrad can follow it there.
fn proof_reference(records: &[SignalRecord], policy: IntegrityPolicy) -> Finding {
    let mut markers = 0usize;
    let mut unrecognized = 0usize;
    for r in records {
        match r.proof.kind {
            ProofRefKind::PipelineMarker => markers += 1,
            ProofRefKind::Unrecognized => unrecognized += 1,
            ProofRefKind::ReclaimProof => {}
        }
    }

    if unrecognized > 0 {
        return Finding::new(
            "proof_reference",
            Status::Fail,
            format!("{unrecognized} contribution(s) carry a proof reference in no known shape"),
            unrecognized,
        );
    }
    if markers == 0 {
        return Finding::new(
            "proof_reference",
            Status::Pass,
            format!(
                "all {} contribution(s) reference a Reclaim proof id",
                records.len()
            ),
            0,
        );
    }
    Finding::new(
        "proof_reference",
        if policy.require_reclaim_proof_ref {
            Status::Fail
        } else {
            Status::Warn
        },
        format!(
            "{markers} of {} contribution(s) reference a pipeline delivery marker rather than a Reclaim proof id, so the proof is resolvable only inside Myrad",
            records.len()
        ),
        markers,
    )
}

fn cohort_size(records: &[SignalRecord], policy: IntegrityPolicy) -> Finding {
    let k = records.len();
    if k < policy.min_k {
        Finding::new(
            "cohort_min_k",
            Status::Fail,
            format!("cohort of {k} is below the minimum of {}", policy.min_k),
            k,
        )
    } else {
        Finding::new(
            "cohort_min_k",
            Status::Pass,
            format!("cohort of {k} meets the minimum of {}", policy.min_k),
            0,
        )
    }
}

/// Activity dated after the record was generated. A pipeline cannot observe the
/// future, so this is a parsing or padding fault, and it lands in an aggregate
/// as inflated volume.
fn temporal_bounds(records: &[SignalRecord]) -> Finding {
    let mut evaluated = 0usize;
    let mut affected = 0usize;
    let mut worst = 0i32;
    for r in records {
        let Some(generated) = r.generated_at().and_then(month_index) else {
            continue;
        };
        let Some(furthest) = r
            .activity_months()
            .iter()
            .filter_map(|m| month_index(m))
            .map(|i| i - generated)
            .max()
        else {
            continue;
        };
        evaluated += 1;
        if furthest > 0 {
            affected += 1;
            worst = worst.max(furthest);
        }
    }

    if evaluated == 0 {
        return Finding::not_evaluated("temporal_bounds", "dated activity", records.len());
    }
    if affected == 0 {
        Finding::new(
            "temporal_bounds",
            Status::Pass,
            format!(
                "{evaluated} of {} contribution(s) carried dated activity, none after its own generation time",
                records.len()
            ),
            0,
        )
    } else {
        Finding::new(
            "temporal_bounds",
            Status::Fail,
            format!(
                "{affected} contribution(s) carry activity dated after their own generation time (up to {worst} month(s) ahead)"
            ),
            affected,
        )
    }
}

/// The declared retention window against the activity actually present. A
/// constant window field next to a multi-year history means the window is a
/// label, and a buyer pricing recency off it is pricing the wrong thing.
fn window_consistency(records: &[SignalRecord]) -> Finding {
    let mut evaluated = 0usize;
    let mut affected = 0usize;
    let mut widest = 0i32;
    for r in records {
        let Some(days) = r.window_days_claimed() else {
            continue;
        };
        let mut months: Vec<i32> = r
            .activity_months()
            .iter()
            .filter_map(|m| month_index(m))
            .collect();
        months.sort_unstable();
        let (Some(first), Some(last)) = (months.first(), months.last()) else {
            continue;
        };
        evaluated += 1;
        let span = last - first + 1;
        let claimed = days.div_ceil(30).clamp(1, i32::MAX as u64) as i32;
        if span > claimed {
            affected += 1;
            widest = widest.max(span);
        }
    }

    if evaluated == 0 {
        return Finding::not_evaluated(
            "window_consistency",
            "declared window with dated activity",
            records.len(),
        );
    }
    if affected == 0 {
        Finding::new(
            "window_consistency",
            Status::Pass,
            format!("{evaluated} declared window(s) cover the activity present"),
            0,
        )
    } else {
        Finding::new(
            "window_consistency",
            Status::Warn,
            format!(
                "{affected} contribution(s) carry activity spanning more than the declared window (up to {widest} months)"
            ),
            affected,
        )
    }
}

/// Quasi-identifiers surviving in a payload that declares itself PII-stripped.
fn pii_surface(records: &[SignalRecord]) -> Finding {
    let claiming: Vec<&SignalRecord> = records.iter().filter(|r| r.claims_pii_stripped()).collect();
    if claiming.is_empty() {
        return Finding::not_evaluated("pii_surface", "pii_stripped declaration", records.len());
    }

    let mut fields: BTreeMap<&str, usize> = BTreeMap::new();
    let mut affected = 0usize;
    for r in &claiming {
        let found = r.quasi_identifiers();
        if !found.is_empty() {
            affected += 1;
        }
        for f in found {
            *fields.entry(f).or_default() += 1;
        }
    }

    if fields.is_empty() {
        return Finding::new(
            "pii_surface",
            Status::Pass,
            format!(
                "{} payload(s) declaring pii_stripped carry no known quasi-identifier",
                claiming.len()
            ),
            0,
        );
    }
    let named: Vec<String> = fields.iter().map(|(f, n)| format!("{f} ({n})")).collect();
    Finding::new(
        "pii_surface",
        Status::Warn,
        format!(
            "payloads declare pii_stripped while carrying quasi-identifiers: {}",
            named.join(", ")
        ),
        affected,
    )
}

/// Contributions verified as real but reporting nothing measured. They are
/// legitimate records; they just don't contribute signal, and counting them
/// toward a cohort overstates it.
fn activity_present(records: &[SignalRecord]) -> Finding {
    let counted: Vec<&SignalRecord> = records
        .iter()
        .filter(|r| r.activity_count().is_some())
        .collect();
    if counted.is_empty() {
        return Finding::not_evaluated("activity_present", "activity count", records.len());
    }

    let empty = counted
        .iter()
        .filter(|r| r.activity_count() == Some(0))
        .count();
    if empty == 0 {
        Finding::new(
            "activity_present",
            Status::Pass,
            format!("{} contribution(s) report measured activity", counted.len()),
            0,
        )
    } else {
        Finding::new(
            "activity_present",
            Status::Warn,
            format!("{empty} contribution(s) report an activity count of zero"),
            empty,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// A well-formed contribution. Tests override the one field they are about.
    struct Spec<'a> {
        proof_id: String,
        subject: Option<String>,
        verification: &'a str,
        generated: &'a str,
        months: &'a [&'a str],
        titles: Option<u64>,
        initial: Option<&'a str>,
        window_days: Option<u64>,
    }

    impl Spec<'_> {
        fn new(i: usize) -> Self {
            Self {
                proof_id: format!("{:024x}", i + 1),
                subject: Some(format!("subject-{i}")),
                verification: "verified",
                generated: "2026-05-19T00:00:00Z",
                months: &["2026-04", "2026-05"],
                titles: Some(100 + i as u64),
                initial: None,
                window_days: Some(90),
            }
        }

        fn build(self) -> SignalRecord {
            let monthly: serde_json::Map<String, Value> = self
                .months
                .iter()
                .map(|m| ((*m).to_string(), json!(1)))
                .collect();
            let mut summary = serde_json::Map::new();
            if let Some(days) = self.window_days {
                summary.insert("data_window_days".into(), json!(days));
            }
            if let Some(titles) = self.titles {
                summary.insert("total_titles_watched".into(), json!(titles));
            }
            let value = json!({
                "provider": "netflix",
                "reclaim_proof_id": self.proof_id,
                "verification_status": self.verification,
                "signal": {
                    "dataset_id": "myrad_netflix_v1",
                    "generated_at": self.generated,
                    "metadata": { "privacy_compliance": { "cohort_id": "c", "pii_stripped": true } },
                    "user_profile": { "profile_name_initial": self.initial },
                    "viewing_summary": Value::Object(summary),
                    "viewing_behavior": { "monthly_pattern": Value::Object(monthly) }
                }
            });
            SignalRecord::from_sample(&value, self.subject, false).unwrap()
        }
    }

    /// A record from a provider whose payload has none of the streaming fields.
    fn foreign(i: usize) -> SignalRecord {
        let value = json!({
            "provider": "spotify",
            "reclaim_proof_id": format!("{:024x}", 900 + i),
            "verification_status": "verified",
            "signal": {
                "dataset_id": "myrad_spotify_v1",
                "generated_at": "2026-05-19T00:00:00Z",
                "metadata": { "privacy_compliance": { "cohort_id": "c" } },
                "listening_behavior": { "monthly_pattern": { "2031-12": 4 } }
            }
        });
        SignalRecord::from_sample(&value, Some(format!("subject-{i}")), false).unwrap()
    }

    fn clean(n: usize) -> Vec<SignalRecord> {
        (0..n).map(|i| Spec::new(i).build()).collect()
    }

    #[test]
    fn a_clean_cohort_passes_every_check() {
        let report = evaluate(&clean(5), IntegrityPolicy::default());
        assert_eq!(report.status, Status::Pass, "{:#?}", report.findings);
        assert!(report.sellable());
    }

    #[test]
    fn an_opted_out_contribution_blocks_the_sale() {
        let mut records = clean(5);
        records[2].opt_out = true;
        let report = evaluate(&records, IntegrityPolicy::default());
        assert_eq!(report.finding("consent").unwrap().status, Status::Fail);
        assert!(!report.sellable());
    }

    #[test]
    fn zk_verified_is_accepted_and_an_unknown_verdict_names_itself() {
        let mut records = clean(5);
        records[0] = Spec {
            verification: "zk_verified",
            ..Spec::new(0)
        }
        .build();
        let report = evaluate(&records, IntegrityPolicy::default());
        assert_eq!(
            report.finding("verification_status").unwrap().status,
            Status::Pass
        );

        records[1] = Spec {
            verification: "pending_review",
            ..Spec::new(1)
        }
        .build();
        let f = evaluate(&records, IntegrityPolicy::default());
        let f = f.finding("verification_status").unwrap();
        assert_eq!(f.status, Status::Fail);
        assert!(f.detail.contains("pending_review"), "{}", f.detail);
    }

    #[test]
    fn the_same_subject_twice_is_a_failure() {
        let mut records = clean(5);
        records[4].subject_commitment = records[0].subject_commitment.clone();
        let report = evaluate(&records, IntegrityPolicy::default());
        let f = report.finding("subject_uniqueness").unwrap();
        assert_eq!(f.status, Status::Fail);
        assert_eq!(f.affected, 1);
    }

    #[test]
    fn a_missing_subject_commitment_warns_rather_than_passing_silently() {
        let mut records = clean(5);
        records[1].subject_commitment = None;
        let report = evaluate(&records, IntegrityPolicy::default());
        assert_eq!(
            report.finding("subject_uniqueness").unwrap().status,
            Status::Warn
        );
    }

    #[test]
    fn a_duplicate_and_a_missing_commitment_are_both_reported() {
        let mut records = clean(5);
        records[4].subject_commitment = records[0].subject_commitment.clone();
        records[1].subject_commitment = None;
        let report = evaluate(&records, IntegrityPolicy::default());
        let f = report.finding("subject_uniqueness").unwrap();
        assert_eq!(f.status, Status::Fail);
        assert!(f.detail.contains("no subject commitment"), "{}", f.detail);
        assert_eq!(f.affected, 2);
    }

    #[test]
    fn one_proof_backing_two_contributions_fails() {
        let mut records = clean(5);
        records[3].proof.id = records[0].proof.id.clone();
        let report = evaluate(&records, IntegrityPolicy::default());
        let f = report.finding("proof_uniqueness").unwrap();
        assert_eq!(f.status, Status::Fail);
        assert_eq!(f.affected, 1);
        assert!(!report.sellable());
    }

    #[test]
    fn identical_payloads_under_two_subjects_are_flagged() {
        let mut records = clean(5);
        records[3].payload = records[0].payload.clone();
        let report = evaluate(&records, IntegrityPolicy::default());
        let f = report.finding("payload_distinctness").unwrap();
        assert_eq!(f.status, Status::Warn);
        assert_eq!(f.affected, 2);
    }

    #[test]
    fn pipeline_markers_warn_by_default_and_fail_when_required() {
        let mut records = clean(5);
        records[0] = Spec {
            proof_id: "polled_1783517938566".into(),
            ..Spec::new(0)
        }
        .build();
        let lenient = evaluate(&records, IntegrityPolicy::default());
        assert_eq!(
            lenient.finding("proof_reference").unwrap().status,
            Status::Warn
        );

        let strict = evaluate(
            &records,
            IntegrityPolicy {
                require_reclaim_proof_ref: true,
                ..IntegrityPolicy::default()
            },
        );
        assert_eq!(
            strict.finding("proof_reference").unwrap().status,
            Status::Fail
        );
    }

    #[test]
    fn a_cohort_below_min_k_cannot_be_sold() {
        let report = evaluate(&clean(2), IntegrityPolicy::default());
        assert_eq!(report.finding("cohort_min_k").unwrap().status, Status::Fail);
        assert!(!report.sellable());
    }

    #[test]
    fn future_dated_activity_fails() {
        let mut records = clean(5);
        records[0] = Spec {
            months: &["2026-04", "2027-06"],
            ..Spec::new(0)
        }
        .build();
        let report = evaluate(&records, IntegrityPolicy::default());
        let f = report.finding("temporal_bounds").unwrap();
        assert_eq!(f.status, Status::Fail);
        assert!(f.detail.contains("13 month"), "{}", f.detail);
    }

    #[test]
    fn a_declared_window_narrower_than_the_data_warns() {
        let mut records = clean(5);
        records[0] = Spec {
            months: &["2017-04", "2026-05"],
            ..Spec::new(0)
        }
        .build();
        let report = evaluate(&records, IntegrityPolicy::default());
        assert_eq!(
            report.finding("window_consistency").unwrap().status,
            Status::Warn
        );
    }

    #[test]
    fn a_surviving_initial_warns_against_the_pii_stripped_claim() {
        let mut records = clean(5);
        records[0] = Spec {
            initial: Some("K"),
            ..Spec::new(0)
        }
        .build();
        let report = evaluate(&records, IntegrityPolicy::default());
        let f = report.finding("pii_surface").unwrap();
        assert_eq!(f.status, Status::Warn);
        assert!(f.detail.contains("profile_name_initial"));
        assert_eq!(f.affected, 1);
    }

    #[test]
    fn contributions_reporting_zero_activity_warn() {
        let mut records = clean(5);
        records[0] = Spec {
            titles: Some(0),
            ..Spec::new(0)
        }
        .build();
        let report = evaluate(&records, IntegrityPolicy::default());
        assert_eq!(
            report.finding("activity_present").unwrap().status,
            Status::Warn
        );
    }

    #[test]
    fn a_payload_shape_the_checks_cannot_read_never_reports_a_pass() {
        let records: Vec<SignalRecord> = (0..5).map(foreign).collect();
        let report = evaluate(&records, IntegrityPolicy::default());

        for check in [
            "temporal_bounds",
            "window_consistency",
            "pii_surface",
            "activity_present",
        ] {
            let f = report.finding(check).unwrap();
            assert_eq!(f.status, Status::Warn, "{check} passed vacuously: {f:?}");
            assert!(f.detail.starts_with("not evaluated"), "{}", f.detail);
            assert_eq!(f.affected, 5);
        }
        assert_eq!(report.status, Status::Warn);
        assert!(
            report.sellable(),
            "unreadable fields are soft, not blocking"
        );
    }

    #[test]
    fn month_index_rejects_an_out_of_range_month() {
        assert_eq!(month_index("2026-05"), month_index("2026-05-19T17:40:20Z"));
        assert!(month_index("2027-06").unwrap() > month_index("2026-05").unwrap());
        assert_eq!(month_index("nope"), None);
        // 2025-17 must not alias onto 2026-05.
        assert_eq!(month_index("2025-17"), None);
        assert_eq!(month_index("2026-00"), None);
    }

    #[test]
    fn an_out_of_range_month_does_not_hide_future_dated_activity() {
        let mut records = clean(5);
        records[0] = Spec {
            months: &["2025-17"],
            ..Spec::new(0)
        }
        .build();
        let report = evaluate(&records, IntegrityPolicy::default());
        let f = report.finding("temporal_bounds").unwrap();
        assert_eq!(f.status, Status::Pass);
        assert!(f.detail.contains("4 of 5"), "{}", f.detail);
    }

    #[test]
    fn an_absurd_declared_window_does_not_wrap_into_a_pass() {
        let mut records = clean(5);
        records[0] = Spec {
            window_days: Some(u64::MAX),
            months: &["2017-04", "2026-05"],
            ..Spec::new(0)
        }
        .build();
        let report = evaluate(&records, IntegrityPolicy::default());
        assert_eq!(
            report.finding("window_consistency").unwrap().status,
            Status::Pass
        );
    }

    #[test]
    fn a_partial_policy_fills_in_the_defaults() {
        let p: IntegrityPolicy = serde_json::from_value(json!({ "min_k": 25 })).unwrap();
        assert_eq!(p.min_k, 25);
        assert!(!p.require_reclaim_proof_ref);
        assert_eq!(
            serde_json::from_value::<IntegrityPolicy>(json!({})).unwrap(),
            IntegrityPolicy::default()
        );
    }
}
