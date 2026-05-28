//! Decision policy: when to push a mark, when to crank. Pure
//! function — no side effects, no capability checks, no spend. The
//! keeper agent invokes [`KeeperPolicy::decide`] and then runs each
//! returned action through the capability + budget gates before
//! executing.

use crate::state::{AssetLifecycle, KeeperAction, MarketState};

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
            let age = market.current_slot.saturating_sub(asset.last_mark_slot);
            if age > self.max_staleness_slots {
                // The policy doesn't price marks — it carries the last
                // known value forward. A real keeper plugs in a fresh
                // oracle read between decide and execute.
                out.push(KeeperAction::PushHyperpMark {
                    asset_index: asset.index,
                    mark_e6: asset.last_mark_e6,
                });
            }
        }
        let crank_age = market
            .current_slot
            .saturating_sub(market.last_crank_slot);
        if crank_age >= self.crank_interval_slots {
            out.push(KeeperAction::Crank);
        }
        out
    }
}
