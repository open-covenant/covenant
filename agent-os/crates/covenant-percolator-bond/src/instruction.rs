//! Off-chain instruction builders for the bond program.
//!
//! Wire format mirrors percolator-prog's convention (u8 tag + LE
//! primitives) and is byte-locked by the tests at the bottom of
//! this file. PDA derivations match the on-chain program — operators
//! can derive bond addresses without touching the program.

use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey::Pubkey;

use crate::evidence::SlashEvidence;
use crate::program::tag;
use crate::scope::ScopeHash;
use crate::{BOND_SEED, SLASH_SEED};

pub fn bond_pda(program_id: &Pubkey, keeper: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[BOND_SEED, keeper.as_ref()], program_id)
}

pub fn slash_receipt_pda(
    program_id: &Pubkey,
    bond: &Pubkey,
    receipt_id: &[u8; 16],
) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SLASH_SEED, bond.as_ref(), receipt_id.as_ref()], program_id)
}

/// `Initialize` — operator opens a bond pre-funded with `lamports`,
/// stores `scope_hash` + `slash_recipient`, and pins `created_slot`.
///
/// Accounts:
///   0. `bond` — writable (PDA, init)
///   1. `operator` — signer, writable (paying for rent + init lamports)
///   2. `system_program` — readonly
pub fn initialize_bond(
    program_id: Pubkey,
    bond: Pubkey,
    operator: Pubkey,
    keeper: Pubkey,
    scope_hash: ScopeHash,
    slash_recipient: Pubkey,
    created_slot: u64,
) -> Instruction {
    let mut data = Vec::with_capacity(1 + 32 + 32 + 32 + 8);
    data.push(tag::INITIALIZE);
    data.extend_from_slice(keeper.as_ref());
    data.extend_from_slice(scope_hash.as_bytes());
    data.extend_from_slice(slash_recipient.as_ref());
    data.extend_from_slice(&created_slot.to_le_bytes());
    Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(bond, false),
            AccountMeta::new(operator, true),
            AccountMeta::new_readonly(solana_sdk::system_program::ID, false),
        ],
        data,
    }
}

/// `Deposit` — top up an existing bond. Anyone signs (top-ups are
/// strictly beneficial to the operator/slasher).
///
/// Accounts:
///   0. `bond` — writable
///   1. `funder` — signer, writable
///   2. `system_program` — readonly
pub fn deposit(
    program_id: Pubkey,
    bond: Pubkey,
    funder: Pubkey,
    lamports: u64,
) -> Instruction {
    let mut data = Vec::with_capacity(1 + 8);
    data.push(tag::DEPOSIT);
    data.extend_from_slice(&lamports.to_le_bytes());
    Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(bond, false),
            AccountMeta::new(funder, true),
            AccountMeta::new_readonly(solana_sdk::system_program::ID, false),
        ],
        data,
    }
}

/// `SetPaused` — operator-only. Keepers can only `Withdraw` when
/// `paused == 1`.
///
/// Accounts:
///   0. `bond` — writable
///   1. `operator` — signer
pub fn set_paused(
    program_id: Pubkey,
    bond: Pubkey,
    operator: Pubkey,
    paused: bool,
) -> Instruction {
    let data = vec![tag::SET_PAUSED, u8::from(paused)];
    Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(bond, false),
            AccountMeta::new_readonly(operator, true),
        ],
        data,
    }
}

/// `Withdraw` — keeper-signed, requires `paused == 1`. Transfers
/// `lamports` from the bond to `dest`.
///
/// Accounts:
///   0. `bond` — writable
///   1. `keeper` — signer
///   2. `dest` — writable
pub fn withdraw(
    program_id: Pubkey,
    bond: Pubkey,
    keeper: Pubkey,
    dest: Pubkey,
    lamports: u64,
) -> Instruction {
    let mut data = Vec::with_capacity(1 + 8);
    data.push(tag::WITHDRAW);
    data.extend_from_slice(&lamports.to_le_bytes());
    Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(bond, false),
            AccountMeta::new_readonly(keeper, true),
            AccountMeta::new(dest, false),
        ],
        data,
    }
}

