//! Shared litesvm harness for the on-chain settlement program.
//!
//! Tests load the compiled `.so` into an in-process SVM and drive the real
//! instruction handlers (no mocks), so guard logic is exercised against actual
//! Anchor account validation and SPL token CPIs.

use anchor_lang::{AccountDeserialize, AccountSerialize, InstructionData};
use litesvm::LiteSVM;
use solana_sdk::{
    account::{Account, ReadableAccount},
    instruction::{AccountMeta, Instruction},
    program_pack::Pack,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_instruction, system_program,
    transaction::{Transaction, TransactionError},
};

use covenant_settlement_program::{
    instruction as ix, AnchorReceiptBatchArgs, Config, CreateTaskArgs, CreditAccount,
    InitializeArgs, RegisterAgentArgs, StakePosition, Task, ID,
};
use solana_sdk::clock::Clock;

const SO_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../target/deploy/covenant_settlement_program.so"
);

pub const DECIMALS: u8 = 0;

pub struct Env {
    pub svm: LiteSVM,
    pub payer: Keypair,
    pub slash_authority: Keypair,
    pub mint: Pubkey,
    pub config: Pubkey,
    pub treasury: Pubkey,
}

pub fn config_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"config"], &ID).0
}

pub fn credits_pda(owner: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"credits", owner.as_ref()], &ID).0
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
        args: InitializeArgs {
            slash_authority: slash_authority.pubkey(),
            credits_per_covnt: 10,
            min_stake_lock: 0,
        },
    }
    .data();
    let metas = vec![
        AccountMeta::new(config, false),
        AccountMeta::new(payer.pubkey(), true),
        AccountMeta::new_readonly(mint, false),
        AccountMeta::new_readonly(treasury, false),
        AccountMeta::new_readonly(system_program::ID, false),
    ];
    send(
        &mut svm,
        &payer,
        &[Instruction {
            program_id: ID,
            accounts: metas,
            data,
        }],
        &[],
    )
    .expect("initialize");

    Env {
        svm,
        payer,
        slash_authority,
        mint,
        config,
        treasury,
    }
}

/// Open the canonical credit account PDA for the payer.
pub fn open_credit_account(env: &mut Env) -> Pubkey {
    let owner = env.payer.pubkey();
    let credits = credits_pda(&owner);
    let data = ix::OpenCreditAccount {}.data();
    let metas = vec![
        AccountMeta::new(credits, false),
        AccountMeta::new(owner, true),
        AccountMeta::new_readonly(system_program::ID, false),
    ];
    let payer = env.payer.insecure_clone();
    send(
        &mut env.svm,
        &payer,
        &[Instruction {
            program_id: ID,
            accounts: metas,
            data,
        }],
        &[],
    )
    .expect("open_credit_account");
    credits
}

/// Fund a fresh owner COVNT account with `amount` and return it.
pub fn funded_covnt(env: &mut Env, amount: u64) -> Pubkey {
    let owner = env.payer.pubkey();
    let acct = create_token_account(&mut env.svm, &env.payer.insecure_clone(), &env.mint, &owner);
    mint_to(
        &mut env.svm,
        &env.payer.insecure_clone(),
        &env.mint,
        &acct,
        amount,
    );
    acct
}

pub fn buy_credits(
    env: &mut Env,
    credits: &Pubkey,
    owner_covnt: &Pubkey,
    amount_covnt: u64,
) -> Result<(), TransactionError> {
    let data = ix::BuyCredits { amount_covnt }.data();
    let metas = vec![
        AccountMeta::new_readonly(env.config, false),
        AccountMeta::new(*credits, false),
        AccountMeta::new(env.payer.pubkey(), true),
        AccountMeta::new(*owner_covnt, false),
        AccountMeta::new(env.treasury, false),
        AccountMeta::new_readonly(spl_token::ID, false),
    ];
    let payer = env.payer.insecure_clone();
    send(
        &mut env.svm,
        &payer,
        &[Instruction {
            program_id: ID,
            accounts: metas,
            data,
        }],
        &[],
    )
}

