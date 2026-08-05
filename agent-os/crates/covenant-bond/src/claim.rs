//! `covenant.bond.slash.v1`: a slash, and the record it came from.
//!
//! The deployed settlement program reads a slash reason from the onchain
//! provenance root through the seed-bound credit account rather than from
//! anything the caller passes in, so there is no reason to forge. This mirrors
//! that off-chain: a claim cannot be constructed without a breach, and it
//! carries the observation root the breach was computed over. Anyone holding
//! the log recomputes both the root and the assessment and gets the same
//! answer, or the claim is false.
//!
//! [`SlashClaim::for_breach`] returns `None` when the operator kept the
//! promise. That is the whole enforcement of "no slash without a breach": there
//! is no constructor that takes an amount and a reason.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::observation::ObservationLog;
use crate::sla::{assess, Assessment, Sla};

pub const SCHEMA: &str = "covenant.bond.slash.v1";

/// Basis-point denominator.
const BPS_DENOMINATOR: u64 = 10_000;

/// What the operator has at risk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bond {
    /// The bonded operator, base58.
    pub operator: String,
    /// Amount posted, in the token's base units. This crate never interprets
    /// the unit; it only ever takes a fraction of it.
    pub posted: u64,
}

impl Bond {
    pub fn new(operator: impl Into<String>, posted: u64) -> Result<Self> {
        let operator = operator.into();
        if !(32..=44).contains(&operator.len()) {
            return Err(Error::Bond(format!(
                "operator must be a base58 pubkey of 32-44 characters, got {}",
                operator.len()
            )));
        }
        if posted == 0 {
            return Err(Error::Bond("a bond of zero puts nothing at risk".into()));
        }
        Ok(Self { operator, posted })
    }
}

/// How much a breach costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashPolicy {
    /// Basis points of the posted bond taken per breached window.
    pub slash_bps: u16,
}

impl SlashPolicy {
    pub fn new(slash_bps: u16) -> Result<Self> {
        if slash_bps == 0 {
            return Err(Error::Bond(
                "a slash of zero basis points is not an enforcement".into(),
            ));
        }
        if u64::from(slash_bps) > BPS_DENOMINATOR {
            return Err(Error::Bond(format!(
                "slashBps {slash_bps} exceeds 100 percent ({BPS_DENOMINATOR})"
            )));
        }
        Ok(Self { slash_bps })
    }

    fn amount_of(&self, posted: u64) -> u64 {
        // u128 so a large bond and a large rate cannot wrap into a small slash.
        let amount =
            (u128::from(posted) * u128::from(self.slash_bps)) / u128::from(BPS_DENOMINATOR);
        u64::try_from(amount).unwrap_or(posted).min(posted)
    }
}

/// A slash, with everything needed to check it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashClaim {
    pub schema: String,
    pub operator: String,
    /// Chain root of the observation log the breach was computed over.
    pub observation_root: String,
    pub assessment: Assessment,
    pub bond_posted: u64,
    pub policy: SlashPolicy,
    pub amount: u64,
    pub claimed_at_ms: u64,
}

impl SlashClaim {
    /// Build a claim if, and only if, the operator breached.
    pub fn for_breach(
        bond: &Bond,
        policy: SlashPolicy,
        log: &ObservationLog,
        sla: Sla,
        now_ms: u64,
    ) -> Result<Option<Self>> {
        let assessment = assess(log, sla, now_ms);
        if !assessment.breached {
            return Ok(None);
        }
        Ok(Some(Self {
            schema: SCHEMA.to_string(),
            operator: bond.operator.clone(),
            observation_root: log.root()?,
            assessment,
            bond_posted: bond.posted,
            policy,
            amount: policy.amount_of(bond.posted),
            claimed_at_ms: now_ms,
        }))
    }

