//! Risk-engine integration. The keeper consults Toly's account-local
//! v16 risk engine (`aeyakovenko/percolator`) directly — we depend on
//! his crate and re-export the engine's lifecycle, health, and recovery
//! types so our policy decisions sit on the same invariants his Kani
//! suite proves upstream. This module is the typed adapter; the math
//! lives in his engine.

// Engine enums + structs that are unconditional under the default
// feature set. The Vec-based portfolio views (`PortfolioAccountV16`,
// `MarketGroupV16`) live behind the `runtime-vec-api` feature in the
// upstream crate and aren't re-exported here; consumers wanting them
// can enable that feature on `percolator` and import directly.
pub use percolator::{
    ADL_ONE, AssetLifecycleV16, BOUND_SCALE, CREDIT_RATE_SCALE, HealthCertV16, MarketModeV16,
    PermissionlessRecoveryReasonV16, POS_SCALE, SUPPORT_WEIGHT_SCALE, SideModeV16, SideV16,
    V16Error, V16Result, V16_MAX_PORTFOLIO_ASSETS_N,
};

use crate::state::AssetLifecycle;
use crate::state::AssetIndex;

/// Total conversion from the engine's lifecycle enum to the keeper's
/// view. The keeper only meaningfully distinguishes "may push a fresh
/// mark", "needs recovery sequence", and "no-op" — the mapping is
/// chosen so policy decisions stay in lockstep with the engine's state
/// machine.
impl From<AssetLifecycleV16> for AssetLifecycle {
    fn from(v: AssetLifecycleV16) -> Self {
        match v {
            AssetLifecycleV16::Disabled => AssetLifecycle::Disabled,
            AssetLifecycleV16::PendingActivation => AssetLifecycle::PendingActivation,
            AssetLifecycleV16::Active => AssetLifecycle::Active,
            AssetLifecycleV16::DrainOnly => AssetLifecycle::DrainOnly,
            AssetLifecycleV16::Retired => AssetLifecycle::Retired,
            AssetLifecycleV16::Recovery => AssetLifecycle::Recovery,
        }
    }
}

/// A condition surfaced by the engine that the keeper's recovery scope
/// may act on. Carries the per-asset target and the engine's reason
/// enum verbatim so the audit trail records `which` reason, not just
/// `that` recovery happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryTrigger {
    pub asset_index: AssetIndex,
    pub reason: PermissionlessRecoveryReasonV16,
}

/// Did the engine's health certificate cross into the recovery
/// envelope?  The keeper consults this BEFORE invoking any recovery
/// sub-instruction; "stale" certs (epoch mismatch) and certs marked
/// `valid = false` are conservatively treated as "do nothing"
/// (fail-closed, matching spec §16 "stale backing fails closed").
///
/// This is a thin predicate; the engine itself owns the math.
pub fn cert_signals_recovery(cert: &HealthCertV16) -> bool {
    cert.valid && cert.certified_liq_deficit > 0
}
