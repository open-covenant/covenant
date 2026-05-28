//! On-chain dispatch for the Covenant percolator-keeper bond.
//!
//! The shared library (`covenant-percolator-bond`) owns the state
//! types, canonical scope encoding, and pure verifier. This crate is
//! the SBPF wrapper that:
//!
//!   1. Decodes the instruction tag + args from `data`
//!   2. Validates the accounts the runtime passed in (signer flags,
//!      writable flags, PDA seeds, ownership)
//!   3. Calls the shared `verify_slash` / `BondAccount` codecs
//!   4. Performs the lamport transfers via System program CPI or
//!      direct write to `AccountInfo::try_borrow_mut_lamports`
//!
//! The off-chain `covenant-percolator-bond::program` module hosts a
//! pure-Rust simulation of these handlers — the host tests there +
//! `solana-program-test` tests here together cover the full surface.
//!
//! Wire format (mirrors `covenant_percolator_bond::program::tag`):
//!   0 Initialize(keeper, scope_hash, slash_recipient, created_slot)
//!   1 Deposit(lamports)
//!   2 SetPaused(paused)
//!   3 Withdraw(lamports)
//!   4 Slash(scope_len, scope_bytes…, receipt_id, slot, market,
//!          action_bit, has_asset, asset_index)

#![deny(unsafe_code)]

use solana_program::account_info::AccountInfo;
use solana_program::entrypoint;
use solana_program::entrypoint::ProgramResult;
use solana_program::msg;
use solana_program::program::invoke_signed;
use solana_program::program_error::ProgramError;
use solana_program::pubkey::Pubkey;
use solana_program::rent::Rent;
use solana_program::system_instruction;
use solana_program::sysvar::Sysvar;

use covenant_percolator_bond::evidence::{verify_slash, AttestedAction, SlashEvidence};
use covenant_percolator_bond::scope::{ActionMask, BondScope, ScopeHash};
use covenant_percolator_bond::state::{BondAccount, BOND_VERSION};
use covenant_percolator_bond::{BOND_SEED, SLASH_SEED};

entrypoint!(process_instruction);

mod tag {
    pub const INITIALIZE: u8 = 0;
    pub const DEPOSIT: u8 = 1;
    pub const SET_PAUSED: u8 = 2;
    pub const WITHDRAW: u8 = 3;
    pub const SLASH: u8 = 4;
}

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    if data.is_empty() {
        return Err(ProgramError::InvalidInstructionData);
    }
    let (tag_byte, rest) = data.split_first().unwrap();
    match *tag_byte {
        tag::INITIALIZE => initialize(program_id, accounts, rest),
        tag::DEPOSIT => deposit(program_id, accounts, rest),
        tag::SET_PAUSED => set_paused(program_id, accounts, rest),
        tag::WITHDRAW => withdraw(program_id, accounts, rest),
        tag::SLASH => slash(program_id, accounts, rest),
        other => {
            msg!("unknown instruction tag: {}", other);
            Err(ProgramError::InvalidInstructionData)
        }
    }
}

// ---------- helpers ----------

fn read_32(rest: &mut &[u8]) -> Result<[u8; 32], ProgramError> {
    if rest.len() < 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&rest[..32]);
    *rest = &rest[32..];
    Ok(out)
}
fn read_16(rest: &mut &[u8]) -> Result<[u8; 16], ProgramError> {
    if rest.len() < 16 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let mut out = [0u8; 16];
    out.copy_from_slice(&rest[..16]);
    *rest = &rest[16..];
    Ok(out)
}
fn read_u8(rest: &mut &[u8]) -> Result<u8, ProgramError> {
    if rest.is_empty() {
        return Err(ProgramError::InvalidInstructionData);
    }
    let v = rest[0];
    *rest = &rest[1..];
    Ok(v)
}
fn read_u16(rest: &mut &[u8]) -> Result<u16, ProgramError> {
    if rest.len() < 2 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let v = u16::from_le_bytes([rest[0], rest[1]]);
    *rest = &rest[2..];
    Ok(v)
}
fn read_u64(rest: &mut &[u8]) -> Result<u64, ProgramError> {
    if rest.len() < 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&rest[..8]);
    *rest = &rest[8..];
    Ok(u64::from_le_bytes(buf))
}

