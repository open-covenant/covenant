//! Canonical scope representation for bonded keepers.
//!
//! The keeper crate's `KeeperScope` is operator-facing (JSON-friendly,
//! `Option<Vec<_>>` fields, capabilities-aware). The bond program
//! stores only the *hash* of a canonical, deterministic encoding —
//! the slasher provides the full scope alongside evidence and the
//! hash is recomputed and compared.
//!
//! Encoding is hand-rolled (little-endian, length-prefixed) so the
//! on-chain side has no serde dependency and the bytes are stable
//! across Rust toolchains. Layout is locked by tests below.

use sha2::{Digest, Sha256};

/// Bit mask over the three keeper action labels. Matches
/// `covenant_percolator::state::ActionLabel`:
///   - bit 0 → push_mark
///   - bit 1 → crank
///   - bit 2 → recover
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ActionMask(pub u8);

impl ActionMask {
    pub const PUSH_MARK: u8 = 1 << 0;
    pub const CRANK: u8 = 1 << 1;
    pub const RECOVER: u8 = 1 << 2;

    pub fn empty() -> Self {
        Self(0)
    }
    pub fn all() -> Self {
        Self(Self::PUSH_MARK | Self::CRANK | Self::RECOVER)
    }
    pub fn allows(&self, bit: u8) -> bool {
        self.0 & bit == bit
    }
}

/// Canonical scope. Compared at slash time against the stored hash;
/// the slasher submits this struct verbatim and the program
/// recomputes the hash to confirm authenticity.
///
/// `allowed_assets` is `Option<Vec<u16>>` — `None` = "any asset",
/// `Some(vec![])` = "deny all assets" (explicit), `Some([a, b])` =
/// "this whitelist". This is the SAME semantics as
/// `covenant_percolator::KeeperScope::allowed_assets`, so the
/// bridge in [`BondScope::from_keeper_scope`] is faithful — empty
/// can't accidentally widen to "any".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BondScope {
    pub version: u8,
    pub market: [u8; 32],
    pub allowed_actions: ActionMask,
    pub allowed_assets: Option<Vec<u16>>,
    pub max_actions_per_tick: u32,
}

impl BondScope {
    /// Deterministic canonical encoding. Format:
    ///   u8(version)
    ///   32B(market)
    ///   u8(action_mask)
    ///   u8(asset_kind: 0=None/"any", 1=Some)
    ///   if asset_kind == 1:
    ///     u16le(asset_count) | u16le * count(asset_indexes, ascending)
    ///   u32le(max_actions_per_tick)
    ///
    /// `None` and `Some(vec![])` produce DIFFERENT canonical bytes
    /// (and therefore different hashes), so the verifier cannot
    /// confuse "any" with "deny all".
    ///
    /// Asset indexes are sorted ascending so the slasher can't
    /// reorder a permutation into a "different" scope. Duplicate
    /// indexes hash differently (the verifier treats `[0,0]` and
    /// `[0]` as both covering asset 0).
    pub fn encode_canonical(&self) -> Vec<u8> {
        let asset_count = self.allowed_assets.as_ref().map(|v| v.len()).unwrap_or(0);
        let mut out =
            Vec::with_capacity(1 + 32 + 1 + 1 + 2 + 2 * asset_count + 4);
        out.push(self.version);
        out.extend_from_slice(&self.market);
        out.push(self.allowed_actions.0);
        match &self.allowed_assets {
            None => out.push(0),
            Some(v) => {
                out.push(1);
                let mut sorted = v.clone();
                sorted.sort_unstable();
                out.extend_from_slice(&(sorted.len() as u16).to_le_bytes());
                for a in &sorted {
                    out.extend_from_slice(&a.to_le_bytes());
                }
            }
        }
        out.extend_from_slice(&self.max_actions_per_tick.to_le_bytes());
        out
    }

