#![allow(unexpected_cfgs)]

pub mod error;
pub mod instruction;
pub mod state;

#[cfg(all(test, feature = "sbf-test"))]
#[path = "../tests/abi_contract.rs"]
mod abi_contract_tests;
#[cfg(all(test, feature = "sbf-test"))]
#[path = "../tests/adversarial.rs"]
mod adversarial_tests;
#[cfg(all(test, feature = "sbf-test"))]
#[path = "../tests/lifecycle.rs"]
mod lifecycle_tests;
#[cfg(all(test, feature = "sbf-test"))]
#[path = "../tests/common/mod.rs"]
mod test_common;

#[cfg(not(feature = "no-entrypoint"))]
use solana_program::entrypoint;
use solana_program::{
    account_info::AccountInfo,
    clock::Clock,
    entrypoint::ProgramResult,
    program::{invoke, invoke_signed},
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    sysvar::{clock as clock_sysvar, rent as rent_sysvar, SysvarSerialize},
};
use solana_system_interface::{instruction as system_instruction, program as system_program};

use error::EscrowError;
use instruction::{BindArgs, EscrowInstruction, FundArgs, ResolveArgs};
use state::{
    pack_vault, validate_vault, EscrowGuard, EscrowState, EscrowStatus, GUARD_LEN, STATE_LEN,
    VAULT_LEN,
};

pub const STATE_SEED: &[u8] = b"mizuki-escrow";
pub const VAULT_SEED: &[u8] = b"mizuki-vault";
pub const GUARD_SEED: &[u8] = b"mizuki-guard";

#[cfg(not(feature = "no-entrypoint"))]
entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    match EscrowInstruction::decode(instruction_data)? {
        EscrowInstruction::Fund(args) => process_fund(program_id, accounts, args),
        EscrowInstruction::Bind(args) => process_bind(program_id, accounts, args),
        EscrowInstruction::Release(args) => process_release(program_id, accounts, args),
        EscrowInstruction::Refund(args) => process_refund(program_id, accounts, args),
    }
}

fn process_fund(program_id: &Pubkey, accounts: &[AccountInfo], args: FundArgs) -> ProgramResult {
    let [authority, state, vault, guard, system, clock_account, rent_account] = accounts else {
        return Err(EscrowError::InvalidAccountCount.into());
    };

    require_signer(authority)?;
    require_wallet(authority)?;
    require_writable(authority)?;
    require_writable(state)?;
    require_writable(vault)?;
    require_writable(guard)?;
    if system.key != &system_program::ID {
        return Err(EscrowError::InvalidSystemProgram.into());
    }
    if clock_account.key != &clock_sysvar::ID {
        return Err(EscrowError::InvalidClock.into());
    }
    if rent_account.key != &rent_sysvar::ID {
        return Err(EscrowError::InvalidRent.into());
    }
    if args.amount_lamports == 0 {
        return Err(EscrowError::ZeroAmount.into());
    }
    if args.bounty_id == [0; 32] || args.acceptance_commitment == [0; 32] {
        return Err(EscrowError::InvalidCommitment.into());
    }

    let clock = Clock::from_account_info(clock_account)?;
    if args.offer_expires_at <= clock.unix_timestamp {
        return Err(EscrowError::InvalidExpiry.into());
    }

    let (expected_state, state_bump) = Pubkey::find_program_address(
        &[STATE_SEED, authority.key.as_ref(), &args.bounty_id],
        program_id,
    );
    if state.key != &expected_state || args.state_bump != state_bump {
        return Err(EscrowError::InvalidPda.into());
    }
    let (expected_vault, vault_bump) =
        Pubkey::find_program_address(&[VAULT_SEED, state.key.as_ref()], program_id);
    if vault.key != &expected_vault || args.vault_bump != vault_bump {
        return Err(EscrowError::InvalidPda.into());
    }
    let (expected_guard, guard_bump) = Pubkey::find_program_address(
        &[GUARD_SEED, authority.key.as_ref(), &args.bounty_id],
        program_id,
    );
    if guard.key != &expected_guard || args.guard_bump != guard_bump {
        return Err(EscrowError::InvalidPda.into());
    }
    require_available_pda(state)?;
    require_available_pda(vault)?;
    require_available_pda(guard)?;

    let rent = Rent::from_account_info(rent_account)?;
    let state_rent = rent.minimum_balance(STATE_LEN);
    let vault_rent = rent.minimum_balance(VAULT_LEN);
    let guard_rent = rent.minimum_balance(GUARD_LEN);
    let vault_lamports = vault_rent
        .checked_add(args.amount_lamports)
        .ok_or(EscrowError::ArithmeticOverflow)?;
    let state_bump_seed = [state_bump];
    let state_seeds: &[&[u8]] = &[
        STATE_SEED,
        authority.key.as_ref(),
        &args.bounty_id,
        &state_bump_seed,
    ];
    create_pda_account(
        authority,
        state,
        system,
        state_rent,
        STATE_LEN,
        program_id,
        state_seeds,
    )?;

    let vault_bump_seed = [vault_bump];
    let vault_seeds: &[&[u8]] = &[VAULT_SEED, state.key.as_ref(), &vault_bump_seed];
    create_pda_account(
        authority,
        vault,
        system,
        vault_lamports,
        VAULT_LEN,
        program_id,
        vault_seeds,
    )?;

    let guard_bump_seed = [guard_bump];
    let guard_seeds: &[&[u8]] = &[
        GUARD_SEED,
        authority.key.as_ref(),
        &args.bounty_id,
        &guard_bump_seed,
    ];
    create_pda_account(
        authority,
        guard,
        system,
        guard_rent,
        GUARD_LEN,
        program_id,
        guard_seeds,
    )?;

    let escrow = EscrowState {
        status: EscrowStatus::Funded,
        state_bump,
        vault_bump,
        authority: *authority.key,
        claimant: Pubkey::default(),
        bounty_id: args.bounty_id,
        amount_lamports: args.amount_lamports,
        created_at: clock.unix_timestamp,
        offer_expires_at: args.offer_expires_at,
        claim_expires_at: 0,
        acceptance_commitment: args.acceptance_commitment,
        claim_commitment: [0; 32],
        resolution_evidence: [0; 32],
    };
    escrow.pack(&mut state.try_borrow_mut_data()?)?;
    pack_vault(state.key, &mut vault.try_borrow_mut_data()?)?;
    EscrowGuard {
        status: escrow.status,
        bump: guard_bump,
        authority: escrow.authority,
        bounty_id: escrow.bounty_id,
        state_commitment: escrow.commitment()?,
    }
    .pack(&mut guard.try_borrow_mut_data()?)
}