    /// Recompute this claim from the log it names.
    ///
    /// The check an operator runs when they think a slash is wrong, and the one
    /// a keeper runs before submitting. It re-derives the root, re-assesses the
    /// window, and re-computes the amount, so a claim that was edited anywhere
    /// stops verifying.
    pub fn verifies_against(&self, log: &ObservationLog) -> bool {
        if self.schema != SCHEMA {
            return false;
        }
        let Ok(root) = log.root() else {
            return false;
        };
        if root != self.observation_root {
            return false;
        }
        let recomputed = assess(log, self.assessment.sla, self.assessment.window_end_ms);
        recomputed == self.assessment
            && recomputed.breached
            && self.amount == self.policy.amount_of(self.bond_posted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::{Obligation, Outcome};

    const OPERATOR: &str = "Ep7dD7biX7rZ6NSVzy8uEpgEEYipVfQ8ofwHzZmRM8dF";

    fn bond() -> Bond {
        Bond::new(OPERATOR, 5_000_000_000).unwrap()
    }

    fn policy() -> SlashPolicy {
        SlashPolicy::new(2_000).unwrap()
    }

    fn sla() -> Sla {
        Sla::new(1_000, 1, 10_000).unwrap()
    }

    fn timed_out(id: &str, at: u64) -> Obligation {
        Obligation::new(id, at, Outcome::TimedOut).unwrap()
    }

    fn breaching_log() -> ObservationLog {
        ObservationLog::new(vec![timed_out("a", 1_000), timed_out("b", 2_000)])
    }

    fn clean_log() -> ObservationLog {
        ObservationLog::new(vec![Obligation::new(
            "a",
            1_000,
            Outcome::Settled { at_ms: 1_100 },
        )
        .unwrap()])
    }

    #[test]
    fn keeping_the_promise_yields_no_claim() {
        let claim = SlashClaim::for_breach(&bond(), policy(), &clean_log(), sla(), 10_000).unwrap();
        assert!(
            claim.is_none(),
            "there is no constructor for a slash without a breach"
        );
    }

    #[test]
    fn a_breach_yields_a_claim_that_verifies_against_its_log() {
        let log = breaching_log();
        let claim = SlashClaim::for_breach(&bond(), policy(), &log, sla(), 10_000)
            .unwrap()
            .expect("breached");

        assert_eq!(claim.operator, OPERATOR);
        assert_eq!(claim.observation_root, log.root().unwrap());
        assert_eq!(
            claim.amount, 1_000_000_000,
            "2000 bps of 5 CVNT-scale units"
        );
        assert!(claim.verifies_against(&log));
    }

    #[test]
    fn a_claim_does_not_verify_against_a_different_log() {
        let claim = SlashClaim::for_breach(&bond(), policy(), &breaching_log(), sla(), 10_000)
            .unwrap()
            .unwrap();
        assert!(!claim.verifies_against(&clean_log()));

        // The operator quietly appends a success and re-presents the log.
        let mut edited = breaching_log();
        edited.append(Obligation::new("c", 3_000, Outcome::Settled { at_ms: 3_100 }).unwrap());
        assert!(!claim.verifies_against(&edited));
    }

    #[test]
    fn inflating_the_amount_breaks_the_claim() {
        let log = breaching_log();
        let mut claim = SlashClaim::for_breach(&bond(), policy(), &log, sla(), 10_000)
            .unwrap()
            .unwrap();
        assert!(claim.verifies_against(&log));

        claim.amount = claim.bond_posted;
        assert!(
            !claim.verifies_against(&log),
            "the amount follows from the policy"
        );
    }

    #[test]
    fn softening_the_assessment_breaks_the_claim() {
        let log = breaching_log();
        let mut claim = SlashClaim::for_breach(&bond(), policy(), &log, sla(), 10_000)
            .unwrap()
            .unwrap();
        claim.assessment.misses = 0;
        claim.assessment.breached = false;
        assert!(!claim.verifies_against(&log));
    }

    #[test]
    fn a_claim_relabelled_to_another_schema_does_not_verify() {
        let log = breaching_log();
        let mut claim = SlashClaim::for_breach(&bond(), policy(), &log, sla(), 10_000)
            .unwrap()
            .unwrap();
        claim.schema = "covenant.bond.slash.v9".into();
        assert!(!claim.verifies_against(&log));
    }

    #[test]
    fn a_slash_never_exceeds_the_bond() {
        let full = SlashPolicy::new(10_000).unwrap();
        let log = breaching_log();
        let claim = SlashClaim::for_breach(&bond(), full, &log, sla(), 10_000)
            .unwrap()
            .unwrap();
        assert_eq!(claim.amount, bond().posted);

        // A very large bond at the maximum rate must not wrap.
        let huge = Bond::new(OPERATOR, u64::MAX).unwrap();
        let claim = SlashClaim::for_breach(&huge, full, &log, sla(), 10_000)
            .unwrap()
            .unwrap();
        assert_eq!(claim.amount, u64::MAX);
    }

    #[test]
    fn the_claim_round_trips_and_still_verifies() {
        let log = breaching_log();
        let claim = SlashClaim::for_breach(&bond(), policy(), &log, sla(), 10_000)
            .unwrap()
            .unwrap();
        let wire = serde_json::to_value(&claim).unwrap();
        assert_eq!(wire["schema"], SCHEMA);
        assert_eq!(wire["observationRoot"], log.root().unwrap());

        let back: SlashClaim = serde_json::from_value(wire).unwrap();
        assert_eq!(back, claim);
        assert!(back.verifies_against(&log));
    }

    #[test]
    fn the_claim_carries_nothing_about_what_was_settled() {
        let claim = SlashClaim::for_breach(&bond(), policy(), &breaching_log(), sla(), 10_000)
            .unwrap()
            .unwrap();
        let wire = serde_json::to_string(&claim).unwrap();
        for leaked in ["amountUsd", "recipient", "payer", "counterparty"] {
            assert!(!wire.contains(leaked), "claim leaked {leaked}");
        }
    }

    #[test]
    fn incoherent_bonds_and_policies_are_refused() {
        assert!(Bond::new(OPERATOR, 0).is_err());
        assert!(Bond::new("short", 1).is_err());
        assert!(SlashPolicy::new(0).is_err());
        assert!(SlashPolicy::new(10_001).is_err());
    }
}