/// Inject a program-owned, CreditAccount-shaped account at a non-canonical
/// address (not the `[b"credits", owner]` PDA) to model a phantom-account
/// substitution attempt.
pub fn plant_phantom_credit_account(env: &mut Env, owner: &Pubkey) -> Pubkey {
    let addr = Keypair::new().pubkey();
    let mut data = Vec::new();
    CreditAccount {
        owner: *owner,
        balance: 0,
        bump: 255,
    }
    .try_serialize(&mut data)
    .unwrap();
    let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
    env.svm
        .set_account(
            addr,
            Account {
                lamports,
                data,
                owner: ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
    addr
}

pub fn credit_balance(env: &Env, credits: &Pubkey) -> u64 {
    let acc = env.svm.get_account(credits).expect("credit account exists");
    let mut data = acc.data();
    CreditAccount::try_deserialize(&mut data)
        .expect("credit account")
        .balance
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
    send(
        &mut env.svm,
        &payer,
        &[Instruction {
            program_id: ID,
            accounts: metas,
            data,
        }],
        &[],
    )
    .expect("register_agent");
    agent
}

/// Open a stake position funded with `amount`. Returns (position PDA, stake vault).
pub fn stake(env: &mut Env, agent_key: &[u8; 32], amount: u64) -> (Pubkey, Pubkey) {
    let agent = agent_pda(agent_key);
    let owner = env.payer.pubkey();
    let position = stake_pda(agent_key, &owner);

    let owner_covnt =
        create_token_account(&mut env.svm, &env.payer.insecure_clone(), &env.mint, &owner);
    mint_to(
        &mut env.svm,
        &env.payer.insecure_clone(),
        &env.mint,
        &owner_covnt,
        amount,
    );
    let stake_vault = create_token_account(
        &mut env.svm,
        &env.payer.insecure_clone(),
        &env.mint,
        &position,
    );

    let data = ix::Stake {
        amount,
        lock_until: 0,
    }
    .data();
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
    send(
        &mut env.svm,
        &payer,
        &[Instruction {
            program_id: ID,
            accounts: metas,
            data,
        }],
        &[],
    )
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
    send(
        &mut env.svm,
        &payer,
        &[Instruction {
            program_id: ID,
            accounts: metas,
            data,
        }],
        &[],
    )
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
    let data = ix::SlashStake {
        amount,
        reason_hash: [0u8; 32],
    }
    .data();
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
    send(
        &mut env.svm,
        &payer,
        &[Instruction {
            program_id: ID,
            accounts: metas,
            data,
        }],
        &[&slasher],
    )
}

pub fn token_balance(env: &Env, account: &Pubkey) -> u64 {
    let acc = env.svm.get_account(account).expect("token account exists");
    spl_token::state::Account::unpack(acc.data())
        .expect("token account")
        .amount
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
    let ix =
        spl_token::instruction::mint_to(&spl_token::ID, mint, dest, &payer.pubkey(), &[], amount)
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

/// Anchor's built-in seeds-constraint violation.
pub const E_CONSTRAINT_SEEDS: u32 = 2006;

// Custom error codes (Anchor offsets `CovenantError` from 6000 in declaration order).
pub const E_ZERO_AMOUNT: u32 = 6000;
pub const E_UNAUTHORIZED: u32 = 6003;
pub const E_AGENT_INACTIVE: u32 = 6005;
pub const E_INSUFFICIENT_CREDITS: u32 = 6007;
pub const E_INSUFFICIENT_STAKE: u32 = 6008;
pub const E_STAKE_INACTIVE: u32 = 6009;
pub const E_WRONG_TASK_STATUS: u32 = 6010;
pub const E_TASK_EXPIRED: u32 = 6011;
pub const E_TASK_NOT_EXPIRED: u32 = 6012;
pub const E_STAKE_LOCKED: u32 = 6013;
pub const E_STAKE_STILL_ACTIVE: u32 = 6014;
pub const E_LOCK_TOO_SHORT: u32 = 6015;
pub const E_TASKS_DISABLED: u32 = 6016;

pub fn task_pda(task_id: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(&[b"task", task_id], &ID).0
}

pub fn receipt_batch_pda(batch_id: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(&[b"receipt_batch", batch_id], &ID).0
}

/// Force a fresh blockhash so an otherwise-identical follow-up transaction
/// gets a distinct signature instead of being rejected as already-processed.
pub fn bump_blockhash(env: &mut Env) {
    env.svm.expire_blockhash();
}

/// Set the SVM clock's unix timestamp (litesvm starts at 0), so lock_until
/// and task-deadline branches can be exercised deterministically.
pub fn warp_unix(env: &mut Env, unix: i64) {
    let mut clock: Clock = env.svm.get_sysvar();
    clock.unix_timestamp = unix;
    env.svm.set_sysvar(&clock);
}

/// Stake `amount` with an explicit `lock_until`; returns
/// (position, stake_vault, owner_covnt) so the caller can drive unstake.
pub fn stake_locked(
    env: &mut Env,
    agent_key: &[u8; 32],
    amount: u64,
    lock_until: u64,
) -> (Pubkey, Pubkey, Pubkey) {
    let agent = agent_pda(agent_key);
    let owner = env.payer.pubkey();
    let position = stake_pda(agent_key, &owner);
    let owner_covnt =
        create_token_account(&mut env.svm, &env.payer.insecure_clone(), &env.mint, &owner);
    mint_to(
        &mut env.svm,
        &env.payer.insecure_clone(),
        &env.mint,
        &owner_covnt,
        amount,
    );
    let stake_vault =
        create_token_account(&mut env.svm, &env.payer.insecure_clone(), &env.mint, &position);

    let data = ix::Stake { amount, lock_until }.data();
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
    send(
        &mut env.svm,
        &payer,
        &[Instruction { program_id: ID, accounts: metas, data }],
        &[],
    )
    .expect("stake_locked");
    (position, stake_vault, owner_covnt)
}

/// Fallible stake: provisions a funded owner account + vault, then attempts
/// `stake`. Returns the raw result so failure-mode tests can assert the error.
pub fn try_stake(
    env: &mut Env,
    agent_key: &[u8; 32],
    amount: u64,
    lock_until: u64,
) -> Result<(), TransactionError> {
    let agent = agent_pda(agent_key);
    let owner = env.payer.pubkey();
    let position = stake_pda(agent_key, &owner);
    let owner_covnt =
        create_token_account(&mut env.svm, &env.payer.insecure_clone(), &env.mint, &owner);
    mint_to(&mut env.svm, &env.payer.insecure_clone(), &env.mint, &owner_covnt, amount.max(1));
    let stake_vault =
        create_token_account(&mut env.svm, &env.payer.insecure_clone(), &env.mint, &position);
    let data = ix::Stake { amount, lock_until }.data();
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
}

pub fn unstake(
    env: &mut Env,
    agent_key: &[u8; 32],
    position: &Pubkey,
    stake_vault: &Pubkey,
    owner_covnt: &Pubkey,
) -> Result<(), TransactionError> {
    let agent = agent_pda(agent_key);
    let owner = env.payer.pubkey();
    let data = ix::Unstake {}.data();
    let metas = vec![
        AccountMeta::new_readonly(env.config, false),
        AccountMeta::new(agent, false),
        AccountMeta::new(*position, false),
        AccountMeta::new(owner, true),
        AccountMeta::new(*stake_vault, false),
        AccountMeta::new(*owner_covnt, false),
        AccountMeta::new_readonly(spl_token::ID, false),
    ];
    let payer = env.payer.insecure_clone();
    send(&mut env.svm, &payer, &[Instruction { program_id: ID, accounts: metas, data }], &[])
}

pub fn close_position(env: &mut Env, position: &Pubkey) -> Result<(), TransactionError> {
    let owner = env.payer.pubkey();
    let data = ix::ClosePosition {}.data();
    let metas = vec![
        AccountMeta::new(*position, false),
        AccountMeta::new(owner, true),
    ];
    let payer = env.payer.insecure_clone();
    send(&mut env.svm, &payer, &[Instruction { program_id: ID, accounts: metas, data }], &[])
}

pub fn consume_credits(
    env: &mut Env,
    credits: &Pubkey,
    amount: u64,
) -> Result<(), TransactionError> {
    let owner = env.payer.pubkey();
    let data = ix::ConsumeCredits { amount, receipt_hash: [9u8; 32] }.data();
    let metas = vec![
        AccountMeta::new_readonly(env.config, false),
        AccountMeta::new(*credits, false),
        AccountMeta::new_readonly(owner, true),
    ];
    let payer = env.payer.insecure_clone();
    send(&mut env.svm, &payer, &[Instruction { program_id: ID, accounts: metas, data }], &[])
}

pub fn set_agent_active(
    env: &mut Env,
    agent_key: &[u8; 32],
    active: bool,
) -> Result<(), TransactionError> {
    let agent = agent_pda(agent_key);
    let authority = env.payer.pubkey();
    let data = ix::SetAgentActive { active }.data();
    let metas = vec![
        AccountMeta::new_readonly(env.config, false),
        AccountMeta::new_readonly(authority, true),
        AccountMeta::new(agent, false),
    ];
    let payer = env.payer.insecure_clone();
    send(&mut env.svm, &payer, &[Instruction { program_id: ID, accounts: metas, data }], &[])
}

pub fn burn_covnt(
    env: &mut Env,
    owner_covnt: &Pubkey,
    amount: u64,
) -> Result<(), TransactionError> {
    let owner = env.payer.pubkey();
    let data = ix::BurnCovnt { amount, reason_hash: [3u8; 32] }.data();
    let metas = vec![
        AccountMeta::new_readonly(env.config, false),
        AccountMeta::new(owner, true),
        AccountMeta::new(env.mint, false),
        AccountMeta::new(*owner_covnt, false),
        AccountMeta::new_readonly(spl_token::ID, false),
    ];
    let payer = env.payer.insecure_clone();
    send(&mut env.svm, &payer, &[Instruction { program_id: ID, accounts: metas, data }], &[])
}

pub fn anchor_receipt_batch(
    env: &mut Env,
    batch_id: &[u8; 32],
    receipt_count: u32,
) -> Result<(), TransactionError> {
    let batch = receipt_batch_pda(batch_id);
    let authority = env.payer.pubkey();
    let data = ix::AnchorReceiptBatch {
        args: AnchorReceiptBatchArgs {
            batch_id: *batch_id,
            merkle_root: [1u8; 32],
            receipt_count,
        },
    }
    .data();
    let metas = vec![
        AccountMeta::new_readonly(env.config, false),
        AccountMeta::new(batch, false),
        AccountMeta::new(authority, true),
        AccountMeta::new_readonly(system_program::ID, false),
    ];
    let payer = env.payer.insecure_clone();
    send(&mut env.svm, &payer, &[Instruction { program_id: ID, accounts: metas, data }], &[])
}

pub struct TaskCtx {
    pub task: Pubkey,
    pub escrow_vault: Pubkey,
    pub client_covnt: Pubkey,
}

/// Create the escrow vault (owned by the task PDA) and a funded client
/// COVNT account for a task with `task_id`.
pub fn task_setup(env: &mut Env, task_id: &[u8; 32], fund_client: u64) -> TaskCtx {
    let task = task_pda(task_id);
    let client = env.payer.pubkey();
    let escrow_vault =
        create_token_account(&mut env.svm, &env.payer.insecure_clone(), &env.mint, &task);
    let client_covnt =
        create_token_account(&mut env.svm, &env.payer.insecure_clone(), &env.mint, &client);
    mint_to(
        &mut env.svm,
        &env.payer.insecure_clone(),
        &env.mint,
        &client_covnt,
        fund_client,
    );
    TaskCtx { task, escrow_vault, client_covnt }
}

#[allow(clippy::too_many_arguments)]
pub fn create_task(
    env: &mut Env,
    agent_key: &[u8; 32],
    task_id: &[u8; 32],
    provider: &Pubkey,
    amount_covnt: u64,
    deadline: i64,
    tc: &TaskCtx,
) -> Result<(), TransactionError> {
    let agent = agent_pda(agent_key);
    let client = env.payer.pubkey();
    let data = ix::CreateTask {
        args: CreateTaskArgs {
            task_id: *task_id,
            provider: *provider,
            amount_covnt,
            task_hash: [4u8; 32],
            criteria_hash: [5u8; 32],
            deadline,
        },
    }
    .data();
    let metas = vec![
        AccountMeta::new_readonly(env.config, false),
        AccountMeta::new_readonly(agent, false),
        AccountMeta::new(tc.task, false),
        AccountMeta::new(client, true),
        AccountMeta::new(tc.client_covnt, false),
        AccountMeta::new(tc.escrow_vault, false),
        AccountMeta::new_readonly(spl_token::ID, false),
        AccountMeta::new_readonly(system_program::ID, false),
    ];
    let payer = env.payer.insecure_clone();
    send(&mut env.svm, &payer, &[Instruction { program_id: ID, accounts: metas, data }], &[])
}

pub fn release_task(
    env: &mut Env,
    tc: &TaskCtx,
    provider_covnt: &Pubkey,
) -> Result<(), TransactionError> {
    let client = env.payer.pubkey();
    let data = ix::ReleaseTask { result_hash: [6u8; 32], receipt_hash: [7u8; 32] }.data();
    let metas = vec![
        AccountMeta::new_readonly(env.config, false),
        AccountMeta::new(tc.task, false),
        AccountMeta::new_readonly(client, true),
        AccountMeta::new(tc.escrow_vault, false),
        AccountMeta::new(*provider_covnt, false),
        AccountMeta::new_readonly(spl_token::ID, false),
    ];
    let payer = env.payer.insecure_clone();
    send(&mut env.svm, &payer, &[Instruction { program_id: ID, accounts: metas, data }], &[])
}

pub fn refund_task(env: &mut Env, tc: &TaskCtx) -> Result<(), TransactionError> {
    let client = env.payer.pubkey();
    let data = ix::RefundTask {}.data();
    let metas = vec![
        AccountMeta::new_readonly(env.config, false),
        AccountMeta::new(tc.task, false),
        AccountMeta::new_readonly(client, true),
        AccountMeta::new(tc.escrow_vault, false),
        AccountMeta::new(tc.client_covnt, false),
        AccountMeta::new_readonly(spl_token::ID, false),
    ];
    let payer = env.payer.insecure_clone();
    send(&mut env.svm, &payer, &[Instruction { program_id: ID, accounts: metas, data }], &[])
}

pub fn update_authority(env: &mut Env, new_authority: &Pubkey) -> Result<(), TransactionError> {
    let authority = env.payer.pubkey();
    let data = ix::UpdateAuthority { new_authority: *new_authority }.data();
    let metas = vec![
        AccountMeta::new(env.config, false),
        AccountMeta::new_readonly(authority, true),
    ];
    let payer = env.payer.insecure_clone();
    send(&mut env.svm, &payer, &[Instruction { program_id: ID, accounts: metas, data }], &[])
}

pub fn update_slash_authority(env: &mut Env, new: &Pubkey) -> Result<(), TransactionError> {
    let authority = env.payer.pubkey();
    let data = ix::UpdateSlashAuthority { new_slash_authority: *new }.data();
    let metas = vec![
        AccountMeta::new(env.config, false),
        AccountMeta::new_readonly(authority, true),
    ];
    let payer = env.payer.insecure_clone();
    send(&mut env.svm, &payer, &[Instruction { program_id: ID, accounts: metas, data }], &[])
}

pub fn update_treasury(env: &mut Env, new_treasury: &Pubkey) -> Result<(), TransactionError> {
    let authority = env.payer.pubkey();
    let data = ix::UpdateTreasury {}.data();
    let metas = vec![
        AccountMeta::new(env.config, false),
        AccountMeta::new_readonly(authority, true),
        AccountMeta::new_readonly(*new_treasury, false),
    ];
    let payer = env.payer.insecure_clone();
    send(&mut env.svm, &payer, &[Instruction { program_id: ID, accounts: metas, data }], &[])
}

pub fn config_account(env: &Env) -> Config {
    let acc = env.svm.get_account(&env.config).expect("config exists");
    let mut data = acc.data();
    Config::try_deserialize(&mut data).expect("config")
}

pub fn set_min_stake_lock(env: &mut Env, value: u64) -> Result<(), TransactionError> {
    let authority = env.payer.pubkey();
    let data = ix::SetMinStakeLock { min_stake_lock: value }.data();
    let metas = vec![
        AccountMeta::new(env.config, false),
        AccountMeta::new_readonly(authority, true),
    ];
    let payer = env.payer.insecure_clone();
    send(&mut env.svm, &payer, &[Instruction { program_id: ID, accounts: metas, data }], &[])
}

pub fn migrate_config(env: &mut Env, value: u64) -> Result<(), TransactionError> {
    let authority = env.payer.pubkey();
    let data = ix::MigrateConfig { min_stake_lock: value }.data();
    let metas = vec![
        AccountMeta::new(env.config, false),
        AccountMeta::new(authority, true),
        AccountMeta::new_readonly(system_program::ID, false),
    ];
    let payer = env.payer.insecure_clone();
    send(&mut env.svm, &payer, &[Instruction { program_id: ID, accounts: metas, data }], &[])
}

/// Overwrite the config PDA with the legacy 146-byte layout (no min_stake_lock)
/// to model a pre-migration account. Built by serializing the current Config
/// and dropping the trailing 8 bytes.
pub fn plant_legacy_config(env: &mut Env) {
    let (config, bump) = Pubkey::find_program_address(&[b"config"], &ID);
    let cfg = Config {
        authority: env.payer.pubkey(),
        slash_authority: env.slash_authority.pubkey(),
        covnt_mint: env.mint,
        treasury: env.treasury,
        credits_per_covnt: 10,
        paused: false,
        bump,
        min_stake_lock: 0,
    };
    let mut full = Vec::new();
    cfg.try_serialize(&mut full).unwrap();
    let legacy = full[..full.len() - 8].to_vec();
    let lamports = env.svm.minimum_balance_for_rent_exemption(legacy.len());
    env.svm
        .set_account(
            config,
            Account {
                lamports,
                data: legacy,
                owner: ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
}

pub fn set_credits_per_covnt(env: &mut Env, value: u64) -> Result<(), TransactionError> {
    let authority = env.payer.pubkey();
    let data = ix::SetCreditsPerCovnt { credits_per_covnt: value }.data();
    let metas = vec![
        AccountMeta::new(env.config, false),
        AccountMeta::new_readonly(authority, true),
    ];
    let payer = env.payer.insecure_clone();
    send(&mut env.svm, &payer, &[Instruction { program_id: ID, accounts: metas, data }], &[])
}

pub fn agent_stake(env: &Env, agent_key: &[u8; 32]) -> u64 {
    let acc = env.svm.get_account(&agent_pda(agent_key)).expect("agent exists");
    let mut data = acc.data();
    use anchor_lang::AccountDeserialize;
    covenant_settlement_program::Agent::try_deserialize(&mut data)
        .expect("agent")
        .stake
}

pub fn position_exists(env: &Env, position: &Pubkey) -> bool {
    env.svm.get_account(position).map(|a| !a.data.is_empty()).unwrap_or(false)
}

pub fn task_status(env: &Env, task: &Pubkey) -> u8 {
    let acc = env.svm.get_account(task).expect("task exists");
    let mut data = acc.data();
    Task::try_deserialize(&mut data).expect("task").status
}

pub fn stake_position(env: &Env, position: &Pubkey) -> StakePosition {
    let acc = env.svm.get_account(position).expect("position exists");
    let mut data = acc.data();
    StakePosition::try_deserialize(&mut data).expect("position")
}