/// Confirm `bond_account.key` matches the expected PDA derivation
/// `[b"bond", keeper.as_ref(), bump]`. Returns the bump so callers
/// can sign for the PDA in CPI.
fn verify_bond_pda(
    program_id: &Pubkey,
    bond_key: &Pubkey,
    keeper: &Pubkey,
) -> Result<u8, ProgramError> {
    let (expected, bump) =
        Pubkey::find_program_address(&[BOND_SEED, keeper.as_ref()], program_id);
    if &expected != bond_key {
        msg!("bond PDA mismatch");
        return Err(ProgramError::InvalidSeeds);
    }
    Ok(bump)
}

fn verify_slash_receipt_pda(
    program_id: &Pubkey,
    receipt_key: &Pubkey,
    bond_key: &Pubkey,
    receipt_id: &[u8; 16],
) -> Result<u8, ProgramError> {
    let (expected, bump) = Pubkey::find_program_address(
        &[SLASH_SEED, bond_key.as_ref(), receipt_id.as_ref()],
        program_id,
    );
    if &expected != receipt_key {
        msg!("slash receipt PDA mismatch");
        return Err(ProgramError::InvalidSeeds);
    }
    Ok(bump)
}

fn rent_for(size: usize) -> Result<u64, ProgramError> {
    let rent = Rent::get()?;
    Ok(rent.minimum_balance(size))
}

// ---------- handlers ----------

/// Accounts:
///   0. bond_pda            (writable, init via CPI)
///   1. operator            (writable, signer — pays rent)
///   2. system_program      (readonly)
///
/// Data (after tag byte): keeper(32) | scope_hash(32) | slash_recipient(32) | created_slot(8)
fn initialize(program_id: &Pubkey, accounts: &[AccountInfo], mut rest: &[u8]) -> ProgramResult {
    let bond_ai = accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
    let operator_ai = accounts.get(1).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let system_ai = accounts.get(2).ok_or(ProgramError::NotEnoughAccountKeys)?;

    if !operator_ai.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if !bond_ai.is_writable {
        return Err(ProgramError::InvalidAccountData);
    }

    let keeper = read_32(&mut rest)?;
    let scope_hash = read_32(&mut rest)?;
    let slash_recipient = read_32(&mut rest)?;
    let created_slot = read_u64(&mut rest)?;

    let keeper_pk = Pubkey::new_from_array(keeper);
    let bump = verify_bond_pda(program_id, bond_ai.key, &keeper_pk)?;

    // Refuse re-init on a populated bond.
    if bond_ai.data_len() != 0 {
        return Err(ProgramError::AccountAlreadyInitialized);
    }

    let space = BondAccount::ON_CHAIN_SIZE;
    let lamports = rent_for(space)?;

    invoke_signed(
        &system_instruction::create_account(
            operator_ai.key,
            bond_ai.key,
            lamports,
            space as u64,
            program_id,
        ),
        &[
            operator_ai.clone(),
            bond_ai.clone(),
            system_ai.clone(),
        ],
        &[&[BOND_SEED, keeper_pk.as_ref(), &[bump]]],
    )?;

    let bond = BondAccount::new(
        keeper,
        operator_ai.key.to_bytes(),
        ScopeHash(scope_hash),
        slash_recipient,
        // Account lamports are the rent reserve at this point; the
        // BondAccount's `lamports` field tracks the *bond escrow* and
        // starts at 0. Operators top up via Deposit.
        0,
        created_slot,
    );
    let bytes = bond.encode();
    let mut data = bond_ai.try_borrow_mut_data()?;
    data[..bytes.len()].copy_from_slice(&bytes);
    Ok(())
}

/// Accounts:
///   0. bond_pda            (writable)
///   1. funder              (writable, signer)
///   2. system_program      (readonly)
///
/// Data: lamports(8)
fn deposit(program_id: &Pubkey, accounts: &[AccountInfo], mut rest: &[u8]) -> ProgramResult {
    let bond_ai = accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
    let funder_ai = accounts.get(1).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let system_ai = accounts.get(2).ok_or(ProgramError::NotEnoughAccountKeys)?;
    if !funder_ai.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if bond_ai.owner != program_id {
        return Err(ProgramError::IncorrectProgramId);
    }

    let lamports = read_u64(&mut rest)?;
    if lamports == 0 {
        return Err(ProgramError::InvalidInstructionData);
    }

    // Reject deposits on a slashed bond — the slasher already drained
    // it; depositing more is a confusing footgun.
    {
        let data = bond_ai.try_borrow_data()?;
        let bond = BondAccount::decode(&data).map_err(|_| ProgramError::InvalidAccountData)?;
        if bond.slashed != 0 {
            return Err(ProgramError::Custom(1));
        }
    }

    // System transfer the lamports in; the bond account's lamport
    // balance grows, and we update the in-data `lamports` field to
    // mirror the *bond escrow* portion (i.e., excluding rent reserve).
    invoke_signed(
        &system_instruction::transfer(funder_ai.key, bond_ai.key, lamports),
        &[funder_ai.clone(), bond_ai.clone(), system_ai.clone()],
        &[],
    )?;
    {
        let mut data = bond_ai.try_borrow_mut_data()?;
        let mut bond =
            BondAccount::decode(&data).map_err(|_| ProgramError::InvalidAccountData)?;
        bond.lamports = bond
            .lamports
            .checked_add(lamports)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        let bytes = bond.encode();
        data[..bytes.len()].copy_from_slice(&bytes);
    }
    Ok(())
}