    pub fn hash(&self) -> ScopeHash {
        let mut h = Sha256::new();
        h.update(self.encode_canonical());
        let out = h.finalize();
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&out);
        ScopeHash(arr)
    }

    /// Mirror of `KeeperScope::allows`. Returns `true` if
    /// `(market, action_bit, asset_index)` is permitted by this scope.
    ///
    /// Semantics:
    ///   - `allowed_assets = None` → any asset allowed (no constraint)
    ///   - `allowed_assets = Some([])` → no assets allowed (deny all)
    ///   - `allowed_assets = Some([0,1])` → only the listed assets
    ///
    /// For actions with no `asset_index` (none currently — every v16
    /// keeper action targets an asset), the asset gate is skipped.
    pub fn allows(&self, market: &[u8; 32], action_bit: u8, asset_index: Option<u16>) -> bool {
        if &self.market != market {
            return false;
        }
        if !self.allowed_actions.allows(action_bit) {
            return false;
        }
        if let Some(idx) = asset_index {
            if let Some(allowed) = &self.allowed_assets {
                if !allowed.contains(&idx) {
                    return false;
                }
            }
            // None → any asset allowed; fall through.
        }
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScopeHash(pub [u8; 32]);

impl ScopeHash {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(market_byte: u8, mask: u8, assets: Option<&[u16]>, cap: u32) -> BondScope {
        BondScope {
            version: 1,
            market: [market_byte; 32],
            allowed_actions: ActionMask(mask),
            allowed_assets: assets.map(|a| a.to_vec()),
            max_actions_per_tick: cap,
        }
    }

    #[test]
    fn canonical_encoding_layout_locked_some_variant() {
        let scope = s(
            0x01,
            ActionMask::PUSH_MARK | ActionMask::CRANK,
            Some(&[7, 3]),
            12,
        );
        let bytes = scope.encode_canonical();
        assert_eq!(bytes[0], 1); // version
        assert_eq!(&bytes[1..33], &[0x01u8; 32]); // market
        assert_eq!(bytes[33], 0b011); // mask
        assert_eq!(bytes[34], 1); // asset_kind = Some
        assert_eq!(&bytes[35..37], &2u16.to_le_bytes()); // asset_count
        // Sorted ascending: 3 then 7.
        assert_eq!(&bytes[37..39], &3u16.to_le_bytes());
        assert_eq!(&bytes[39..41], &7u16.to_le_bytes());
        assert_eq!(&bytes[41..45], &12u32.to_le_bytes()); // cap
        assert_eq!(bytes.len(), 1 + 32 + 1 + 1 + 2 + 4 + 4);
    }

    #[test]
    fn canonical_encoding_layout_locked_none_variant() {
        let scope = s(0x02, ActionMask::CRANK, None, 5);
        let bytes = scope.encode_canonical();
        assert_eq!(bytes[0], 1); // version
        assert_eq!(&bytes[1..33], &[0x02u8; 32]); // market
        assert_eq!(bytes[33], 0b010); // mask
        assert_eq!(bytes[34], 0); // asset_kind = None
        assert_eq!(&bytes[35..39], &5u32.to_le_bytes()); // cap (no asset block)
        assert_eq!(bytes.len(), 1 + 32 + 1 + 1 + 4);
    }

    /// The whole point of switching from `Vec<u16>` to
    /// `Option<Vec<u16>>` — `None` (any) and `Some(vec![])` (deny all)
    /// must hash differently so a malicious slasher can't swap one for
    /// the other.
    #[test]
    fn none_and_empty_some_have_distinct_hashes() {
        let any = s(0, ActionMask::CRANK, None, 1);
        let deny_all = s(0, ActionMask::CRANK, Some(&[]), 1);
        assert_ne!(any.hash(), deny_all.hash());
    }

    #[test]
    fn asset_order_does_not_change_hash() {
        let a = s(2, ActionMask::CRANK, Some(&[1, 2, 3]), 4);
        let b = s(2, ActionMask::CRANK, Some(&[3, 2, 1]), 4);
        assert_eq!(a.hash(), b.hash());
    }

    #[test]
    fn distinct_inputs_produce_distinct_hashes() {
        let base = s(0, ActionMask::CRANK, Some(&[0]), 1);
        let h0 = base.hash();
        let h_market = s(1, ActionMask::CRANK, Some(&[0]), 1).hash();
        let h_mask = s(0, ActionMask::PUSH_MARK, Some(&[0]), 1).hash();
        let h_asset = s(0, ActionMask::CRANK, Some(&[1]), 1).hash();
        let h_cap = s(0, ActionMask::CRANK, Some(&[0]), 2).hash();
        assert_ne!(h0, h_market);
        assert_ne!(h0, h_mask);
        assert_ne!(h0, h_asset);
        assert_ne!(h0, h_cap);
    }

    #[test]
    fn none_means_any_asset_allowed() {
        let scope = s(0, ActionMask::CRANK, None, 1);
        assert!(scope.allows(&[0; 32], ActionMask::CRANK, Some(99)));
        assert!(scope.allows(&[0; 32], ActionMask::CRANK, None));
    }

    #[test]
    fn some_empty_denies_all_assets_with_a_target() {
        let scope = s(0, ActionMask::CRANK, Some(&[]), 1);
        // Any specific asset target is denied.
        assert!(!scope.allows(&[0; 32], ActionMask::CRANK, Some(0)));
        assert!(!scope.allows(&[0; 32], ActionMask::CRANK, Some(99)));
        // Action with no target still passes the asset gate (gated
        // by the action mask only).
        assert!(scope.allows(&[0; 32], ActionMask::CRANK, None));
    }

    #[test]
    fn restricted_assets_block_unlisted() {
        let scope = s(0, ActionMask::CRANK, Some(&[0, 1]), 1);
        assert!(scope.allows(&[0; 32], ActionMask::CRANK, Some(0)));
        assert!(!scope.allows(&[0; 32], ActionMask::CRANK, Some(2)));
    }
}
