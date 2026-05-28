//! Capability scope for `percolator.keeper`. The scope is the
//! operator-signed authority that bounds *what* a keeper agent may do:
//! which market, which assets, which verbs, how many actions per tick.
//! The scope is enforced before any spend or on-chain action.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::{ActionLabel, AssetIndex, KeeperAction};
use crate::PercolatorError;

/// Capability action namespace for keeper agents. The capability the
/// operator grants carries `action = "percolator.keeper"` and a scope
/// of the [`KeeperScope`] shape below.
pub const ACTION_KEEPER: &str = "percolator.keeper";

/// Operator-signed authority bounding what a keeper agent may do.
/// Stored as the capability's `scope` JSON; parsed with
/// [`KeeperScope::parse`] and checked via [`KeeperScope::allows`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeeperScope {
    /// Scope envelope version. Only `1` is accepted today.
    pub version: u8,
    /// Base58 pubkey of the market the keeper may operate on. Any
    /// action targeting a different market is rejected.
    pub market: String,
    /// Asset indices the keeper may touch. `None` means "any active
    /// asset" — operators usually pin this.
    #[serde(default)]
    pub allowed_assets: Option<Vec<AssetIndex>>,
    /// Verb-level allowlist. `None` means all keeper verbs are allowed.
    /// Granting `push_mark` and `crank` without `recover` is the
    /// common "freshener / cranker, no liquidations" shape.
    #[serde(default)]
    pub allowed_actions: Option<Vec<ActionLabel>>,
    /// Hard cap on actions per tick. Prevents a hostile/buggy policy
    /// from emptying the budget in one pass.
    #[serde(default)]
    pub max_actions_per_tick: Option<u32>,
}

impl KeeperScope {
    /// Parse a raw capability scope into a typed [`KeeperScope`].
    pub fn parse(scope: &Value) -> Result<Self, PercolatorError> {
        let parsed: KeeperScope = serde_json::from_value(scope.clone())
            .map_err(|e| PercolatorError::Scope(format!("decode keeper scope: {e}")))?;
        if parsed.version != 1 {
            return Err(PercolatorError::Scope(format!(
                "unsupported keeper scope version {}",
                parsed.version
            )));
        }
        Ok(parsed)
    }

    /// True iff `action` against `market` is permitted by this scope.
    /// Called per-action by the keeper *before* any budget debit or
    /// on-chain submission.
    pub fn allows(&self, market: &str, action: &KeeperAction) -> bool {
        if self.market != market {
            return false;
        }
        if let Some(actions) = &self.allowed_actions {
            if !actions.contains(&action.action_label()) {
                return false;
            }
        }
        if let Some(assets) = &self.allowed_assets {
            if let Some(target) = action.target_asset() {
                if !assets.contains(&target) {
                    return false;
                }
            }
            // Crank carries no target asset — the action-label gate
            // above governs it.
        }
        true
    }
}