/// Accounts:
///   0. bond_pda            (writable)
///   1. operator            (signer, readonly)
///
/// Data: paused(1)
fn set_paused(program_id: &Pubkey, accounts: &[AccountInfo], mut rest: &[u8]) -> ProgramResult {
    let bond_ai = accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
    let operator_ai = accounts.get(1).ok_or(ProgramError::NotEnoughAccountKeys)?;
    if !operator_ai.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if bond_ai.owner != program_id {
        return Err(ProgramError::IncorrectProgramId);
    }
    let paused = read_u8(&mut rest)?;

    let mut data = bond_ai.try_borrow_mut_data()?;
    let mut bond = BondAccount::decode(&data).map_err(|_| ProgramError::InvalidAccountData)?;
    if &bond.operator != operator_ai.key.as_ref() {
        return Err(ProgramError::IllegalOwner);
    }
    bond.paused = paused & 1;
    let bytes = bond.encode();
    data[..bytes.len()].copy_from_slice(&bytes);
    Ok(())
}

/// Accounts:
///   0. bond_pda            (writable)
///   1. keeper              (signer)
///   2. dest                (writable)
///
/// Data: lamports(8)
fn withdraw(program_id: &Pubkey, accounts: &[AccountInfo], mut rest: &[u8]) -> ProgramResult {
    let bond_ai = accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
    let keeper_ai = accounts.get(1).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let dest_ai = accounts.get(2).ok_or(ProgramError::NotEnoughAccountKeys)?;
    if !keeper_ai.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if bond_ai.owner != program_id {
        return Err(ProgramError::IncorrectProgramId);
    }
    let lamports = read_u64(&mut rest)?;

    let mut data = bond_ai.try_borrow_mut_data()?;
    let mut bond = BondAccount::decode(&data).map_err(|_| ProgramError::InvalidAccountData)?;
    if &bond.keeper != keeper_ai.key.as_ref() {
        return Err(ProgramError::IllegalOwner);
    }
    if bond.slashed != 0 {
        return Err(ProgramError::Custom(2));
    }
    if bond.paused == 0 {
        return Err(ProgramError::Custom(3));
    }
    if lamports > bond.lamports {
        return Err(ProgramError::InsufficientFunds);
    }
    // Direct lamport adjustment (no system-program CPI needed — the
    // bond is program-owned, we can write its lamports field
    // directly).
    **bond_ai.try_borrow_mut_lamports()? = bond_ai
        .lamports()
        .checked_sub(lamports)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    **dest_ai.try_borrow_mut_lamports()? = dest_ai
        .lamports()
        .checked_add(lamports)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    bond.lamports -= lamports;
    let bytes = bond.encode();
    data[..bytes.len()].copy_from_slice(&bytes);
    Ok(())
}

