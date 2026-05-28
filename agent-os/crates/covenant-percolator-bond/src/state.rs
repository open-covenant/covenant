//! `BondAccount` — the on-chain bond record.
//!
//! Layout is hand-packed little-endian, locked by [`tests::layout_locked`].
//! Total size 160 bytes (152 body + 8B discriminator) — comfortably
//! fits in one Solana account without padding tricks. POD-friendly
//! (no unsafe, no bytemuck — explicit serialize/deserialize that
//! match the on-chain program's reads exactly).

use crate::scope::ScopeHash;

/// Bond version. Incrementing it (and the encoder) requires a
/// migration; the on-chain program rejects accounts with a wrong
/// version so a stale binary can't misread layout-shifted accounts.
pub const BOND_VERSION: u8 = 1;

/// Discriminator written as the first 8 bytes — distinguishes a
/// `BondAccount` from any other account a malicious caller might
/// pass in as the bond. Value = `sha256("covenant-percolator-bond-v1")`'s
/// first 8 bytes, locked by [`tests::discriminator_locked`].
pub const BOND_DISCRIMINATOR: [u8; 8] = [0xd0, 0x6e, 0xd2, 0xa1, 0x4a, 0x26, 0xf9, 0x8a];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BondAccount {
    /// Identity key of the bonded keeper.
    pub keeper: [u8; 32],
    /// Operator key that signed the scope. The off-chain proof that
    /// the slashed action violated *this operator's* scope is
    /// rooted at this pubkey.
    pub operator: [u8; 32],
    /// Hash of the canonical scope bytes. The slasher provides the
    /// full bytes; the program recomputes the hash to confirm
    /// authenticity.
    pub scope_hash: ScopeHash,
    /// Lamports still in the bond.
    pub lamports: u64,
    /// Slot at which the bond was created. Receipts dated before
    /// this slot can't be used as slash evidence (the keeper was
    /// not yet bonded).
    pub created_slot: u64,
    /// Pubkey lamports flow to on slash. Operator-chosen at bond
    /// creation (treasury, burner, separate operator address).
    pub slash_recipient: [u8; 32],
    /// Set to 1 once the bond is fully slashed. A slashed bond
    /// can't be withdrawn or re-slashed.
    pub slashed: u8,
    /// Set to 1 by the operator to allow `Withdraw`. Prevents the
    /// keeper from running away mid-slash-window.
    pub paused: u8,
    /// Layout version. Mismatch ⇒ account is not a current bond.
    pub version: u8,
    /// Reserved for future flags (e.g. multi-sig operator, slash
    /// fraction, multi-token bond). Zero in v1.
    pub _reserved: [u8; 5],
}

impl BondAccount {
    /// Byte size of the serialized account (excluding discriminator).
    pub const SIZE: usize = 32 + 32 + 32 + 8 + 8 + 32 + 1 + 1 + 1 + 5;

    /// Total on-disk size including the 8-byte discriminator.
    pub const ON_CHAIN_SIZE: usize = 8 + Self::SIZE;

    pub fn new(
        keeper: [u8; 32],
        operator: [u8; 32],
        scope_hash: ScopeHash,
        slash_recipient: [u8; 32],
        lamports: u64,
        created_slot: u64,
    ) -> Self {
        Self {
            keeper,
            operator,
            scope_hash,
            lamports,
            created_slot,
            slash_recipient,
            slashed: 0,
            paused: 0,
            version: BOND_VERSION,
            _reserved: [0; 5],
        }
    }

