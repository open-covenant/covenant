//! Shared litesvm harness for the on-chain settlement program.
//!
//! Tests load the compiled `.so` into an in-process SVM and drive the real
//! instruction handlers (no mocks), so guard logic is exercised against actual
//! Anchor account validation and SPL token CPIs.

use anchor_lang::InstructionData;
use litesvm::LiteSVM;
use solana_sdk::{
    account::ReadableAccount,
    instruction::{AccountMeta, Instruction},
    program_pack::Pack,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_instruction, system_program,
    transaction::{Transaction, TransactionError},
};

use covenant_settlement_program::{instruction as ix, InitializeArgs, RegisterAgentArgs, ID};

const SO_PATH: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/deploy/covenant_settlement_program.so");

pub const DECIMALS: u8 = 0;

pub struct Env {
    pub svm: LiteSVM,
    pub payer: Keypair,
    pub slash_authority: Keypair,
    pub mint: Pubkey,
    pub config: Pubkey,
}

pub fn config_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"config"], &ID).0
}

pub fn agent_pda(agent_key: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(&[b"agent", agent_key], &ID).0
}

pub fn stake_pda(agent_key: &[u8; 32], owner: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"stake", agent_key, owner.as_ref()], &ID).0
}

/// Boot an SVM with the program loaded, a funded payer, a fresh COVNT mint, a
/// treasury token account, and `initialize` already executed.
pub fn boot() -> Env {
    let mut svm = LiteSVM::new();
    svm.add_program_from_file(ID, SO_PATH)
        .expect("load settlement.so (run `anchor build` first)");

    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 100_000_000_000).unwrap();

    let slash_authority = Keypair::new();
    let mint = create_mint(&mut svm, &payer);
    let treasury = create_token_account(&mut svm, &payer, &mint, &payer.pubkey());
    let config = config_pda();

    let data = ix::Initialize {
        args: InitializeArgs { slash_authority: slash_authority.pubkey(), credits_per_covnt: 10 },
    }
    .data();
    let metas = vec![
        AccountMeta::new(config, false),
        AccountMeta::new(payer.pubkey(), true),
        AccountMeta::new_readonly(mint, false),
        AccountMeta::new_readonly(treasury, false),
        AccountMeta::new_readonly(system_program::ID, false),
    ];
    send(&mut svm, &payer, &[Instruction { program_id: ID, accounts: metas, data }], &[])
        .expect("initialize");

    Env { svm, payer, slash_authority, mint, config }
}

pub fn register_agent(env: &mut Env, agent_key: &[u8; 32]) -> Pubkey {
    let agent = agent_pda(agent_key);
    let data = ix::RegisterAgent {
        args: RegisterAgentArgs {
            agent_key: *agent_key,
            metadata_hash: [0u8; 32],
            capability_hash: [0u8; 32],
        },
    }
    .data();
    let metas = vec![
        AccountMeta::new_readonly(env.config, false),
        AccountMeta::new(agent, false),
        AccountMeta::new(env.payer.pubkey(), true),
        AccountMeta::new_readonly(system_program::ID, false),
    ];
    let payer = env.payer.insecure_clone();
    send(&mut env.svm, &payer, &[Instruction { program_id: ID, accounts: metas, data }], &[])
        .expect("register_agent");
    agent
}

/// Open a stake position funded with `amount`. Returns (position PDA, stake vault).
pub fn stake(env: &mut Env, agent_key: &[u8; 32], amount: u64) -> (Pubkey, Pubkey) {
    let agent = agent_pda(agent_key);
    let owner = env.payer.pubkey();
    let position = stake_pda(agent_key, &owner);

    let owner_covnt = create_token_account(&mut env.svm, &env.payer.insecure_clone(), &env.mint, &owner);
    mint_to(&mut env.svm, &env.payer.insecure_clone(), &env.mint, &owner_covnt, amount);
    let stake_vault = create_token_account(&mut env.svm, &env.payer.insecure_clone(), &env.mint, &position);

    let data = ix::Stake { amount, lock_until: 0 }.data();
    let metas = vec![
        AccountMeta::new_readonly(env.config, false),
        AccountMeta::new(agent, false),
        AccountMeta::new(position, false),
        AccountMeta::new(owner, true),
        AccountMeta::new(owner_covnt, false),
        AccountMeta::new(stake_vault, false),
        AccountMeta::new_readonly(spl_token::ID, false),
        AccountMeta::new_readonly(system_program::ID, false),
    ];
    let payer = env.payer.insecure_clone();
    send(&mut env.svm, &payer, &[Instruction { program_id: ID, accounts: metas, data }], &[])
        .expect("stake");
    (position, stake_vault)
}