fn process_bind(program_id: &Pubkey, accounts: &[AccountInfo], args: BindArgs) -> ProgramResult {
    let [authority, state, guard, clock_account] = accounts else {
        return Err(EscrowError::InvalidAccountCount.into());
    };
    require_signer(authority)?;
    require_wallet(authority)?;
    require_writable(state)?;
    require_writable(guard)?;
    if clock_account.key != &clock_sysvar::ID {
        return Err(EscrowError::InvalidClock.into());
    }
    if args.claimant == Pubkey::default() || args.claim_commitment == [0; 32] {
        return Err(EscrowError::InvalidCommitment.into());
    }

    let mut escrow = load_state(program_id, authority, state, &args.bounty_id)?;
    let mut escrow_guard = load_guard(program_id, authority, guard, &escrow)?;
    if escrow.status != EscrowStatus::Funded {
        return Err(EscrowError::InvalidState.into());
    }
    let clock = Clock::from_account_info(clock_account)?;
    if clock.unix_timestamp >= escrow.offer_expires_at
        || args.claim_expires_at <= clock.unix_timestamp
    {
        return Err(EscrowError::InvalidExpiry.into());
    }
    let (vault, _) = Pubkey::find_program_address(&[VAULT_SEED, state.key.as_ref()], program_id);
    if args.claimant == *authority.key || args.claimant == *state.key || args.claimant == vault {
        return Err(EscrowError::InvalidClaimant.into());
    }

    escrow.status = EscrowStatus::Bound;
    escrow.claimant = args.claimant;
    escrow.claim_expires_at = args.claim_expires_at;
    escrow.claim_commitment = args.claim_commitment;
    escrow.pack(&mut state.try_borrow_mut_data()?)?;
    escrow_guard.status = escrow.status;
    escrow_guard.state_commitment = escrow.commitment()?;
    escrow_guard.pack(&mut guard.try_borrow_mut_data()?)
}

