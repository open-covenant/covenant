#![allow(dead_code)]

use crate::{
    instruction::{BindArgs, EscrowInstruction, FundArgs, ResolveArgs},
    state::{EscrowGuard, EscrowState},
    GUARD_SEED, STATE_SEED, VAULT_SEED,
};
use litesvm::LiteSVM;
use solana_sdk::{
    clock::Clock,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    sysvar,
    transaction::TransactionError,
};
use solana_system_interface::program as system_program;
use solana_transaction::Transaction;

pub const PROGRAM_ID: Pubkey = Pubkey::new_from_array([
    77, 105, 122, 117, 107, 105, 45, 101, 115, 99, 114, 111, 119, 45, 118, 49, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 1,
]);
pub const BOUNTY: [u8; 32] = [11; 32];
pub const AMOUNT: u64 = 2_000_000;
pub const NOW: i64 = 1_000;
pub const OFFER_EXPIRY: i64 = 2_000;
pub const CLAIM_EXPIRY: i64 = 3_000;

const SO_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/target/deploy/mizuki_escrow_program.so"
);

pub struct Env {
    pub svm: LiteSVM,
    pub payer: Keypair,
    pub authority: Keypair,
    pub claimant: Keypair,
    pub stranger: Keypair,
}

pub fn boot() -> Env {
    let mut svm = LiteSVM::new();
    svm.add_program_from_file(PROGRAM_ID, SO_PATH)
        .expect("load program; run cargo build-sbf first");
    let payer = Keypair::new();
    let authority = Keypair::new();
    let claimant = Keypair::new();
    let stranger = Keypair::new();
    for wallet in [
        payer.pubkey(),
        authority.pubkey(),
        claimant.pubkey(),
        stranger.pubkey(),
    ] {
        svm.airdrop(&wallet, 1_000_000_000).unwrap();
    }
    warp(&mut svm, NOW);
    Env {
        svm,
        payer,
        authority,
        claimant,
        stranger,
    }
}

pub fn state_pda(authority: &Pubkey, bounty: &[u8; 32]) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[STATE_SEED, authority.as_ref(), bounty], &PROGRAM_ID)
}

pub fn vault_pda(state: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[VAULT_SEED, state.as_ref()], &PROGRAM_ID)
}

pub fn guard_pda(authority: &Pubkey, bounty: &[u8; 32]) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[GUARD_SEED, authority.as_ref(), bounty], &PROGRAM_ID)
}

pub fn fund_ix(authority: Pubkey, bounty: [u8; 32], offer_expiry: i64) -> Instruction {
    let (state, state_bump) = state_pda(&authority, &bounty);
    let (vault, vault_bump) = vault_pda(&state);
    let (guard, guard_bump) = guard_pda(&authority, &bounty);
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(authority, true),
            AccountMeta::new(state, false),
            AccountMeta::new(vault, false),
            AccountMeta::new(guard, false),
            AccountMeta::new_readonly(system_program::ID, false),
            AccountMeta::new_readonly(sysvar::clock::ID, false),
            AccountMeta::new_readonly(sysvar::rent::ID, false),
        ],
        data: EscrowInstruction::Fund(FundArgs {
            bounty_id: bounty,
            amount_lamports: AMOUNT,
            offer_expires_at: offer_expiry,
            acceptance_commitment: [21; 32],
            state_bump,
            vault_bump,
            guard_bump,
        })
        .encode(),
    }
}

pub fn bind_ix(
    authority: Pubkey,
    bounty: [u8; 32],
    claimant: Pubkey,
    claim_expiry: i64,
) -> Instruction {
    let state = state_pda(&authority, &bounty).0;
    let guard = guard_pda(&authority, &bounty).0;
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(state, false),
            AccountMeta::new(guard, false),
            AccountMeta::new_readonly(sysvar::clock::ID, false),
        ],
        data: EscrowInstruction::Bind(BindArgs {
            bounty_id: bounty,
            claimant,
            claim_expires_at: claim_expiry,
            claim_commitment: [22; 32],
        })
        .encode(),
    }
}

pub fn release_ix(authority: Pubkey, bounty: [u8; 32], claimant: Pubkey) -> Instruction {
    let state = state_pda(&authority, &bounty).0;
    let vault = vault_pda(&state).0;
    let guard = guard_pda(&authority, &bounty).0;
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(authority, true),
            AccountMeta::new(state, false),
            AccountMeta::new(vault, false),
            AccountMeta::new(guard, false),
            AccountMeta::new(claimant, false),
            AccountMeta::new_readonly(sysvar::clock::ID, false),
        ],
        data: EscrowInstruction::Release(ResolveArgs {
            bounty_id: bounty,
            resolution_evidence: [23; 32],
        })
        .encode(),
    }
}

pub fn refund_ix(authority: Pubkey, bounty: [u8; 32]) -> Instruction {
    let state = state_pda(&authority, &bounty).0;
    let vault = vault_pda(&state).0;
    let guard = guard_pda(&authority, &bounty).0;
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(authority, true),
            AccountMeta::new(state, false),
            AccountMeta::new(vault, false),
            AccountMeta::new(guard, false),
            AccountMeta::new_readonly(sysvar::clock::ID, false),
        ],
        data: EscrowInstruction::Refund(ResolveArgs {
            bounty_id: bounty,
            resolution_evidence: [24; 32],
        })
        .encode(),
    }
}

pub fn fund(env: &mut Env) -> Result<(), TransactionError> {
    let ix = fund_ix(env.authority.pubkey(), BOUNTY, OFFER_EXPIRY);
    send(&mut env.svm, &env.payer, &[ix], &[&env.authority])
}

pub fn bind(env: &mut Env) -> Result<(), TransactionError> {
    let ix = bind_ix(
        env.authority.pubkey(),
        BOUNTY,
        env.claimant.pubkey(),
        CLAIM_EXPIRY,
    );
    send(&mut env.svm, &env.payer, &[ix], &[&env.authority])
}

pub fn send(
    svm: &mut LiteSVM,
    payer: &Keypair,
    instructions: &[Instruction],
    signers: &[&Keypair],
) -> Result<(), TransactionError> {
    let mut all = vec![payer];
    all.extend_from_slice(signers);
    let transaction = Transaction::new_signed_with_payer(
        instructions,
        Some(&payer.pubkey()),
        &all,
        svm.latest_blockhash(),
    );
    svm.send_transaction(transaction)
        .map(|_| ())
        .map_err(|metadata| metadata.err)
}

pub fn warp(svm: &mut LiteSVM, timestamp: i64) {
    let mut clock: Clock = svm.get_sysvar();
    clock.unix_timestamp = timestamp;
    svm.set_sysvar(&clock);
    svm.expire_blockhash();
}

pub fn state(env: &Env) -> EscrowState {
    let state = state_pda(&env.authority.pubkey(), &BOUNTY).0;
    let account = env.svm.get_account(&state).expect("escrow state");
    EscrowState::unpack(&account.data).expect("valid escrow state")
}

pub fn guard(env: &Env) -> EscrowGuard {
    let guard = guard_pda(&env.authority.pubkey(), &BOUNTY).0;
    let account = env.svm.get_account(&guard).expect("escrow guard");
    EscrowGuard::unpack(&account.data).expect("valid escrow guard")
}

pub fn balance(env: &Env, address: &Pubkey) -> u64 {
    env.svm
        .get_account(address)
        .map(|account| account.lamports)
        .unwrap_or(0)
}

pub fn vault(env: &Env) -> Pubkey {
    let state = state_pda(&env.authority.pubkey(), &BOUNTY).0;
    vault_pda(&state).0
}
