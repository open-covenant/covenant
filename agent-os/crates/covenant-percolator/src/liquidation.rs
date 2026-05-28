//! Liquidation sequencing — strict total order over distressed
//! portfolios, with bounded per-step recovery budgets.
//!
//! ## What this layer does and why §21 needs it
//!
//! The crate's `RecoveryPolicy::decide` answers "should this *one*
//! portfolio be acted on, and with what `b_delta_budget`?" — a
//! per-portfolio question. The remaining question is: **when many
//! portfolios are distressed at once, in what order do keepers
//! act?**
//!
//! Toly's spec §21 ("Preemptible close ownership") requires:
//!
//! > a strict total order over close work, so two keepers seeing
//! > the same on-chain state arrive at the same next action with
//! > no hold-and-wait and no equal-priority livelock.
//!
//! Without that total order, two keepers can race indefinitely on
//! "who liquidates first" with no protocol-level resolution. Our
//! [`coordination::should_lead`] handles single-action races, but
//! the *queue itself* (which portfolio is at the front of the line)
//! must also be deterministic.
//!
//! This module provides the queue.
//!
//! ## The sequencing rule
//!
//! Sort `(certified_liq_deficit DESC, portfolio_address ASC)`.
//! Largest-deficit-first is the obvious behavior: the protocol
//! benefits most from clearing the biggest holes first. The address
//! tiebreak (lex over the base58 string) is *deterministic* — any
//! two keepers reading the same snapshot compute the same order.
//!
//! ## Termination + bounds
//!
//! Each step's `b_delta_budget` is capped at the engine-certified
//! deficit (`cert.certified_liq_deficit`). The on-chain
//! `ForfeitRecoveryLeg` further bounds: it can't move more than the
//! engine says is needed. So:
//!
//! - Each step monotonically reduces aggregate certified deficit
//!   (the engine's invariant; our policy just doesn't add to it).
//! - The sequence is finite: there are finitely many distressed
//!   portfolios, each emits exactly one `RecoveryForfeitLeg`, the
//!   sequence length is bounded by `portfolios.len()`.
//! - The sequence is fail-closed (§16): a portfolio whose cert
//!   is `valid = false` is dropped from the queue entirely, not
//!   "second-guessed".
//!
//! ## Property surface (locked by [`tests::*`] + proptest)
//!
//! - **L1 Determinism.** `sequence(certs)` is a pure function. Same
//!   input → same output. Trivially true by sort.
//! - **L2 Total-deficit-bounded.**
//!   `sum(action.b_delta_budget) ≤ sum(cert.certified_liq_deficit)`
//!   (over valid certs above `min_deficit`).
//! - **L3 Per-step-bounded.**
//!   `action[i].b_delta_budget ≤ cert[i].certified_liq_deficit`
//! - **L4 Fail-closed.** A portfolio with `cert.valid == false`
//!   produces no action in the sequence.
//! - **L5 Input-shuffle-invariant.** Permuting the input vector
//!   produces the identical output sequence (sort is stable on the
//!   `(deficit, address)` total order).
//! - **L6 Strict-total-order.** For any two adjacent actions in the
//!   sequence, they target distinct `(portfolio_address, action_label)`
//!   tuples — no ambiguity about which is "next".

use std::cmp::Reverse;

use crate::state::{KeeperAction, PortfolioSnapshot};

/// One scheduled recovery step in the deterministic queue. Pairs
/// the action with the portfolio it targets so the keeper can
/// match it back against the snapshot it read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledRecovery {
    /// Portfolio address (base58 string, matches `PortfolioSnapshot`).
    pub portfolio_address: String,
    /// Cap on what this step is allowed to ask the engine for —
    /// always `≤ cert.certified_liq_deficit`.
    pub b_delta_budget: i128,
    /// The action to submit on-chain.
    pub action: KeeperAction,
}

#[derive(Debug, Clone, Copy)]
pub struct LiquidationPolicy {
    /// Minimum certified deficit to consider acting. Default `1` —
    /// any non-zero deficit is actionable. Operators bump this to
    /// avoid dust liquidations.
    pub min_deficit: u128,
}

impl Default for LiquidationPolicy {
    fn default() -> Self {
        Self { min_deficit: 1 }
    }
}