fn process_release(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    args: ResolveArgs,
) -> ProgramResult {
    let [authority, state, vault, guard, claimant, clock_account] = accounts else {
        return Err(EscrowError::InvalidAccountCount.into());
    };
    require_signer(authority)?;
    require_wallet(authority)?;
    require_writable(authority)?;
    require_writable(state)?;
    require_writable(vault)?;
    require_writable(guard)?;
    require_writable(claimant)?;
    require_wallet(claimant)?;
    if clock_account.key != &clock_sysvar::ID {
        return Err(EscrowError::InvalidClock.into());
    }
    if args.resolution_evidence == [0; 32] {
        return Err(EscrowError::InvalidCommitment.into());
    }

    let mut escrow = load_state(program_id, authority, state, &args.bounty_id)?;
    let mut escrow_guard = load_guard(program_id, authority, guard, &escrow)?;
    if escrow.status != EscrowStatus::Bound {
        return Err(EscrowError::InvalidState.into());
    }
    if Clock::from_account_info(clock_account)?.unix_timestamp >= escrow.claim_expires_at {
        return Err(EscrowError::InvalidExpiry.into());
    }
    if claimant.key != &escrow.claimant || claimant.key == authority.key {
        return Err(EscrowError::InvalidClaimant.into());
    }
    validate_vault_account(
        program_id,
        state,
        vault,
        escrow.vault_bump,
        escrow.amount_lamports,
    )?;

    escrow.status = EscrowStatus::Released;
    escrow.resolution_evidence = args.resolution_evidence;
    escrow.pack(&mut state.try_borrow_mut_data()?)?;
    escrow_guard.status = escrow.status;
    escrow_guard.state_commitment = escrow.commitment()?;
    escrow_guard.pack(&mut guard.try_borrow_mut_data()?)?;
    transfer_principal(vault, claimant, escrow.amount_lamports)?;
    close_program_account(vault, authority)?;
    close_program_account(state, authority)
}

fn process_refund(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    args: ResolveArgs,
) -> ProgramResult {
    let [authority, state, vault, guard, clock_account] = accounts else {
        return Err(EscrowError::InvalidAccountCount.into());
    };
    require_signer(authority)?;
    require_wallet(authority)?;
    require_writable(authority)?;
    require_writable(state)?;
    require_writable(vault)?;
    require_writable(guard)?;
    if clock_account.key != &clock_sysvar::ID {
        return Err(EscrowError::InvalidClock.into());
    }
    if args.resolution_evidence == [0; 32] {
        return Err(EscrowError::InvalidCommitment.into());
    }

    let mut escrow = load_state(program_id, authority, state, &args.bounty_id)?;
    let mut escrow_guard = load_guard(program_id, authority, guard, &escrow)?;
    let clock = Clock::from_account_info(clock_account)?;
    let refund_at = match escrow.status {
        EscrowStatus::Funded => escrow.offer_expires_at,
        EscrowStatus::Bound => escrow.claim_expires_at,
        _ => return Err(EscrowError::InvalidState.into()),
    };
    if clock.unix_timestamp < refund_at {
        return Err(EscrowError::InvalidExpiry.into());
    }
    validate_vault_account(
        program_id,
        state,
        vault,
        escrow.vault_bump,
        escrow.amount_lamports,
    )?;

    escrow.status = EscrowStatus::Refunded;
    escrow.resolution_evidence = args.resolution_evidence;
    escrow.pack(&mut state.try_borrow_mut_data()?)?;
    escrow_guard.status = escrow.status;
    escrow_guard.state_commitment = escrow.commitment()?;
    escrow_guard.pack(&mut guard.try_borrow_mut_data()?)?;
    transfer_principal(vault, authority, escrow.amount_lamports)?;
    close_program_account(vault, authority)?;
    close_program_account(state, authority)
}

fn load_state(
    program_id: &Pubkey,
    authority: &AccountInfo,
    state: &AccountInfo,
    bounty_id: &[u8; 32],
) -> Result<EscrowState, ProgramError> {
    if state.owner != program_id {
        return Err(ProgramError::IncorrectProgramId);
    }
    let escrow = EscrowState::unpack(&state.try_borrow_data()?)?;
    if !authority.is_signer || authority.key != &escrow.authority {
        return Err(EscrowError::InvalidAuthority.into());
    }
    if bounty_id != &escrow.bounty_id {
        return Err(EscrowError::InvalidPda.into());
    }
    let (expected, bump) =
        Pubkey::find_program_address(&[STATE_SEED, authority.key.as_ref(), bounty_id], program_id);
    if state.key != &expected || escrow.state_bump != bump {
        return Err(EscrowError::InvalidPda.into());
    }
    Ok(escrow)
}

