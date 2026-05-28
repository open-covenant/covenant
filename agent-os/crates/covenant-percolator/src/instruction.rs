//! Wire-level instruction builders for the v16 keeper surface.
//!
//! Layout source of truth: `aeyakovenko/percolator-prog`,
//! `src/v16_program.rs`. The program decodes a single-byte u8 tag
//! followed by little-endian primitives — *not* Anchor-style 8-byte
//! sha256 discriminators, *not* borsh enum tags. Each builder here
//! emits exactly the byte sequence the program's `Instruction::decode`
//! reads.
//!
//! Tags (program-side):
//!   - `5`  PermissionlessCrank
//!   - `43` ForfeitRecoveryLeg
//!   - `44` RebalanceReduce
//!   - `45` FinalizeResetSide
//!   - `63` PushAuthMark   (replaces retired `36 PushHyperpMark`)
//!
//! Byte layouts are locked by [`tests::*_wire_bytes_locked`]. If a
//! future percolator-prog change shifts a layout, those tests fail
//! deterministically; **do not** edit them without re-reading the
//! program source.
//!
//! Accounts mirror the on-chain handler's expectations; signer /
//! writable flags follow what the program's runtime check enforces.

use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey::Pubkey;

/// Tag bytes — first byte of every instruction `data` blob. Lifted
/// verbatim from `percolator-prog/src/v16_program.rs:Instruction::encode`.
pub mod tag {
    pub const PERMISSIONLESS_CRANK: u8 = 5;
    pub const FORFEIT_RECOVERY_LEG: u8 = 43;
    pub const REBALANCE_REDUCE: u8 = 44;
    pub const FINALIZE_RESET_SIDE: u8 = 45;
    pub const PUSH_AUTH_MARK: u8 = 63;
}

/// `PermissionlessCrank::action` values. Sourced from
/// `handle_permissionless_crank`. v16 forbids `funding_rate_e9 != 0`
/// and `recovery_reason != 0` on this path, so [`crank_refresh`] is
/// the right default for a routine keeper tick.
pub mod crank_action {
    pub const REFRESH: u8 = 0;
    pub const LIQUIDATE: u8 = 1;
    pub const SETTLE_B: u8 = 2;
}

/// Side encoding for `FinalizeResetSide`. Matches `SideV16::*` via
/// the program's `decode_side`.
pub mod side {
    pub const LONG: u8 = 0;
    pub const SHORT: u8 = 1;
}

fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn push_i128(out: &mut Vec<u8>, v: i128) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn push_u128(out: &mut Vec<u8>, v: u128) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Routine `PermissionlessCrank` with `action = Refresh`. v16 requires
/// `funding_rate_e9 == 0` and `recovery_reason == 0` on the refresh
/// path; we hard-code them and do not expose toggles, so an operator
/// can't accidentally exercise a retired branch.
///
/// `now_slot` should be the cluster's current slot at submission;
/// the program rejects stale `now_slot`. `fee_bps` is a no-op on
/// refresh (the program uses it only on action=1) — pass `0`.
///
/// Accounts (the program enforces this order; do not reorder):
///   0. `owner_or_cranker` — read-only, signer (writable iff the
///      market awards a cranker rebate, set via `liquidate_crank`)
///   1. `market` — writable
///   2. `portfolio` — writable
///
/// `cranker_reward_portfolio` (4th account) is only set on
/// `LIQUIDATE`; for refresh leave it `None`.
pub fn permissionless_crank_refresh(
    program_id: Pubkey,
    market: Pubkey,
    portfolio: Pubkey,
    cranker: Pubkey,
    asset_index: u16,
    now_slot: u64,
) -> Instruction {
    permissionless_crank(
        program_id,
        market,
        portfolio,
        cranker,
        None,
        crank_action::REFRESH,
        asset_index,
        now_slot,
        0,
        0,
        0,
        0,
    )
}

/// Full `PermissionlessCrank` builder for operators that need
/// non-default actions (liquidation, settle-B). Keeps the program's
/// argument shape 1:1 so an operator who studied `v16_program.rs`
/// recognizes every parameter.
#[allow(clippy::too_many_arguments)]
pub fn permissionless_crank(
    program_id: Pubkey,
    market: Pubkey,
    portfolio: Pubkey,
    cranker: Pubkey,
    cranker_reward_portfolio: Option<Pubkey>,
    action: u8,
    asset_index: u16,
    now_slot: u64,
    funding_rate_e9: i128,
    close_q: u128,
    fee_bps: u64,
    recovery_reason: u8,
) -> Instruction {
    let mut data = Vec::with_capacity(1 + 1 + 2 + 8 + 16 + 16 + 8 + 1);
    data.push(tag::PERMISSIONLESS_CRANK);
    data.push(action);
    push_u16(&mut data, asset_index);
    push_u64(&mut data, now_slot);
    push_i128(&mut data, funding_rate_e9);
    push_u128(&mut data, close_q);
    push_u64(&mut data, fee_bps);
    data.push(recovery_reason);

    let mut accounts = vec![
        AccountMeta::new_readonly(cranker, true),
        AccountMeta::new(market, false),
        AccountMeta::new(portfolio, false),
    ];
    if let Some(reward) = cranker_reward_portfolio {
        accounts.push(AccountMeta::new(reward, false));
    }

    Instruction {
        program_id,
        accounts,
        data,
    }
}