impl LiquidationPolicy {
    /// Build the deterministic liquidation queue.
    ///
    /// Returns the actions ordered `(deficit DESC, address ASC)`.
    /// Drops any portfolio with `cert.valid = false` (§16 fail-closed)
    /// or below `min_deficit` or with `asset_in_distress = None`.
    pub fn sequence(&self, portfolios: &[PortfolioSnapshot]) -> Vec<ScheduledRecovery> {
        let mut admissible: Vec<&PortfolioSnapshot> = portfolios
            .iter()
            .filter(|p| Self::admissible(p, self.min_deficit))
            .collect();
        admissible.sort_by(|a, b| {
            Reverse(a.health_cert.certified_liq_deficit)
                .cmp(&Reverse(b.health_cert.certified_liq_deficit))
                .then_with(|| a.portfolio_address.cmp(&b.portfolio_address))
        });
        admissible
            .into_iter()
            .filter_map(|p| {
                let asset_index = p.asset_in_distress?;
                let b_delta_budget = clamp_to_i128(p.health_cert.certified_liq_deficit);
                Some(ScheduledRecovery {
                    portfolio_address: p.portfolio_address.clone(),
                    b_delta_budget,
                    action: KeeperAction::RecoveryForfeitLeg {
                        asset_index,
                        b_delta_budget,
                    },
                })
            })
            .collect()
    }

    fn admissible(p: &PortfolioSnapshot, min_deficit: u128) -> bool {
        p.health_cert.valid
            && p.health_cert.certified_liq_deficit >= min_deficit
            && p.asset_in_distress.is_some()
    }
}

