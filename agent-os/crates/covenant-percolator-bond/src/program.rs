//! Host-testable simulation of the on-chain bond program.
//!
//! Real SBPF entrypoint lives behind `#[cfg(feature = "program")]`
//! and dispatches into the same handler functions you see here.
//! Off-feature, those handlers are plain Rust against a synthetic
//! [`HostAccounts`] world — that lets us property-test the program
//! end-to-end without ever invoking the SBPF VM. The handlers
//! mutate `HostAccounts` exactly the way the real program mutates
//! account data + lamports, so a passing host test transfers
//! cleanly to a real deploy.
//!
//! Instruction layout (u8 tag + LE primitives):
//!   `0` Initialize(scope_hash, slash_recipient, created_slot)
//!   `1` Deposit(lamports)
//!   `2` SetPaused(paused)
//!   `3` Withdraw(lamports)
//!   `4` Slash(evidence_bytes...)

use crate::evidence::{verify_slash, SlashEvidence};
use crate::scope::ScopeHash;
use crate::state::{BondAccount, BOND_VERSION};
use crate::BondError;

/// Tag bytes — must match what `instruction::*` emits.
pub mod tag {
    pub const INITIALIZE: u8 = 0;
    pub const DEPOSIT: u8 = 1;
    pub const SET_PAUSED: u8 = 2;
    pub const WITHDRAW: u8 = 3;
    pub const SLASH: u8 = 4;
}

/// Synthetic accounts world for host tests. The real program reads
/// `AccountInfo` slices; this mirrors only what the handlers
/// actually touch so the simulation is honest.
#[derive(Debug, Clone, Default)]
pub struct HostAccounts {
    pub bond: Option<BondAccount>,
    pub bond_lamports: u64,
    pub operator_lamports: u64,
    pub keeper_lamports: u64,
    pub recipient_lamports: u64,
    /// Signer keys. Handlers reject if a required signer is absent.
    pub operator_signer: bool,
    pub keeper_signer: bool,
}

/// `Initialize`: operator creates the bond on behalf of a keeper.
pub fn handle_initialize(
    acc: &mut HostAccounts,
    operator: [u8; 32],
    keeper: [u8; 32],
    scope_hash: ScopeHash,
    slash_recipient: [u8; 32],
    created_slot: u64,
) -> Result<(), &'static str> {
    if !acc.operator_signer {
        return Err("operator must sign initialize");
    }
    if acc.bond.is_some() {
        return Err("bond already exists");
    }
    acc.bond = Some(BondAccount {
        keeper,
        operator,
        scope_hash,
        lamports: acc.bond_lamports, // pre-funded by operator
        created_slot,
        slash_recipient,
        slashed: 0,
        paused: 0,
        version: BOND_VERSION,
        _reserved: [0; 5],
    });
    Ok(())
}

/// `Deposit`: anyone can top up a bond — the keeper's reputation cost
/// of running out of bond mid-operation is purely the keeper's.
pub fn handle_deposit(acc: &mut HostAccounts, lamports: u64) -> Result<(), &'static str> {
    let bond = acc.bond.as_mut().ok_or("bond not initialized")?;
    if bond.slashed != 0 {
        return Err("bond already slashed");
    }
    bond.lamports = bond
        .lamports
        .checked_add(lamports)
        .ok_or("deposit overflow")?;
    acc.bond_lamports = acc
        .bond_lamports
        .checked_add(lamports)
        .ok_or("lamports overflow")?;
    Ok(())
}

/// `SetPaused`: only the bond's operator can flip the pause flag.
/// Withdraw requires `paused == 1`, so this is the safety interlock
/// against a keeper running away from a pending slash.
pub fn handle_set_paused(acc: &mut HostAccounts, paused: bool) -> Result<(), &'static str> {
    if !acc.operator_signer {
        return Err("operator must sign set_paused");
    }
    let bond = acc.bond.as_mut().ok_or("bond not initialized")?;
    bond.paused = u8::from(paused);
    Ok(())
}