/// `PushAuthMark` (program tag 63) — the post-migration manual-mark
/// path. The asset must be in `AUTH_MARK` oracle mode on-chain (a
/// market-admin config, not a keeper-flippable bit); the program
/// returns `InvalidInstructionData` otherwise.
///
/// Accounts:
///   0. `authority` — read-only, signer. Market admin when
///      `asset_index == 0`; per-asset `oracle_authority` otherwise.
///   1. `market` — writable
pub fn push_auth_mark(
    program_id: Pubkey,
    market: Pubkey,
    authority: Pubkey,
    asset_index: u16,
    now_slot: u64,
    mark_e6: u64,
) -> Instruction {
    let mut data = Vec::with_capacity(1 + 2 + 8 + 8);
    data.push(tag::PUSH_AUTH_MARK);
    push_u16(&mut data, asset_index);
    push_u64(&mut data, now_slot);
    push_u64(&mut data, mark_e6);

    Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(market, false),
        ],
        data,
    }
}

/// `ForfeitRecoveryLeg` (program tag 43). The asset's side must be
/// in recovery mode on-chain; the program rejects `b_delta_budget == 0`.
///
/// Accounts:
///   0. `owner` — read-only, signer (the portfolio's owner)
///   1. `market` — writable
///   2. `portfolio` — writable
pub fn forfeit_recovery_leg(
    program_id: Pubkey,
    market: Pubkey,
    portfolio: Pubkey,
    owner: Pubkey,
    asset_index: u16,
    b_delta_budget: u128,
) -> Instruction {
    let mut data = Vec::with_capacity(1 + 2 + 16);
    data.push(tag::FORFEIT_RECOVERY_LEG);
    push_u16(&mut data, asset_index);
    push_u128(&mut data, b_delta_budget);

    Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new_readonly(owner, true),
            AccountMeta::new(market, false),
            AccountMeta::new(portfolio, false),
        ],
        data,
    }
}

/// `RebalanceReduce` (program tag 44). The program rejects
/// `reduce_q == 0`.
///
/// Accounts: same shape and order as [`forfeit_recovery_leg`].
pub fn rebalance_reduce(
    program_id: Pubkey,
    market: Pubkey,
    portfolio: Pubkey,
    owner: Pubkey,
    asset_index: u16,
    reduce_q: u128,
) -> Instruction {
    let mut data = Vec::with_capacity(1 + 2 + 16);
    data.push(tag::REBALANCE_REDUCE);
    push_u16(&mut data, asset_index);
    push_u128(&mut data, reduce_q);

    Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new_readonly(owner, true),
            AccountMeta::new(market, false),
            AccountMeta::new(portfolio, false),
        ],
        data,
    }
}

/// `FinalizeResetSide` (program tag 45) — permissionless, no signer.
/// The asset side must be in `ResetMode` with zero open positions /
/// stale accounts; the program rejects otherwise.
///
/// Accounts:
///   0. `market` — writable (only)
pub fn finalize_reset_side(
    program_id: Pubkey,
    market: Pubkey,
    asset_index: u16,
    side: u8,
) -> Instruction {
    let mut data = Vec::with_capacity(1 + 2 + 1);
    data.push(tag::FINALIZE_RESET_SIDE);
    push_u16(&mut data, asset_index);
    data.push(side);

    Instruction {
        program_id,
        accounts: vec![AccountMeta::new(market, false)],
        data,
    }
}

#[cfg(test)]
mod tests {
    //! Golden byte tests. The wire format is a contract with
    //! percolator-prog; if these break, either the program changed
    //! (re-verify against `v16_program.rs:Instruction::decode`) or
    //! a builder regressed.
    use super::*;

    fn pk(b: u8) -> Pubkey {
        Pubkey::new_from_array([b; 32])
    }

    #[test]
    fn permissionless_crank_refresh_wire_bytes_locked() {
        let ix = permissionless_crank_refresh(pk(0xAA), pk(0x01), pk(0x02), pk(0x03), 7, 1_234);
        // tag=5, action=0, asset=7 LE, slot=1234 LE, funding=0 (16B), close_q=0 (16B), fee_bps=0 (8B), recovery=0
        let mut expected = vec![5u8, 0, 7, 0];
        expected.extend_from_slice(&1234u64.to_le_bytes());
        expected.extend_from_slice(&[0u8; 16]); // funding_rate_e9
        expected.extend_from_slice(&[0u8; 16]); // close_q
        expected.extend_from_slice(&[0u8; 8]); // fee_bps
        expected.push(0); // recovery_reason
        assert_eq!(ix.data, expected, "data layout drift");
        assert_eq!(ix.data.len(), 1 + 1 + 2 + 8 + 16 + 16 + 8 + 1);
        assert_eq!(ix.accounts.len(), 3);
        assert!(ix.accounts[0].is_signer && !ix.accounts[0].is_writable);
        assert!(!ix.accounts[1].is_signer && ix.accounts[1].is_writable);
        assert!(!ix.accounts[2].is_signer && ix.accounts[2].is_writable);
    }

