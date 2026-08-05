//! `covenant.tee.binding.v1`: a DCAP result plus the binding it was taken
//! under.
//!
//! The ER verifier already answers "is this a genuine, current TDX enclave"
//! against the Intel PCCS. What it hands back is a validator, a TCB status, a
//! measurement, and the `report_data` the quote echoed. On its own that says an
//! enclave is real; paired with the [`Binding`] the challenge was built from it
//! says the quote is fresh and names a specific thing for a specific agent (see
//! [`Binding`] for why a match names the triple rather than proving the agent
//! authorized it).
//!
//! [`BoundQuote::check`] answers both halves separately, because they fail for
//! different reasons and a caller usually wants to treat them differently. A
//! quote that is genuine but bound to the wrong subject is an integration bug.
//! A quote correctly bound to your subject on an out-of-date TCB is a fleet
//! problem. Collapsing them into one boolean loses that.

use serde::{Deserialize, Serialize};

use crate::challenge::Binding;

/// Wire schema tag carried in a serialized [`BoundQuote`].
pub const SCHEMA: &str = "covenant.tee.binding.v1";

/// TCB status the Intel PCCS reports for the platform the quote came from.
/// Anything other than `UpToDate` means the platform is behind on microcode or
/// carries an outstanding advisory.
pub const TCB_UP_TO_DATE: &str = "UpToDate";

/// A verified quote together with what it was bound to.
///
/// The `nonce` inside `binding` is published deliberately. It is not a secret
/// after the fact: its only job is to be unpredictable before the quote was
/// produced, and a verifier needs it to recompute the challenge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundQuote {
    pub schema: String,
    pub binding: Binding,
    /// ER validator identity, base58: the enclave's stable pubkey.
    pub validator: String,
    /// DCAP TCB status, verbatim from the verifier.
    pub tcb_status: String,
    /// TDX measurement (`mr_td`), hex: identifies the enclave image.
    pub mr_td: String,
    /// Intel advisory ids, empty when the TCB is clean.
    #[serde(default)]
    pub advisory_ids: Vec<String>,
    /// The `report_data` the quote carried, as the verifier read it.
    pub report_data: String,
    pub verified_at: u64,
}

impl BoundQuote {
    /// Assemble the envelope from a binding and the fields the ER verifier
    /// returns, in that order: `validator`, `tcb_status`, `mr_td`,
    /// `advisory_ids`, `report_data`, `verified_at`. The usual path is to
    /// deserialize a `BoundQuote` the verifier produced, not build one by hand.
    pub fn new(
        binding: Binding,
        validator: impl Into<String>,
        tcb_status: impl Into<String>,
        mr_td: impl Into<String>,
        advisory_ids: Vec<String>,
        report_data: impl Into<String>,
        verified_at: u64,
    ) -> Self {
        Self {
            schema: SCHEMA.to_string(),
            binding,
            validator: validator.into(),
            tcb_status: tcb_status.into(),
            mr_td: mr_td.into(),
            advisory_ids,
            report_data: report_data.into(),
            verified_at,
        }
    }

    /// The two questions this envelope answers, kept apart: is the quote bound
    /// to this binding's subject, and is the platform current. See [`Check`].
    pub fn check(&self) -> Check {
        Check {
            bound: self.binding.matches_report_data(&self.report_data),
            tcb_current: self.tcb_status.eq_ignore_ascii_case(TCB_UP_TO_DATE)
                && self.advisory_ids.is_empty(),
        }
    }

    /// The subject commitment this quote speaks for, or `None` when the quote
    /// is not bound to this binding.
    pub fn subject(&self) -> Option<&str> {
        self.check()
            .bound
            .then_some(self.binding.subject_commitment.as_str())
    }
}

/// The two questions, kept apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    /// The quote's `report_data` is the challenge this binding produces, so the
    /// quote is about this agent and this subject.
    pub bound: bool,
    /// The platform is current: `UpToDate` with no outstanding advisories.
    pub tcb_current: bool,
}

impl Check {
    /// Both halves hold.
    pub fn ok(&self) -> bool {
        self.bound && self.tcb_current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGENT: &str = "Ep7dD7biX7rZ6NSVzy8uEpgEEYipVfQ8ofwHzZmRM8dF";
    const VALIDATOR: &str = "MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo";

    fn binding() -> Binding {
        Binding::new(AGENT, "a".repeat(64), "b".repeat(64)).unwrap()
    }

    fn quote(report_data: String, tcb: &str, advisories: Vec<String>) -> BoundQuote {
        BoundQuote::new(
            binding(),
            VALIDATOR,
            tcb,
            "feb748".to_string() + &"0".repeat(90),
            advisories,
            report_data,
            1_785_000_000,
        )
    }

    #[test]
    fn a_bound_current_quote_passes_both_halves() {
        let q = quote(binding().challenge_hex(), TCB_UP_TO_DATE, vec![]);
        let c = q.check();
        assert!(c.bound);
        assert!(c.tcb_current);
        assert!(c.ok());
        assert_eq!(q.subject(), Some(binding().subject_commitment.as_str()));
    }

    #[test]
    fn a_genuine_quote_bound_to_another_subject_is_not_ok_and_names_no_subject() {
        let elsewhere = Binding::new(AGENT, "c".repeat(64), "b".repeat(64)).unwrap();
        let q = quote(elsewhere.challenge_hex(), TCB_UP_TO_DATE, vec![]);
        let c = q.check();
        assert!(!c.bound, "a quote about another subject must not bind");
        assert!(
            c.tcb_current,
            "the platform is still fine; only the binding failed"
        );
        assert!(!c.ok());
        assert_eq!(q.subject(), None);
    }

    #[test]
    fn a_correctly_bound_quote_on_a_stale_platform_separates_the_two_failures() {
        let q = quote(binding().challenge_hex(), "OutOfDate", vec![]);
        let c = q.check();
        assert!(c.bound);
        assert!(!c.tcb_current);
        assert!(!c.ok());
        // Still speaks for the subject: the binding held, the platform did not.
        assert_eq!(q.subject(), Some(binding().subject_commitment.as_str()));
    }

    #[test]
    fn an_outstanding_advisory_is_not_current_even_when_the_status_reads_up_to_date() {
        let q = quote(
            binding().challenge_hex(),
            TCB_UP_TO_DATE,
            vec!["INTEL-SA-00615".into()],
        );
        assert!(!q.check().tcb_current);
    }

    #[test]
    fn the_envelope_round_trips() {
        let q = quote(binding().challenge_hex(), TCB_UP_TO_DATE, vec![]);
        let wire = serde_json::to_value(&q).unwrap();
        assert_eq!(wire["schema"], SCHEMA);
        assert_eq!(wire["binding"]["subjectCommitment"], "a".repeat(64));
        assert_eq!(wire["reportData"], binding().challenge_hex());

        let back: BoundQuote = serde_json::from_value(wire).unwrap();
        assert_eq!(back, q);
        assert!(back.check().ok());
    }

    #[test]
    fn advisory_ids_default_to_empty_when_absent() {
        let mut wire =
            serde_json::to_value(quote(binding().challenge_hex(), TCB_UP_TO_DATE, vec![])).unwrap();
        wire.as_object_mut().unwrap().remove("advisoryIds");
        let back: BoundQuote = serde_json::from_value(wire).unwrap();
        assert!(back.advisory_ids.is_empty());
        assert!(back.check().ok());
    }
}