pub fn set_pause(env: &mut Env, paused: bool) {
    let data = ix::SetPause { paused }.data();
    let metas = vec![
        AccountMeta::new(env.config, false),
        AccountMeta::new_readonly(env.payer.pubkey(), true),
    ];
    let payer = env.payer.insecure_clone();
    send(&mut env.svm, &payer, &[Instruction { program_id: ID, accounts: metas, data }], &[])
        .expect("set_pause");
}

pub fn slash_stake(
    env: &mut Env,
    agent_key: &[u8; 32],
    position: &Pubkey,
    stake_vault: &Pubkey,
    slash_vault: &Pubkey,
    amount: u64,
) -> Result<(), TransactionError> {
    let agent = agent_pda(agent_key);
    let data = ix::SlashStake { amount, reason_hash: [0u8; 32] }.data();
    let metas = vec![
        AccountMeta::new_readonly(env.config, false),
        AccountMeta::new_readonly(env.slash_authority.pubkey(), true),
        AccountMeta::new(agent, false),
        AccountMeta::new(*position, false),
        AccountMeta::new(*stake_vault, false),
        AccountMeta::new(*slash_vault, false),
        AccountMeta::new_readonly(spl_token::ID, false),
    ];
    let payer = env.payer.insecure_clone();
    let slasher = env.slash_authority.insecure_clone();
    send(&mut env.svm, &payer, &[Instruction { program_id: ID, accounts: metas, data }], &[&slasher])
}

pub fn token_balance(env: &Env, account: &Pubkey) -> u64 {
    let acc = env.svm.get_account(account).expect("token account exists");
    spl_token::state::Account::unpack(acc.data()).expect("token account").amount
}

pub fn create_mint(svm: &mut LiteSVM, payer: &Keypair) -> Pubkey {
    let mint = Keypair::new();
    let rent = svm.minimum_balance_for_rent_exemption(spl_token::state::Mint::LEN);
    let create = system_instruction::create_account(
        &payer.pubkey(),
        &mint.pubkey(),
        rent,
        spl_token::state::Mint::LEN as u64,
        &spl_token::ID,
    );
    let init = spl_token::instruction::initialize_mint2(
        &spl_token::ID,
        &mint.pubkey(),
        &payer.pubkey(),
        None,
        DECIMALS,
    )
    .unwrap();
    send(svm, payer, &[create, init], &[&mint]).expect("create mint");
    mint.pubkey()
}

pub fn create_token_account(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint: &Pubkey,
    authority: &Pubkey,
) -> Pubkey {
    let acct = Keypair::new();
    let rent = svm.minimum_balance_for_rent_exemption(spl_token::state::Account::LEN);
    let create = system_instruction::create_account(
        &payer.pubkey(),
        &acct.pubkey(),
        rent,
        spl_token::state::Account::LEN as u64,
        &spl_token::ID,
    );
    let init = spl_token::instruction::initialize_account3(
        &spl_token::ID,
        &acct.pubkey(),
        mint,
        authority,
    )
    .unwrap();
    send(svm, payer, &[create, init], &[&acct]).expect("create token account");
    acct.pubkey()
}

pub fn mint_to(svm: &mut LiteSVM, payer: &Keypair, mint: &Pubkey, dest: &Pubkey, amount: u64) {
    let ix = spl_token::instruction::mint_to(
        &spl_token::ID,
        mint,
        dest,
        &payer.pubkey(),
        &[],
        amount,
    )
    .unwrap();
    send(svm, payer, &[ix], &[]).expect("mint_to");
}

pub fn send(
    svm: &mut LiteSVM,
    payer: &Keypair,
    ixs: &[Instruction],
    extra_signers: &[&Keypair],
) -> Result<(), TransactionError> {
    let mut signers = vec![payer];
    signers.extend_from_slice(extra_signers);
    let tx = Transaction::new_signed_with_payer(
        ixs,
        Some(&payer.pubkey()),
        &signers,
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).map(|_| ()).map_err(|e| e.err)
}

/// Anchor custom error code, if the failure was `InstructionError::Custom`.
pub fn custom_error(err: &TransactionError) -> Option<u32> {
    use solana_sdk::instruction::InstructionError;
    match err {
        TransactionError::InstructionError(_, InstructionError::Custom(code)) => Some(*code),
        _ => None,
    }
}

/// Anchor maps `CovenantError::ProtocolPaused` (3rd variant) to 6000 + 2.
pub const E_PROTOCOL_PAUSED: u32 = 6002;