    #[test]
    fn permissionless_crank_liquidate_adds_reward_account() {
        let ix = permissionless_crank(
            pk(0xAA),
            pk(0x01),
            pk(0x02),
            pk(0x03),
            Some(pk(0x04)),
            crank_action::LIQUIDATE,
            3,
            500,
            0,
            10_000,
            25,
            0,
        );
        assert_eq!(ix.accounts.len(), 4);
        assert_eq!(ix.accounts[3].pubkey, pk(0x04));
        assert!(ix.accounts[3].is_writable);
        assert!(!ix.accounts[3].is_signer);
        // First byte is the tag; second is the action.
        assert_eq!(ix.data[0], 5);
        assert_eq!(ix.data[1], 1);
    }

    #[test]
    fn push_auth_mark_wire_bytes_locked() {
        let ix = push_auth_mark(pk(0xAA), pk(0x01), pk(0x02), 9, 7_777, 145_321_000);
        let mut expected = vec![63u8, 9, 0];
        expected.extend_from_slice(&7_777u64.to_le_bytes());
        expected.extend_from_slice(&145_321_000u64.to_le_bytes());
        assert_eq!(ix.data, expected);
        assert_eq!(ix.data.len(), 1 + 2 + 8 + 8);
        assert_eq!(ix.accounts.len(), 2);
        assert!(ix.accounts[0].is_signer && !ix.accounts[0].is_writable);
        assert!(!ix.accounts[1].is_signer && ix.accounts[1].is_writable);
    }

    #[test]
    fn forfeit_recovery_leg_wire_bytes_locked() {
        let ix = forfeit_recovery_leg(
            pk(0xAA),
            pk(0x01),
            pk(0x02),
            pk(0x03),
            4,
            12_345_678_901_234,
        );
        let mut expected = vec![43u8, 4, 0];
        expected.extend_from_slice(&12_345_678_901_234u128.to_le_bytes());
        assert_eq!(ix.data, expected);
        assert_eq!(ix.data.len(), 1 + 2 + 16);
        assert_eq!(ix.accounts.len(), 3);
        assert!(ix.accounts[0].is_signer);
    }

    #[test]
    fn rebalance_reduce_wire_bytes_locked() {
        let ix = rebalance_reduce(
            pk(0xAA),
            pk(0x01),
            pk(0x02),
            pk(0x03),
            12,
            500_000_000_000u128,
        );
        let mut expected = vec![44u8, 12, 0];
        expected.extend_from_slice(&500_000_000_000u128.to_le_bytes());
        assert_eq!(ix.data, expected);
        assert_eq!(ix.data.len(), 1 + 2 + 16);
        assert_eq!(ix.accounts.len(), 3);
    }

    #[test]
    fn finalize_reset_side_wire_bytes_locked() {
        let ix = finalize_reset_side(pk(0xAA), pk(0x01), 2, side::SHORT);
        assert_eq!(ix.data, vec![45u8, 2, 0, 1]);
        assert_eq!(ix.data.len(), 1 + 2 + 1);
        assert_eq!(ix.accounts.len(), 1);
        assert!(!ix.accounts[0].is_signer);
        assert!(ix.accounts[0].is_writable);
    }

    #[test]
    fn u128_field_is_little_endian() {
        // 1 << 64 — the high half is non-zero, locking byte order.
        let v: u128 = 1u128 << 64;
        let ix = rebalance_reduce(pk(0), pk(0), pk(0), pk(0), 0, v);
        // After tag(1) + asset_index(2) = offset 3.
        let payload = &ix.data[3..];
        assert_eq!(payload.len(), 16);
        assert_eq!(payload, &v.to_le_bytes());
        // Sanity: low 8 bytes are 0, byte 8 is 1.
        assert_eq!(&payload[..8], &[0u8; 8]);
        assert_eq!(payload[8], 1);
    }

    #[test]
    fn i128_field_is_little_endian_twos_complement() {
        // -1i128 → all-ones in two's complement; LE encoding is 16 × 0xff.
        let ix = permissionless_crank(
            pk(0),
            pk(0),
            pk(0),
            pk(0),
            None,
            crank_action::REFRESH,
            0,
            0,
            -1i128,
            0,
            0,
            0,
        );
        // Layout: tag(1) action(1) asset(2) slot(8) funding(16) ...
        let funding = &ix.data[12..28];
        assert_eq!(funding, &[0xffu8; 16]);
    }
}