fn validate_vault_account(
    program_id: &Pubkey,
    state: &AccountInfo,
    vault: &AccountInfo,
    stored_bump: u8,
    amount: u64,
) -> ProgramResult {
    let (expected, bump) =
        Pubkey::find_program_address(&[VAULT_SEED, state.key.as_ref()], program_id);
    if vault.key != &expected || bump != stored_bump {
        return Err(EscrowError::InvalidPda.into());
    }
    if vault.owner != program_id {
        return Err(ProgramError::IncorrectProgramId);
    }
    validate_vault(state.key, &vault.try_borrow_data()?)?;
    if vault.lamports() < amount {
        return Err(EscrowError::InsufficientVaultBalance.into());
    }
    Ok(())
}

fn load_guard(
    program_id: &Pubkey,
    authority: &AccountInfo,
    guard: &AccountInfo,
    escrow: &EscrowState,
) -> Result<EscrowGuard, ProgramError> {
    if guard.owner != program_id {
        return Err(ProgramError::IncorrectProgramId);
    }
    let loaded = EscrowGuard::unpack(&guard.try_borrow_data()?)?;
    let (expected, bump) = Pubkey::find_program_address(
        &[GUARD_SEED, authority.key.as_ref(), &escrow.bounty_id],
        program_id,
    );
    if guard.key != &expected
        || loaded.bump != bump
        || loaded.authority != escrow.authority
        || loaded.bounty_id != escrow.bounty_id
        || loaded.status != escrow.status
        || loaded.state_commitment != escrow.commitment()?
    {
        return Err(EscrowError::InvalidState.into());
    }
    Ok(loaded)
}

fn transfer_principal(from: &AccountInfo, to: &AccountInfo, amount: u64) -> ProgramResult {
    if from.key == to.key {
        return Err(EscrowError::InvalidVault.into());
    }
    let from_balance = from
        .lamports()
        .checked_sub(amount)
        .ok_or(EscrowError::InsufficientVaultBalance)?;
    let to_balance = to
        .lamports()
        .checked_add(amount)
        .ok_or(EscrowError::ArithmeticOverflow)?;
    **from.try_borrow_mut_lamports()? = from_balance;
    **to.try_borrow_mut_lamports()? = to_balance;
    Ok(())
}

fn close_program_account(account: &AccountInfo, authority: &AccountInfo) -> ProgramResult {
    if account.owner == &system_program::ID || account.key == authority.key {
        return Err(EscrowError::InvalidState.into());
    }
    let amount = account.lamports();
    let authority_balance = authority
        .lamports()
        .checked_add(amount)
        .ok_or(EscrowError::ArithmeticOverflow)?;
    account.try_borrow_mut_data()?.fill(0);
    account.resize(0)?;
    account.assign(&system_program::ID);
    **account.try_borrow_mut_lamports()? = 0;
    **authority.try_borrow_mut_lamports()? = authority_balance;
    Ok(())
}

fn require_signer(account: &AccountInfo) -> ProgramResult {
    if !account.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    Ok(())
}

fn require_writable(account: &AccountInfo) -> ProgramResult {
    if !account.is_writable {
        return Err(EscrowError::AccountNotWritable.into());
    }
    Ok(())
}

fn require_wallet(account: &AccountInfo) -> ProgramResult {
    if account.owner != &system_program::ID || !account.data_is_empty() {
        return Err(ProgramError::InvalidAccountOwner);
    }
    Ok(())
}

fn create_pda_account<'a>(
    authority: &AccountInfo<'a>,
    account: &AccountInfo<'a>,
    system: &AccountInfo<'a>,
    required_lamports: u64,
    space: usize,
    owner: &Pubkey,
    signer_seeds: &[&[u8]],
) -> ProgramResult {
    if account.lamports() == 0 {
        return invoke_signed(
            &system_instruction::create_account(
                authority.key,
                account.key,
                required_lamports,
                space as u64,
                owner,
            ),
            &[authority.clone(), account.clone(), system.clone()],
            &[signer_seeds],
        );
    }

    let top_up = required_lamports.saturating_sub(account.lamports());
    if top_up > 0 {
        invoke(
            &system_instruction::transfer(authority.key, account.key, top_up),
            &[authority.clone(), account.clone(), system.clone()],
        )?;
    }
    invoke_signed(
        &system_instruction::allocate(account.key, space as u64),
        &[account.clone(), system.clone()],
        &[signer_seeds],
    )?;
    invoke_signed(
        &system_instruction::assign(account.key, owner),
        &[account.clone(), system.clone()],
        &[signer_seeds],
    )
}

fn require_available_pda(account: &AccountInfo) -> ProgramResult {
    if account.owner != &system_program::ID || !account.data_is_empty() {
        return Err(EscrowError::AlreadyInitialized.into());
    }
    Ok(())
}