/// `Slash` — anyone signs. The program calls `verify_slash` against
/// the bond's stored scope_hash + the supplied evidence; on success
/// transfers the entire bond to the slash recipient.
///
/// Accounts:
///   0. `bond` — writable
///   1. `recipient` — writable (must equal `bond.slash_recipient`)
///   2. `slash_receipt` — writable (PDA, init; replay protection)
///   3. `payer` — signer, writable (rent for `slash_receipt`)
///   4. `system_program` — readonly
pub fn slash(
    program_id: Pubkey,
    bond: Pubkey,
    recipient: Pubkey,
    slash_receipt: Pubkey,
    payer: Pubkey,
    evidence: &SlashEvidence,
) -> Instruction {
    let mut data = Vec::with_capacity(1 + 256);
    data.push(tag::SLASH);
    // Scope: encode_canonical's length-prefixed shape.
    let scope_bytes = evidence.scope.encode_canonical();
    data.extend_from_slice(&(scope_bytes.len() as u16).to_le_bytes());
    data.extend_from_slice(&scope_bytes);
    // Action.
    data.extend_from_slice(&evidence.action.receipt_id);
    data.extend_from_slice(&evidence.action.executed_slot.to_le_bytes());
    data.extend_from_slice(&evidence.action.market);
    data.push(evidence.action.action_bit);
    match evidence.action.asset_index {
        Some(i) => {
            data.push(1);
            data.extend_from_slice(&i.to_le_bytes());
        }
        None => {
            data.push(0);
            data.extend_from_slice(&[0u8; 2]);
        }
    }
    Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(bond, false),
            AccountMeta::new(recipient, false),
            AccountMeta::new(slash_receipt, false),
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(solana_sdk::system_program::ID, false),
        ],
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::AttestedAction;
    use crate::scope::{ActionMask, BondScope};

    fn pk(b: u8) -> Pubkey {
        Pubkey::new_from_array([b; 32])
    }

    #[test]
    fn initialize_layout_locked() {
        let ix = initialize_bond(
            pk(0xAA),
            pk(0x01),
            pk(0x02),
            pk(0x03),
            ScopeHash([0x44; 32]),
            pk(0x05),
            777,
        );
        assert_eq!(ix.data[0], tag::INITIALIZE);
        assert_eq!(&ix.data[1..33], &[0x03u8; 32]); // keeper
        assert_eq!(&ix.data[33..65], &[0x44u8; 32]); // scope_hash
        assert_eq!(&ix.data[65..97], &[0x05u8; 32]); // slash_recipient
        assert_eq!(&ix.data[97..105], &777u64.to_le_bytes());
        assert_eq!(ix.accounts.len(), 3);
        assert!(ix.accounts[1].is_signer && ix.accounts[1].is_writable);
    }

    #[test]
    fn deposit_layout_locked() {
        let ix = deposit(pk(0xAA), pk(0x01), pk(0x02), 500_000);
        assert_eq!(ix.data[0], tag::DEPOSIT);
        assert_eq!(&ix.data[1..9], &500_000u64.to_le_bytes());
        assert_eq!(ix.data.len(), 9);
    }

    #[test]
    fn set_paused_layout_locked() {
        let ix = set_paused(pk(0xAA), pk(0x01), pk(0x02), true);
        assert_eq!(ix.data, vec![tag::SET_PAUSED, 1]);
        let ix = set_paused(pk(0xAA), pk(0x01), pk(0x02), false);
        assert_eq!(ix.data, vec![tag::SET_PAUSED, 0]);
    }

    #[test]
    fn withdraw_layout_locked() {
        let ix = withdraw(pk(0xAA), pk(0x01), pk(0x02), pk(0x03), 1_234_567);
        assert_eq!(ix.data[0], tag::WITHDRAW);
        assert_eq!(&ix.data[1..9], &1_234_567u64.to_le_bytes());
        assert_eq!(ix.accounts.len(), 3);
        assert!(ix.accounts[1].is_signer && !ix.accounts[1].is_writable);
    }

    #[test]
    fn slash_layout_locked() {
        let scope = BondScope {
            version: 1,
            market: [0x11; 32],
            allowed_actions: ActionMask(ActionMask::CRANK),
            allowed_assets: Some(vec![0]),
            max_actions_per_tick: 1,
        };
        let action = AttestedAction {
            receipt_id: [0xCC; 16],
            executed_slot: 500,
            market: [0x11; 32],
            action_bit: ActionMask::PUSH_MARK,
            asset_index: Some(7),
        };
        let ev = SlashEvidence { scope, action };
        let ix = slash(pk(0xAA), pk(0x01), pk(0x02), pk(0x03), pk(0x04), &ev);
        assert_eq!(ix.data[0], tag::SLASH);
        let scope_len =
            u16::from_le_bytes(ix.data[1..3].try_into().unwrap()) as usize;
        // After tag(1) + scope_len(2) + scope_bytes(scope_len), the
        // action block follows: receipt_id(16) + slot(8) + market(32)
        // + action_bit(1) + has_asset(1) + asset_index(2) = 60 bytes.
        let action_off = 1 + 2 + scope_len;
        assert_eq!(&ix.data[action_off..action_off + 16], &[0xCCu8; 16]);
        assert_eq!(
            &ix.data[action_off + 16..action_off + 24],
            &500u64.to_le_bytes()
        );
        assert_eq!(&ix.data[action_off + 24..action_off + 56], &[0x11u8; 32]);
        assert_eq!(ix.data[action_off + 56], ActionMask::PUSH_MARK);
        assert_eq!(ix.data[action_off + 57], 1); // has_asset
        assert_eq!(
            &ix.data[action_off + 58..action_off + 60],
            &7u16.to_le_bytes()
        );
        assert_eq!(ix.accounts.len(), 5);
        assert!(ix.accounts[3].is_signer);
    }

    #[test]
    fn slash_serializes_none_asset_with_two_padding_bytes() {
        let scope = BondScope {
            version: 1,
            market: [0x11; 32],
            allowed_actions: ActionMask(ActionMask::CRANK),
            allowed_assets: None,
            max_actions_per_tick: 1,
        };
        let action = AttestedAction {
            receipt_id: [0; 16],
            executed_slot: 0,
            market: [0xFF; 32], // wrong market — would trigger MarketMismatch
            action_bit: ActionMask::CRANK,
            asset_index: None,
        };
        let ev = SlashEvidence { scope, action };
        let ix = slash(pk(0), pk(0), pk(0), pk(0), pk(0), &ev);
        let scope_len =
            u16::from_le_bytes(ix.data[1..3].try_into().unwrap()) as usize;
        let action_off = 1 + 2 + scope_len;
        // has_asset = 0 followed by two padding bytes.
        assert_eq!(ix.data[action_off + 57], 0);
        assert_eq!(&ix.data[action_off + 58..action_off + 60], &[0u8; 2]);
    }

    #[test]
    fn bond_pda_is_deterministic() {
        let pid = pk(0xAA);
        let keeper = pk(0xBB);
        let (a, _) = bond_pda(&pid, &keeper);
        let (b, _) = bond_pda(&pid, &keeper);
        assert_eq!(a, b);
    }
}
