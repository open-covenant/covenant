//! The payout policy and its pre-send decision.
//!
//! A Loofta payout routes value to a destination the sender named by handle.
//! This gate runs on the resolved recipient just before the transfer is signed
//! and returns allow, refuse, or escalate. It is pure: the recipient's Covenant
//! standing and the amount come in, a [`Verdict`] comes out. No network, no
//! funds move here.

use serde::{Deserialize, Serialize};

use crate::recipient::RecipientStanding;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PayoutPolicy {
    #[serde(default = "version_one")]
    pub version: u8,
    /// Reputation floor (0-100) an established recipient must clear. Applied
    /// only when the recipient has a track record; a fresh recipient has no
    /// score to fail against.
    #[serde(default)]
    pub min_score: u8,
    /// Hard cap per payout in USD. `None` leaves it uncapped here; the sender's
    /// own limits still apply upstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_payout_usd: Option<f64>,
    /// A payout to a recipient with no Covenant history over this USD amount
    /// escalates to approval. Fresh recipients are normal on Loofta (the privacy
    /// layer unlinks most destinations), so a small unknown payout is allowed
    /// and only a large one to a stranger is held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_recipient_review_over_usd: Option<f64>,
    /// Any payout at or above this escalates to human approval regardless of
    /// standing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_approval_over_usd: Option<f64>,
}

fn version_one() -> u8 {
    1
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Allow,
    Refuse(String),
    NeedsApproval(String),
}

impl PayoutPolicy {
    /// A conservative starting gate: refuse flagged recipients and anyone with a
    /// track record scoring under 25, escalate an unknown recipient over $50 and
    /// any payout at or above $1,000, no hard cap. The daemon overrides this per
    /// deployment.
    pub fn conservative() -> Self {
        Self {
            version: 1,
            min_score: 25,
            per_payout_usd: None,
            unknown_recipient_review_over_usd: Some(50.0),
            require_approval_over_usd: Some(1_000.0),
        }
    }

    /// Decide a payout. Checks run most-severe first: a hard refuse (abuse flag,
    /// cap, reputation floor) wins over an escalation, and an escalation wins
    /// over allow.
    pub fn authorize(&self, recipient: &RecipientStanding, amount_usd: f64) -> Verdict {
        if !amount_usd.is_finite() || amount_usd < 0.0 {
            return Verdict::Refuse(format!(
                "amount {amount_usd} is not a valid non-negative number"
            ));
        }
        if recipient.flagged {
            return Verdict::Refuse(format!(
                "recipient {} carries a Covenant abuse flag",
                recipient.recipient
            ));
        }
        if let Some(cap) = self.per_payout_usd {
            if amount_usd > cap {
                return Verdict::Refuse(format!(
                    "per-payout cap exceeded ({amount_usd:.2} > {cap:.2})"
                ));
            }
        }
        if recipient.established && recipient.score < self.min_score {
            return Verdict::Refuse(format!(
                "recipient reputation {} below floor {}",
                recipient.score, self.min_score
            ));
        }
        if !recipient.established {
            if let Some(limit) = self.unknown_recipient_review_over_usd {
                if amount_usd > limit {
                    return Verdict::NeedsApproval(format!(
                        "no Covenant history for {} and amount {amount_usd:.2} over {limit:.2}",
                        recipient.recipient
                    ));
                }
            }
        }
        if let Some(threshold) = self.require_approval_over_usd {
            if amount_usd >= threshold {
                return Verdict::NeedsApproval(format!(
                    "amount {amount_usd:.2} at or above approval threshold {threshold:.2}"
                ));
            }
        }
        Verdict::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn established(score: u8) -> RecipientStanding {
        RecipientStanding {
            recipient: "Rcpt1111111111111111111111111111111111111111".into(),
            score,
            established: true,
            flagged: false,
            source: "test".into(),
        }
    }

    fn fresh() -> RecipientStanding {
        RecipientStanding::unknown("Rcpt2222222222222222222222222222222222222222", "test")
    }

    #[test]
    fn allows_established_clean_within_limits() {
        assert_eq!(
            PayoutPolicy::conservative().authorize(&established(80), 120.0),
            Verdict::Allow
        );
    }

    #[test]
    fn refuses_flagged_recipient_even_for_a_dollar() {
        let mut r = established(99);
        r.flagged = true;
        assert!(matches!(
            PayoutPolicy::conservative().authorize(&r, 1.0),
            Verdict::Refuse(_)
        ));
    }

    #[test]
    fn refuses_established_below_score_floor() {
        assert!(matches!(
            PayoutPolicy::conservative().authorize(&established(10), 20.0),
            Verdict::Refuse(_)
        ));
    }

    #[test]
    fn score_floor_does_not_apply_to_fresh_recipient() {
        // A fresh recipient scores 0 but has no track record, so the floor is
        // not the thing that gates it; a small amount goes straight through.
        assert_eq!(
            PayoutPolicy::conservative().authorize(&fresh(), 20.0),
            Verdict::Allow
        );
    }

    #[test]
    fn escalates_large_payout_to_unknown_recipient() {
        assert!(matches!(
            PayoutPolicy::conservative().authorize(&fresh(), 400.0),
            Verdict::NeedsApproval(_)
        ));
    }

    #[test]
    fn refuses_over_hard_cap_before_escalating() {
        let p = PayoutPolicy {
            per_payout_usd: Some(250.0),
            ..PayoutPolicy::conservative()
        };
        // 400 clears the $250 cap but not the $1,000 approval floor, so the cap
        // refusal wins.
        assert!(matches!(
            p.authorize(&established(80), 400.0),
            Verdict::Refuse(_)
        ));
    }

    #[test]
    fn escalates_any_payout_over_global_threshold() {
        assert!(matches!(
            PayoutPolicy::conservative().authorize(&established(90), 1_000.0),
            Verdict::NeedsApproval(_)
        ));
    }

    #[test]
    fn refuses_non_finite_or_negative_amount() {
        let p = PayoutPolicy::conservative();
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
            assert!(
                matches!(p.authorize(&established(90), bad), Verdict::Refuse(_)),
                "amount {bad} must refuse, not fall through to allow"
            );
        }
    }

    #[test]
    fn policy_roundtrips_through_json() {
        let p = PayoutPolicy::conservative();
        let back: PayoutPolicy = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(p, back);
    }
}