/// Clamp `u128` to non-negative `i128`. The engine's deficit field
/// is `u128`; our action's budget is `i128` (matches engine math).
/// Saturating cast — values above `i128::MAX` clamp down so the
/// conversion is total.
fn clamp_to_i128(v: u128) -> i128 {
    v.min(i128::MAX as u128) as i128
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::risk::HealthCertV16;

    fn snap(addr: &str, valid: bool, deficit: u128, asset: Option<u16>) -> PortfolioSnapshot {
        PortfolioSnapshot {
            portfolio_address: addr.into(),
            health_cert: HealthCertV16 {
                valid,
                certified_liq_deficit: deficit,
                ..HealthCertV16::default()
            },
            asset_in_distress: asset,
        }
    }

    // ---------- L1: Determinism ----------

    #[test]
    fn determinism_same_input_same_output() {
        let policy = LiquidationPolicy::default();
        let snaps = vec![
            snap("AAA", true, 100, Some(0)),
            snap("BBB", true, 200, Some(1)),
        ];
        let a = policy.sequence(&snaps);
        let b = policy.sequence(&snaps);
        assert_eq!(a, b);
    }

    // ---------- L2 / L3: Bounded budgets ----------

    #[test]
    fn per_step_budget_bounded_by_cert_deficit() {
        let policy = LiquidationPolicy::default();
        let snaps = vec![
            snap("AAA", true, 100, Some(0)),
            snap("BBB", true, 5_000_000, Some(1)),
        ];
        let seq = policy.sequence(&snaps);
        for step in &seq {
            // Find matching snap by address; b_delta_budget never exceeds
            // its cert's deficit.
            let s = snaps
                .iter()
                .find(|s| s.portfolio_address == step.portfolio_address)
                .unwrap();
            assert!(step.b_delta_budget as u128 <= s.health_cert.certified_liq_deficit);
        }
    }

    #[test]
    fn total_budget_bounded_by_total_deficit() {
        let policy = LiquidationPolicy::default();
        let snaps = vec![
            snap("AAA", true, 100, Some(0)),
            snap("BBB", true, 500, Some(1)),
            snap("CCC", true, 9_999, Some(2)),
        ];
        let seq = policy.sequence(&snaps);
        let total_budget: i128 = seq.iter().map(|s| s.b_delta_budget).sum();
        let total_deficit: u128 = snaps.iter().map(|s| s.health_cert.certified_liq_deficit).sum();
        assert!(total_budget as u128 <= total_deficit);
    }

    // ---------- L4: Fail-closed ----------

    #[test]
    fn invalid_cert_dropped() {
        let policy = LiquidationPolicy::default();
        let snaps = vec![
            snap("AAA", false, 5_000, Some(0)), // stale cert — dropped
            snap("BBB", true, 100, Some(1)),
        ];
        let seq = policy.sequence(&snaps);
        assert_eq!(seq.len(), 1);
        assert_eq!(seq[0].portfolio_address, "BBB");
    }

    #[test]
    fn below_min_deficit_dropped() {
        let policy = LiquidationPolicy { min_deficit: 1_000 };
        let snaps = vec![
            snap("AAA", true, 100, Some(0)), // below floor
            snap("BBB", true, 5_000, Some(1)),
        ];
        let seq = policy.sequence(&snaps);
        assert_eq!(seq.len(), 1);
        assert_eq!(seq[0].portfolio_address, "BBB");
    }

    #[test]
    fn missing_asset_in_distress_dropped() {
        let policy = LiquidationPolicy::default();
        let snaps = vec![
            snap("AAA", true, 5_000, None), // no target asset
            snap("BBB", true, 100, Some(1)),
        ];
        let seq = policy.sequence(&snaps);
        assert_eq!(seq.len(), 1);
        assert_eq!(seq[0].portfolio_address, "BBB");
    }

    // ---------- L5: Input-shuffle-invariant ----------

    #[test]
    fn input_order_does_not_affect_output() {
        let policy = LiquidationPolicy::default();
        let s1 = snap("Aa", true, 500, Some(0));
        let s2 = snap("Bb", true, 1_500, Some(1));
        let s3 = snap("Cc", true, 100, Some(2));
        let a = policy.sequence(&[s1.clone(), s2.clone(), s3.clone()]);
        let b = policy.sequence(&[s3.clone(), s1.clone(), s2.clone()]);
        let c = policy.sequence(&[s2.clone(), s3.clone(), s1.clone()]);
        assert_eq!(a, b);
        assert_eq!(b, c);
        // And the order is by deficit DESC.
        let deficits: Vec<i128> = a.iter().map(|x| x.b_delta_budget).collect();
        let mut sorted = deficits.clone();
        sorted.sort_unstable_by(|x, y| y.cmp(x));
        assert_eq!(deficits, sorted);
    }

    // ---------- L6: Strict total order on equal-deficit ----------

    #[test]
    fn equal_deficit_breaks_ties_by_address() {
        let policy = LiquidationPolicy::default();
        let snaps = vec![
            snap("ZZZ", true, 100, Some(0)),
            snap("AAA", true, 100, Some(1)),
            snap("MMM", true, 100, Some(2)),
        ];
        let seq = policy.sequence(&snaps);
        let addrs: Vec<&str> = seq.iter().map(|s| s.portfolio_address.as_str()).collect();
        // Equal deficits → address tiebreak ascending.
        assert_eq!(addrs, vec!["AAA", "MMM", "ZZZ"]);
    }

    /// The motivating §21 property: any two keepers seeing the same
    /// distressed set agree on EVERY step's identity, in order.
    /// Two keepers reading the same on-chain snapshot would call
    /// `sequence` on the same input and the strict total order
    /// gives them the same queue.
    #[test]
    fn two_keepers_agree_on_full_queue_no_livelock() {
        let policy = LiquidationPolicy::default();
        let snaps = vec![
            snap("Acc7", true, 7_000, Some(2)),
            snap("Acc2", true, 2_000, Some(0)),
            snap("Acc9", true, 9_000, Some(1)),
            snap("Acc4", true, 4_000, Some(0)),
        ];
        // Keeper A reads the snap as-is; Keeper B reads it (perhaps
        // from a different RPC ordering) shuffled.
        let mut snaps_b = snaps.clone();
        snaps_b.reverse();

        let seq_a = policy.sequence(&snaps);
        let seq_b = policy.sequence(&snaps_b);
        assert_eq!(seq_a, seq_b);
        // Queue order is deficit-descending.
        let addrs: Vec<&str> = seq_a.iter().map(|s| s.portfolio_address.as_str()).collect();
        assert_eq!(addrs, vec!["Acc9", "Acc7", "Acc4", "Acc2"]);
    }

    // ---------- Edge cases ----------

    #[test]
    fn empty_input_empty_output() {
        let policy = LiquidationPolicy::default();
        assert!(policy.sequence(&[]).is_empty());
    }

    #[test]
    fn deficit_at_u128_max_clamps_to_i128_max() {
        let policy = LiquidationPolicy::default();
        let snap_max = snap("X", true, u128::MAX, Some(0));
        let seq = policy.sequence(&[snap_max]);
        assert_eq!(seq[0].b_delta_budget, i128::MAX);
    }
}
