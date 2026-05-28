//! Decision policy: when to push a mark, when to crank. Pure
//! function — no side effects, no capability checks, no spend. The
//! keeper agent invokes [`KeeperPolicy::decide`] and then runs each
//! returned action through the capability + budget gates before
//! executing.

use crate::state::{AssetLifecycle, KeeperAction, MarketState, PortfolioSnapshot};

#[derive(Debug, Clone, Copy)]
pub struct KeeperPolicy {
    /// Push a fresh mark for an asset if its last mark is older than
    /// this many slots. Honors v16's per-asset staleness gating —
    /// each asset is independently fresh or stale.
    pub max_staleness_slots: u64,
    /// Run the permissionless crank if the market hasn't been cranked
    /// for at least this many slots.
    pub crank_interval_slots: u64,
}

impl Default for KeeperPolicy {
    fn default() -> Self {
        // ~60s freshness @ 400ms slots; crank every ~2 minutes.
        Self {
            max_staleness_slots: 150,
            crank_interval_slots: 300,
        }
    }
}

impl KeeperPolicy {
    /// Decide the actions to take for the current market state. Pure.
    /// Order: stale-mark pushes first (unblocks gated operations),
    /// then the crank.
    pub fn decide(&self, market: &MarketState) -> Vec<KeeperAction> {
        let mut out = Vec::new();
        for asset in &market.assets {
            if !matches!(asset.lifecycle, AssetLifecycle::Active) {
                continue;
            }
            // Consistent `>=` semantics: at exactly `max_staleness_slots`
            // the mark is considered due. The crank-interval check
            // below uses the same convention so an operator-set value
            // means "act when AT LEAST this many slots have passed".
            let age = market.current_slot.saturating_sub(asset.last_mark_slot);
            if age >= self.max_staleness_slots {
                // The policy doesn't price marks — it carries the last
                // known value forward. A real keeper plugs in a fresh
                // oracle read between decide and execute.
                out.push(KeeperAction::PushAuthMark {
                    asset_index: asset.index,
                    mark_e6: asset.last_mark_e6,
                });
            }
        }
        // Percolator-prog cranks ONE asset per IX, so a full-market
        // freshen is N actions — one per Active asset. The crank
        // interval is a market-global cadence; the fan-out happens
        // here so coordination peers can split work asset-by-asset.
        let crank_age = market
            .current_slot
            .saturating_sub(market.last_crank_slot);
        if crank_age >= self.crank_interval_slots {
            for asset in &market.assets {
                if matches!(asset.lifecycle, AssetLifecycle::Active) {
                    out.push(KeeperAction::CrankAsset {
                        asset_index: asset.index,
                    });
                }
            }
        }
        out
    }
}

/// Recovery policy: when a portfolio's engine-issued health certificate
/// indicates a non-zero `certified_liq_deficit`, emit the first leg of
/// the permissionless-recovery sequence (`ForfeitRecoveryLeg`). The
/// keeper does NOT compute health — it consults the engine's cert. The
/// subsequent `RebalanceReduce` / `FinalizeResetSide` legs are driven
/// on the next tick after the program advances the close ledger.
///
/// Fail-closed on stale certs (spec §16 "stale backing fails closed"):
/// `valid=false` is treated as "do nothing", deferring to engine
/// freshness rather than the keeper second-guessing.
///
/// `b_delta_budget` is bounded by `certified_liq_deficit`, so the
/// keeper cannot ask the program for more recovery than the engine
/// already certified as needed.
#[derive(Debug, Clone, Copy)]
pub struct RecoveryPolicy {
    /// Minimum deficit (in engine raw units) before the keeper acts.
    /// Default `1` — any non-zero deficit is actionable.
    pub min_deficit: u128,
}

impl Default for RecoveryPolicy {
    fn default() -> Self {
        Self { min_deficit: 1 }
    }
}

impl RecoveryPolicy {
    /// Delegates to [`crate::liquidation::LiquidationPolicy::sequence`]
    /// so the per-tick recovery flow inherits the §21 strict total
    /// order (deficit DESC, address ASC). Returns just the action
    /// list — operators wanting the full `ScheduledRecovery`
    /// (portfolio address + budget) call the sequencer directly.
    pub fn decide(&self, portfolios: &[PortfolioSnapshot]) -> Vec<KeeperAction> {
        let seq = crate::liquidation::LiquidationPolicy {
            min_deficit: self.min_deficit,
        }
        .sequence(portfolios);
        seq.into_iter().map(|s| s.action).collect()
    }
}