    /// Serialize to the on-chain wire format (discriminator + fields).
    pub fn encode(&self) -> [u8; Self::ON_CHAIN_SIZE] {
        let mut out = [0u8; Self::ON_CHAIN_SIZE];
        out[..8].copy_from_slice(&BOND_DISCRIMINATOR);
        let mut p = 8;
        out[p..p + 32].copy_from_slice(&self.keeper);
        p += 32;
        out[p..p + 32].copy_from_slice(&self.operator);
        p += 32;
        out[p..p + 32].copy_from_slice(&self.scope_hash.0);
        p += 32;
        out[p..p + 8].copy_from_slice(&self.lamports.to_le_bytes());
        p += 8;
        out[p..p + 8].copy_from_slice(&self.created_slot.to_le_bytes());
        p += 8;
        out[p..p + 32].copy_from_slice(&self.slash_recipient);
        p += 32;
        out[p] = self.slashed;
        p += 1;
        out[p] = self.paused;
        p += 1;
        out[p] = self.version;
        p += 1;
        out[p..p + 5].copy_from_slice(&self._reserved);
        out
    }

    /// Deserialize from the on-chain wire format. Rejects on
    /// discriminator mismatch or wrong version, so a hostile caller
    /// can't smuggle a non-bond account through.
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() < Self::ON_CHAIN_SIZE {
            return Err("bond account too short");
        }
        if bytes[..8] != BOND_DISCRIMINATOR {
            return Err("bond discriminator mismatch");
        }
        let mut p = 8;
        let mut keeper = [0u8; 32];
        keeper.copy_from_slice(&bytes[p..p + 32]);
        p += 32;
        let mut operator = [0u8; 32];
        operator.copy_from_slice(&bytes[p..p + 32]);
        p += 32;
        let mut scope_hash = [0u8; 32];
        scope_hash.copy_from_slice(&bytes[p..p + 32]);
        p += 32;
        let lamports =
            u64::from_le_bytes(bytes[p..p + 8].try_into().map_err(|_| "lamports")?);
        p += 8;
        let created_slot =
            u64::from_le_bytes(bytes[p..p + 8].try_into().map_err(|_| "slot")?);
        p += 8;
        let mut slash_recipient = [0u8; 32];
        slash_recipient.copy_from_slice(&bytes[p..p + 32]);
        p += 32;
        let slashed = bytes[p];
        p += 1;
        let paused = bytes[p];
        p += 1;
        let version = bytes[p];
        p += 1;
        if version != BOND_VERSION {
            return Err("bond version mismatch");
        }
        let mut _reserved = [0u8; 5];
        _reserved.copy_from_slice(&bytes[p..p + 5]);
        Ok(Self {
            keeper,
            operator,
            scope_hash: ScopeHash(scope_hash),
            lamports,
            created_slot,
            slash_recipient,
            slashed,
            paused,
            version,
            _reserved,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn discriminator_locked() {
        let mut h = Sha256::new();
        h.update(b"covenant-percolator-bond-v1");
        let full = h.finalize();
        assert_eq!(&BOND_DISCRIMINATOR, &full[..8]);
    }

    #[test]
    fn layout_locked() {
        // Size sanity — drift this and dependents must update.
        // 32+32+32+8+8+32+1+1+1+5 = 152 body bytes; plus 8B disc = 160.
        assert_eq!(BondAccount::SIZE, 152);
        assert_eq!(BondAccount::ON_CHAIN_SIZE, 160);
    }

    #[test]
    fn encode_decode_roundtrip() {
        let bond = BondAccount::new(
            [1u8; 32],
            [2u8; 32],
            ScopeHash([3u8; 32]),
            [4u8; 32],
            1_500_000_000,
            10_000,
        );
        let bytes = bond.encode();
        let back = BondAccount::decode(&bytes).unwrap();
        assert_eq!(back, bond);
    }

    #[test]
    fn decode_rejects_wrong_discriminator() {
        let mut bond = BondAccount::new([0; 32], [0; 32], ScopeHash([0; 32]), [0; 32], 0, 0);
        let mut bytes = bond.encode();
        bytes[0] = 0;
        assert!(BondAccount::decode(&bytes).is_err());
        bond.version = 99;
        let bytes = bond.encode();
        assert!(BondAccount::decode(&bytes).is_err());
    }

    #[test]
    fn decode_rejects_short_buffer() {
        assert!(BondAccount::decode(&[0u8; 10]).is_err());
    }
}
