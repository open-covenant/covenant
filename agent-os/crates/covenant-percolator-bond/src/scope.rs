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
/// `allowed_assets` is `Vec<u16>` (empty = "any asset"). The wire
/// encoding length-prefixes it, so `[]` and a one-asset list of `[0]`
/// hash differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BondScope {
    pub version: u8,
    pub market: [u8; 32],
    pub allowed_actions: ActionMask,
    pub allowed_assets: Vec<u16>,
    pub max_actions_per_tick: u32,
}

impl BondScope {
    /// Deterministic canonical encoding. Format:
    ///   u8(version)
    ///   32B(market)
    ///   u8(action_mask)
    ///   u16le(asset_count) | u16le * count(asset_indexes, ascending)
    ///   u32le(max_actions_per_tick)
    ///
    /// Asset indexes are sorted ascending so the slasher can't
    /// reorder a permutation into a "different" scope. Duplicate
    /// indexes are kept verbatim (the hash will differ for `[0,0]`
    /// vs `[0]`); the verifier treats both as covering asset 0.
    pub fn encode_canonical(&self) -> Vec<u8> {
        let mut sorted = self.allowed_assets.clone();
        sorted.sort_unstable();
        let mut out =
            Vec::with_capacity(1 + 32 + 1 + 2 + 2 * sorted.len() + 4);
        out.push(self.version);
        out.extend_from_slice(&self.market);
        out.push(self.allowed_actions.0);
        out.extend_from_slice(&(sorted.len() as u16).to_le_bytes());
        for a in &sorted {
            out.extend_from_slice(&a.to_le_bytes());
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

    /// Mirror of `KeeperScope::allows` over the canonical mask. Returns
    /// `true` if (market, action_bit, asset_index) is permitted.
    pub fn allows(&self, market: &[u8; 32], action_bit: u8, asset_index: Option<u16>) -> bool {
        if &self.market != market {
            return false;
        }
        if !self.allowed_actions.allows(action_bit) {
            return false;
        }
        if let Some(idx) = asset_index {
            if !self.allowed_assets.is_empty() && !self.allowed_assets.contains(&idx) {
                return false;
            }
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

    fn s(market_byte: u8, mask: u8, assets: &[u16], cap: u32) -> BondScope {
        BondScope {
            version: 1,
            market: [market_byte; 32],
            allowed_actions: ActionMask(mask),
            allowed_assets: assets.to_vec(),
            max_actions_per_tick: cap,
        }
    }

    #[test]
    fn canonical_encoding_layout_locked() {
        let scope = s(0x01, ActionMask::PUSH_MARK | ActionMask::CRANK, &[7, 3], 12);
        let bytes = scope.encode_canonical();
        assert_eq!(bytes[0], 1);
        assert_eq!(&bytes[1..33], &[0x01u8; 32]);
        assert_eq!(bytes[33], 0b011);
        assert_eq!(&bytes[34..36], &2u16.to_le_bytes());
        // Sorted ascending: 3 then 7.
        assert_eq!(&bytes[36..38], &3u16.to_le_bytes());
        assert_eq!(&bytes[38..40], &7u16.to_le_bytes());
        assert_eq!(&bytes[40..44], &12u32.to_le_bytes());
        assert_eq!(bytes.len(), 1 + 32 + 1 + 2 + 4 + 4);
    }

    #[test]
    fn asset_order_does_not_change_hash() {
        let a = s(2, ActionMask::CRANK, &[1, 2, 3], 4);
        let b = s(2, ActionMask::CRANK, &[3, 2, 1], 4);
        assert_eq!(a.hash(), b.hash());
    }

    #[test]
    fn distinct_inputs_produce_distinct_hashes() {
        // Each field flips independently.
        let base = s(0, ActionMask::CRANK, &[0], 1);
        let h0 = base.hash();
        let h_market = s(1, ActionMask::CRANK, &[0], 1).hash();
        let h_mask = s(0, ActionMask::PUSH_MARK, &[0], 1).hash();
        let h_asset = s(0, ActionMask::CRANK, &[1], 1).hash();
        let h_cap = s(0, ActionMask::CRANK, &[0], 2).hash();
        assert_ne!(h0, h_market);
        assert_ne!(h0, h_mask);
        assert_ne!(h0, h_asset);
        assert_ne!(h0, h_cap);
    }

    #[test]
    fn empty_assets_means_any() {
        let scope = s(0, ActionMask::CRANK, &[], 1);
        assert!(scope.allows(&[0; 32], ActionMask::CRANK, Some(99)));
    }

    #[test]
    fn restricted_assets_block_unlisted() {
        let scope = s(0, ActionMask::CRANK, &[0, 1], 1);
        assert!(scope.allows(&[0; 32], ActionMask::CRANK, Some(0)));
        assert!(!scope.allows(&[0; 32], ActionMask::CRANK, Some(2)));
    }
}
