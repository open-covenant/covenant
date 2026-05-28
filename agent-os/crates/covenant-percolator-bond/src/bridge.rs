//! `KeeperScope` (the operator-facing capability scope) ↔ `BondScope`
//! (the on-chain canonical scope) bridge.
//!
//! Gated behind `--features bridge` so the bond crate stays
//! scope-light by default. With the feature on, operators can pin
//! the *exact* scope they signed in a capability into the bond
//! account by computing `BondScope::from(&keeper_scope).hash()`.
//!
//! The conversion is faithful — same semantics on both sides:
//!   - market base58 → 32-byte pubkey
//!   - `Option<Vec<AssetIndex>>` carried through verbatim
//!   - `Option<Vec<ActionLabel>>` converted to bitmask; `None`
//!     (any) maps to `ActionMask::all()`
//!   - `Option<u32>` cap → `u32` (None → 0, meaning unbounded)

use covenant_percolator::capability::KeeperScope;
use covenant_percolator::state::ActionLabel;

use crate::scope::{ActionMask, BondScope};

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("market base58 decode: {0}")]
    MarketDecode(String),
    #[error("market pubkey must be 32 bytes, got {0}")]
    MarketWrongSize(usize),
    #[error("scope version {0} unsupported (bond expects v1)")]
    UnsupportedVersion(u8),
}

impl BondScope {
    /// Build a `BondScope` from an operator-signed `KeeperScope`.
    /// Returns the scope (the caller hashes it via [`BondScope::hash`]
    /// to compare against `BondAccount::scope_hash`).
    pub fn from_keeper_scope(s: &KeeperScope) -> Result<Self, BridgeError> {
        if s.version != 1 {
            return Err(BridgeError::UnsupportedVersion(s.version));
        }
        let market_bytes = bs58::decode(&s.market)
            .into_vec()
            .map_err(|e| BridgeError::MarketDecode(e.to_string()))?;
        if market_bytes.len() != 32 {
            return Err(BridgeError::MarketWrongSize(market_bytes.len()));
        }
        let mut market = [0u8; 32];
        market.copy_from_slice(&market_bytes);

        let mask = match &s.allowed_actions {
            None => ActionMask::all(),
            Some(labels) => {
                let mut m = 0u8;
                for l in labels {
                    m |= match l {
                        ActionLabel::PushMark => ActionMask::PUSH_MARK,
                        ActionLabel::Crank => ActionMask::CRANK,
                        ActionLabel::Recover => ActionMask::RECOVER,
                    };
                }
                ActionMask(m)
            }
        };

        Ok(BondScope {
            version: 1,
            market,
            allowed_actions: mask,
            allowed_assets: s.allowed_assets.clone(),
            // None (no per-tick cap) → 0, meaning unbounded. The bond
            // verifier doesn't enforce this field; it's stored for
            // round-trip transparency.
            max_actions_per_tick: s.max_actions_per_tick.unwrap_or(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_percolator::state::ActionLabel;

    fn k(market: &str) -> KeeperScope {
        KeeperScope {
            version: 1,
            market: market.into(),
            allowed_assets: Some(vec![0, 1]),
            allowed_actions: Some(vec![ActionLabel::PushMark, ActionLabel::Crank]),
            max_actions_per_tick: Some(8),
        }
    }

    #[test]
    fn faithful_conversion_for_typical_scope() {
        let ks = k("4m3ipBQDYX6JQ9YSmUXDjESDHMtGWtiXforkWr9Qoxdi");
        let bs = BondScope::from_keeper_scope(&ks).unwrap();
        assert_eq!(bs.version, 1);
        assert_eq!(bs.market.len(), 32);
        assert_eq!(
            bs.allowed_actions.0,
            ActionMask::PUSH_MARK | ActionMask::CRANK
        );
        assert_eq!(bs.allowed_assets, Some(vec![0, 1]));
        assert_eq!(bs.max_actions_per_tick, 8);
    }

    /// The semantic-divergence guard: `KeeperScope { allowed_assets:
    /// Some(vec![]) }` (deny all) MUST NOT become "any allowed" in
    /// the bond. With the Option<Vec> shape this is automatic.
    #[test]
    fn empty_some_in_keeper_stays_empty_some_in_bond() {
        let ks = KeeperScope {
            version: 1,
            market: "4m3ipBQDYX6JQ9YSmUXDjESDHMtGWtiXforkWr9Qoxdi".into(),
            allowed_assets: Some(vec![]),
            allowed_actions: Some(vec![ActionLabel::Crank]),
            max_actions_per_tick: None,
        };
        let bs = BondScope::from_keeper_scope(&ks).unwrap();
        assert_eq!(bs.allowed_assets, Some(vec![]));
        // A keeper acting on asset 0 is a violation — verified by the
        // bond's scope predicate.
        assert!(!bs.allows(&bs.market, ActionMask::CRANK, Some(0)));
    }

    #[test]
    fn none_assets_means_any_through_the_bridge() {
        let mut ks = k("4m3ipBQDYX6JQ9YSmUXDjESDHMtGWtiXforkWr9Qoxdi");
        ks.allowed_assets = None;
        let bs = BondScope::from_keeper_scope(&ks).unwrap();
        assert_eq!(bs.allowed_assets, None);
        assert!(bs.allows(&bs.market, ActionMask::CRANK, Some(99)));
    }

    #[test]
    fn unsupported_version_rejected() {
        let mut ks = k("4m3ipBQDYX6JQ9YSmUXDjESDHMtGWtiXforkWr9Qoxdi");
        ks.version = 2;
        assert!(matches!(
            BondScope::from_keeper_scope(&ks),
            Err(BridgeError::UnsupportedVersion(2))
        ));
    }

    #[test]
    fn malformed_market_string_rejected() {
        let mut ks = k("not base58 !!");
        ks.market = "not valid".into();
        let err = BondScope::from_keeper_scope(&ks).unwrap_err();
        assert!(matches!(err, BridgeError::MarketDecode(_)));
    }

    /// Two operators independently constructing the same KeeperScope
    /// arrive at the same bond hash — the canonical encoding makes
    /// the bond verifiable from operator-signed JSON alone.
    #[test]
    fn deterministic_hash_across_conversions() {
        let ks1 = k("4m3ipBQDYX6JQ9YSmUXDjESDHMtGWtiXforkWr9Qoxdi");
        let ks2 = k("4m3ipBQDYX6JQ9YSmUXDjESDHMtGWtiXforkWr9Qoxdi");
        let h1 = BondScope::from_keeper_scope(&ks1).unwrap().hash();
        let h2 = BondScope::from_keeper_scope(&ks2).unwrap().hash();
        assert_eq!(h1, h2);
    }
}