/// Accounts:
///   0. bond_pda            (writable)
///   1. recipient           (writable; must equal bond.slash_recipient)
///   2. slash_receipt_pda   (writable, init via CPI; replay protection)
///   3. payer               (writable, signer — pays rent for receipt)
///   4. system_program      (readonly)
///
/// Data: scope_len(2) | scope_bytes | receipt_id(16) | slot(8) |
///       market(32) | action_bit(1) | has_asset(1) | asset_index(2)
fn slash(program_id: &Pubkey, accounts: &[AccountInfo], mut rest: &[u8]) -> ProgramResult {
    let bond_ai = accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
    let recipient_ai = accounts.get(1).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let receipt_ai = accounts.get(2).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let payer_ai = accounts.get(3).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let system_ai = accounts.get(4).ok_or(ProgramError::NotEnoughAccountKeys)?;
    if !payer_ai.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if bond_ai.owner != program_id {
        return Err(ProgramError::IncorrectProgramId);
    }

    // Parse evidence.
    let scope_len = read_u16(&mut rest)? as usize;
    if rest.len() < scope_len {
        return Err(ProgramError::InvalidInstructionData);
    }
    let scope = decode_scope(&rest[..scope_len])?;
    rest = &rest[scope_len..];
    let receipt_id = read_16(&mut rest)?;
    let executed_slot = read_u64(&mut rest)?;
    let market = read_32(&mut rest)?;
    let action_bit = read_u8(&mut rest)?;
    let has_asset = read_u8(&mut rest)?;
    let asset_raw = read_u16(&mut rest)?;
    let asset_index = if has_asset != 0 { Some(asset_raw) } else { None };

    let evidence = SlashEvidence {
        scope,
        action: AttestedAction {
            receipt_id,
            executed_slot,
            market,
            action_bit,
            asset_index,
        },
    };

    // Decode + run the verifier.
    let mut data = bond_ai.try_borrow_mut_data()?;
    let mut bond = BondAccount::decode(&data).map_err(|_| ProgramError::InvalidAccountData)?;
    if &bond.slash_recipient != recipient_ai.key.as_ref() {
        return Err(ProgramError::IllegalOwner);
    }

    let verdict = match verify_slash(&bond, &evidence) {
        Ok(v) => v,
        Err(_) => return Err(ProgramError::Custom(4)),
    };

    // Allocate the slash-receipt PDA — this is replay protection. If
    // the PDA already exists, create_account fails and the whole tx
    // reverts.
    let bond_key = *bond_ai.key;
    let receipt_bump =
        verify_slash_receipt_pda(program_id, receipt_ai.key, &bond_key, &receipt_id)?;
    let receipt_lamports = rent_for(1)?; // 1 byte sentinel
    invoke_signed(
        &system_instruction::create_account(
            payer_ai.key,
            receipt_ai.key,
            receipt_lamports,
            1,
            program_id,
        ),
        &[
            payer_ai.clone(),
            receipt_ai.clone(),
            system_ai.clone(),
        ],
        &[&[SLASH_SEED, bond_key.as_ref(), &receipt_id, &[receipt_bump]]],
    )?;

    // Move lamports from the bond to the recipient.
    **bond_ai.try_borrow_mut_lamports()? = bond_ai
        .lamports()
        .checked_sub(verdict.slash_lamports)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    **recipient_ai.try_borrow_mut_lamports()? = recipient_ai
        .lamports()
        .checked_add(verdict.slash_lamports)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    bond.lamports = 0;
    bond.slashed = 1;
    let bytes = bond.encode();
    data[..bytes.len()].copy_from_slice(&bytes);
    msg!("slashed {} lamports", verdict.slash_lamports);
    Ok(())
}

fn decode_scope(bytes: &[u8]) -> Result<BondScope, ProgramError> {
    // Layout (mirrors BondScope::encode_canonical):
    //   u8(version) | 32B(market) | u8(action_mask) | u8(asset_kind)
    //   [if Some: u16(count) | u16 × count]
    //   u32(max_actions_per_tick)
    if bytes.len() < 1 + 32 + 1 + 1 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let mut p = 0;
    let version = bytes[p];
    p += 1;
    if version != BOND_VERSION {
        return Err(ProgramError::InvalidAccountData);
    }
    let mut market = [0u8; 32];
    market.copy_from_slice(&bytes[p..p + 32]);
    p += 32;
    let mask = bytes[p];
    p += 1;
    let asset_kind = bytes[p];
    p += 1;
    let allowed_assets = match asset_kind {
        0 => None,
        1 => {
            if bytes.len() < p + 2 {
                return Err(ProgramError::InvalidInstructionData);
            }
            let count = u16::from_le_bytes([bytes[p], bytes[p + 1]]) as usize;
            p += 2;
            if bytes.len() < p + 2 * count {
                return Err(ProgramError::InvalidInstructionData);
            }
            let mut v = Vec::with_capacity(count);
            for _ in 0..count {
                v.push(u16::from_le_bytes([bytes[p], bytes[p + 1]]));
                p += 2;
            }
            Some(v)
        }
        _ => return Err(ProgramError::InvalidInstructionData),
    };
    if bytes.len() < p + 4 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let cap = u32::from_le_bytes([bytes[p], bytes[p + 1], bytes[p + 2], bytes[p + 3]]);
    Ok(BondScope {
        version,
        market,
        allowed_actions: ActionMask(mask),
        allowed_assets,
        max_actions_per_tick: cap,
    })
}
