//! Types mirroring the minimum slice of percolator-prog v16 that a
//! keeper agent needs to reason about and act on. Field names follow
//! the v16 wire shape (`AssetStateV16Account`, `PermissionlessCrank`,
//! `PushAuthMark`, `ForfeitRecoveryLeg`, `RebalanceReduce`,
//! `FinalizeResetSide`) so an `IDL`-driven `RealPercolator` can map
//! onto them without renames. `PushAuthMark` is the post-migration
//! manual-mark path (program tag 63); the retired `PushHyperpMark`
//! (tag 36) is intentionally absent.

use serde::{Deserialize, Serialize};

use crate::risk::HealthCertV16;

pub type AssetIndex = u16;

/// Per-asset lifecycle. Mirrors `percolator::AssetLifecycleV16`
/// variant-for-variant so the keeper's view aligns with the on-chain
/// state machine and `From<AssetLifecycleV16>` is total.
///
/// Only `Active` assets are eligible for the normal `push_mark`/`crank`
/// path. `Recovery` is what the keeper's recovery scope acts on;
/// `DrainOnly`/`Retired`/`Disabled`/`PendingActivation` are
/// admin-driven and the keeper takes no action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetLifecycle {
    Disabled,
    PendingActivation,
    Active,
    DrainOnly,
    Retired,
    Recovery,
}

/// Per-asset state — what the keeper reads to decide if a fresh mark
/// is needed. The `last_mark_slot` is the heart of v16's per-asset
/// staleness gating: each asset is independently fresh or stale, so a
/// keeper freshens only the assets it actually operates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetState {
    pub index: AssetIndex,
    pub label: String,
    pub lifecycle: AssetLifecycle,
    pub last_mark_slot: u64,
    pub last_mark_e6: u64,
}

/// Aggregate view of a percolator market the keeper operates on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketState {
    pub market_address: String,
    pub program_id: String,
    pub current_slot: u64,
    pub last_crank_slot: u64,
    pub assets: Vec<AssetState>,
}

impl MarketState {
    pub fn asset(&self, index: AssetIndex) -> Option<&AssetState> {
        self.assets.iter().find(|a| a.index == index)
    }
}

/// One on-chain action the keeper performs. Variants mirror the v16
/// keeper surface; the recovery trio stays separate so a capability
/// scope can grant `push_mark` and `crank` without unlocking recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KeeperAction {
    PushAuthMark {
        asset_index: AssetIndex,
        mark_e6: u64,
    },
    Crank,
    RecoveryForfeitLeg {
        asset_index: AssetIndex,
        b_delta_budget: i128,
    },
    RecoveryRebalanceReduce {
        asset_index: AssetIndex,
        reduce_q: i128,
    },
    RecoveryFinalize {
        asset_index: AssetIndex,
        side: u8,
    },
}

impl KeeperAction {
    /// Coarse label used by capability scopes (`push_mark`, `crank`,
    /// `recover`). Operators grant in terms of intent, not specific
    /// recovery sub-instructions.
    pub fn action_label(&self) -> ActionLabel {
        match self {
            KeeperAction::PushAuthMark { .. } => ActionLabel::PushMark,
            KeeperAction::Crank => ActionLabel::Crank,
            KeeperAction::RecoveryForfeitLeg { .. }
            | KeeperAction::RecoveryRebalanceReduce { .. }
            | KeeperAction::RecoveryFinalize { .. } => ActionLabel::Recover,
        }
    }

    pub fn target_asset(&self) -> Option<AssetIndex> {
        match self {
            KeeperAction::PushAuthMark { asset_index, .. }
            | KeeperAction::RecoveryForfeitLeg { asset_index, .. }
            | KeeperAction::RecoveryRebalanceReduce { asset_index, .. }
            | KeeperAction::RecoveryFinalize { asset_index, .. } => Some(*asset_index),
            KeeperAction::Crank => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ActionLabel {
    PushMark,
    Crank,
    Recover,
}

/// Read-only view of one portfolio account, as the keeper consults it.
/// The `health_cert` is the engine's own `HealthCertV16` — the keeper
/// *reads* it to decide, it does not recompute it. `asset_in_distress`
/// is the asset slot the program's close-progress ledger surfaces; the
/// real client derives it from portfolio state, the mock supplies it.
#[derive(Debug, Clone)]
pub struct PortfolioSnapshot {
    pub portfolio_address: String,
    pub health_cert: HealthCertV16,
    pub asset_in_distress: Option<AssetIndex>,
}