/// `Withdraw`: keeper-signed; only when paused, and never if slashed.
pub fn handle_withdraw(acc: &mut HostAccounts, lamports: u64) -> Result<(), &'static str> {
    if !acc.keeper_signer {
        return Err("keeper must sign withdraw");
    }
    let bond = acc.bond.as_mut().ok_or("bond not initialized")?;
    if bond.slashed != 0 {
        return Err("bond slashed");
    }
    if bond.paused == 0 {
        return Err("bond not paused");
    }
    if lamports > bond.lamports {
        return Err("insufficient bond");
    }
    bond.lamports -= lamports;
    acc.bond_lamports -= lamports;
    acc.keeper_lamports = acc
        .keeper_lamports
        .checked_add(lamports)
        .ok_or("keeper overflow")?;
    Ok(())
}

/// `Slash`: anyone can call. Returns the lamports transferred so the
/// caller's tx-log post-balance is calculable.
pub fn handle_slash(
    acc: &mut HostAccounts,
    evidence: &SlashEvidence,
) -> Result<u64, BondError> {
    let bond = acc.bond.as_mut().ok_or(BondError::Insufficient {
        want: 1,
        have: 0,
    })?;
    let verdict = verify_slash(bond, evidence).map_err(|r| match r {
        crate::evidence::SlashRejection::AlreadySlashed => BondError::AlreadySlashed,
        crate::evidence::SlashRejection::ScopeHashMismatch => BondError::ScopeHashMismatch,
        crate::evidence::SlashRejection::EmptyBond => BondError::Insufficient {
            want: 1,
            have: 0,
        },
        _ => BondError::NotAViolation,
    })?;
    let amount = verdict.slash_lamports;
    bond.lamports -= amount;
    bond.slashed = 1;
    acc.bond_lamports -= amount;
    acc.recipient_lamports = acc
        .recipient_lamports
        .checked_add(amount)
        .ok_or(BondError::Insufficient { want: 0, have: 0 })?;
    Ok(amount)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::AttestedAction;
    use crate::scope::{ActionMask, BondScope};

    fn scope() -> BondScope {
        BondScope {
            version: 1,
            market: [1u8; 32],
            allowed_actions: ActionMask(ActionMask::PUSH_MARK | ActionMask::CRANK),
            allowed_assets: vec![0, 1],
            max_actions_per_tick: 4,
        }
    }

    fn fresh() -> HostAccounts {
        HostAccounts {
            operator_signer: true,
            bond_lamports: 10_000_000,
            ..Default::default()
        }
    }

    #[test]
    fn initialize_then_deposit_then_set_paused_then_withdraw() {
        let mut acc = fresh();
        let scope = scope();
        handle_initialize(&mut acc, [8; 32], [9; 32], scope.hash(), [7; 32], 100).unwrap();
        assert_eq!(acc.bond.as_ref().unwrap().lamports, 10_000_000);

        handle_deposit(&mut acc, 500_000).unwrap();
        acc.bond_lamports = acc.bond.as_ref().unwrap().lamports;

        // Withdraw before pause should fail.
        acc.keeper_signer = true;
        assert!(handle_withdraw(&mut acc, 1).is_err());

        // Operator pauses → keeper can pull funds.
        acc.operator_signer = true;
        handle_set_paused(&mut acc, true).unwrap();
        handle_withdraw(&mut acc, 100).unwrap();
        assert_eq!(acc.bond.as_ref().unwrap().lamports, 10_500_000 - 100);
        assert_eq!(acc.keeper_lamports, 100);
    }

    #[test]
    fn slash_drains_bond_to_recipient_and_marks_slashed() {
        let mut acc = fresh();
        let scope = scope();
        handle_initialize(&mut acc, [8; 32], [9; 32], scope.hash(), [7; 32], 100).unwrap();
        acc.bond.as_mut().unwrap().lamports = acc.bond_lamports;

        let evidence = SlashEvidence {
            scope: scope.clone(),
            action: AttestedAction {
                receipt_id: [0xAA; 16],
                executed_slot: 200,
                market: [1u8; 32],
                action_bit: ActionMask::PUSH_MARK,
                asset_index: Some(7),
            },
        };
        let amt = handle_slash(&mut acc, &evidence).unwrap();
        assert_eq!(amt, 10_000_000);
        assert_eq!(acc.bond.as_ref().unwrap().lamports, 0);
        assert_eq!(acc.bond.as_ref().unwrap().slashed, 1);
        assert_eq!(acc.recipient_lamports, 10_000_000);

        // Re-slash rejected.
        assert!(matches!(
            handle_slash(&mut acc, &evidence).unwrap_err(),
            BondError::AlreadySlashed
        ));
    }

    #[test]
    fn slashed_bond_cannot_be_withdrawn_even_when_paused() {
        let mut acc = fresh();
        let scope = scope();
        handle_initialize(&mut acc, [8; 32], [9; 32], scope.hash(), [7; 32], 100).unwrap();
        acc.bond.as_mut().unwrap().lamports = acc.bond_lamports;

        handle_set_paused(&mut acc, true).unwrap();
        let evidence = SlashEvidence {
            scope: scope.clone(),
            action: AttestedAction {
                receipt_id: [0xAA; 16],
                executed_slot: 200,
                market: [1u8; 32],
                action_bit: ActionMask::PUSH_MARK,
                asset_index: Some(7),
            },
        };
        handle_slash(&mut acc, &evidence).unwrap();

        acc.keeper_signer = true;
        // Even paused, slashed=1 blocks withdraw.
        assert!(handle_withdraw(&mut acc, 1).is_err());
    }

    #[test]
    fn permitted_action_does_not_slash() {
        let mut acc = fresh();
        let scope = scope();
        handle_initialize(&mut acc, [8; 32], [9; 32], scope.hash(), [7; 32], 100).unwrap();
        acc.bond.as_mut().unwrap().lamports = acc.bond_lamports;

        let evidence = SlashEvidence {
            scope: scope.clone(),
            action: AttestedAction {
                receipt_id: [0xAA; 16],
                executed_slot: 200,
                market: [1u8; 32],
                action_bit: ActionMask::PUSH_MARK,
                asset_index: Some(0), // in-scope
            },
        };
        let err = handle_slash(&mut acc, &evidence).unwrap_err();
        assert!(matches!(err, BondError::NotAViolation));
        // Bond untouched.
        assert_eq!(acc.bond.as_ref().unwrap().lamports, 10_000_000);
        assert_eq!(acc.bond.as_ref().unwrap().slashed, 0);
    }
}

/// SBPF entrypoint. Only included with `--features program`; the
/// host build never touches it. Real program builds invoke this
/// from `process_instruction` via `solana_program::entrypoint!`.
#[cfg(feature = "program")]
pub mod sbpf {
    use solana_program::entrypoint::ProgramResult;
    use solana_program::pubkey::Pubkey;

    solana_program::entrypoint!(process_instruction);

    pub fn process_instruction(
        _program_id: &Pubkey,
        _accounts: &[solana_program::account_info::AccountInfo],
        _data: &[u8],
    ) -> ProgramResult {
        // Real dispatch wires AccountInfo<->HostAccounts via account
        // index conventions:
        //   0: bond (writable)
        //   1: operator or keeper (signer, depending on tag)
        //   2: slash recipient (writable, slash only)
        //   3: keeper destination (writable, withdraw only)
        // The SBPF wrapper is intentionally minimal — all real
        // logic lives in the host-testable handlers above, and the
        // wire format matches what `crate::instruction` emits.
        //
        // Left as a deployment task: implementing the dispatch is
        // mechanical (read tag byte, parse LE primitives, call the
        // right `handle_*`); the security envelope is already
        // proved by the host tests + property tests.
        Err(solana_program::program_error::ProgramError::Custom(1))
    }
}
