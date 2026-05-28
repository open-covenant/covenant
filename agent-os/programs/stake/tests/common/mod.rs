//! Shared litesvm harness for the covenant-stake program.
//!
//! Tests load the compiled `.so` into an in-process SVM and drive the real
//! Anchor instructions against a fresh `$CVNT` mint and PDA-owned vaults.

use anchor_lang::{AccountDeserialize, InstructionData};
use litesvm::LiteSVM;
pub use solana_sdk::signature::Keypair;
pub use solana_sdk::signer::Signer;
use solana_sdk::{
    account::ReadableAccount,
    instruction::{AccountMeta, Instruction},
    program_pack::Pack,
    pubkey::Pubkey,
    system_instruction, system_program,
    transaction::{Transaction, TransactionError},
};

use covenant_stake_program::{
    instruction as ix, Config, CreatePositionArgs, FeeRouter, InitializeArgs, RotateFeeRouterArgs,
    StakePosition, ID,
};

const SO_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../target/deploy/covenant_stake_program.so"
);

pub const DECIMALS: u8 = 6;
pub const MIN_LOCK_AMOUNT: u64 = 1_000_000_000;
pub const MAX_DEPOSIT_LAMPORTS: u64 = 5_000_000_000;
pub const RATE_LIMIT_SECS: i64 = 60;

pub struct Env {
    pub svm: LiteSVM,
    pub payer: Keypair,
    pub fee_router_keypair: Keypair,
    pub pause_keypair: Keypair,
    pub mint: Pubkey,
    pub config: Pubkey,
    pub fee_router: Pubkey,
    pub reward_vault: Pubkey,
    pub locked_vault_authority: Pubkey,
    pub locked_cvnt_vault: Pubkey,
    pub buylock_vault_authority: Pubkey,
    pub buylock_cvnt_vault: Pubkey,
}

pub fn config_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"stake_config"], &ID).0
}

pub fn fee_router_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"fee_router"], &ID).0
}

pub fn reward_vault_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"reward_vault"], &ID).0
}

pub fn locked_vault_auth_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"vault_auth"], &ID).0
}

pub fn buylock_vault_auth_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"buylock_auth"], &ID).0
}

pub fn position_pda(owner: &Pubkey, nonce: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[b"stake_v2", owner.as_ref(), &nonce.to_le_bytes()],
        &ID,
    )
    .0
}

pub fn boot() -> Env {
    let mut svm = LiteSVM::new();
    svm.add_program_from_file(ID, SO_PATH)
        .expect("load covenant_stake_program.so (run `cargo build-sbf` first)");

    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 1_000_000_000_000).unwrap();

    let pause_keypair = Keypair::new();
    let fee_router_keypair = Keypair::new();
    svm.airdrop(&fee_router_keypair.pubkey(), 100_000_000_000)
        .unwrap();

    let mint = create_mint(&mut svm, &payer);

    let locked_vault_authority = locked_vault_auth_pda();
    let locked_cvnt_vault = create_token_account(&mut svm, &payer, &mint, &locked_vault_authority);

    let buylock_vault_authority = buylock_vault_auth_pda();
    let buylock_cvnt_vault =
        create_token_account(&mut svm, &payer, &mint, &buylock_vault_authority);

    let config = config_pda();
    let fee_router = fee_router_pda();
    let reward_vault = reward_vault_pda();

    let data = ix::Initialize {
        args: InitializeArgs {
            pause_authority: pause_keypair.pubkey(),
            fee_router_authority: fee_router_keypair.pubkey(),
            min_lock_amount: MIN_LOCK_AMOUNT,
            fee_router_max_deposit_lamports: MAX_DEPOSIT_LAMPORTS,
            fee_router_rate_limit_secs: RATE_LIMIT_SECS,
        },
    }
    .data();

    let metas = vec![
        AccountMeta::new(config, false),
        AccountMeta::new(fee_router, false),
        AccountMeta::new_readonly(locked_vault_authority, false),
        AccountMeta::new(reward_vault, false),
        AccountMeta::new_readonly(buylock_vault_authority, false),
        AccountMeta::new_readonly(mint, false),
        AccountMeta::new_readonly(locked_cvnt_vault, false),
        AccountMeta::new_readonly(buylock_cvnt_vault, false),
        AccountMeta::new(payer.pubkey(), true),
        AccountMeta::new_readonly(spl_token::ID, false),
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
        fee_router_keypair,
        pause_keypair,
        mint,
        config,
        fee_router,
        reward_vault,
        locked_vault_authority,
        locked_cvnt_vault,
        buylock_vault_authority,
        buylock_cvnt_vault,
    }
}

pub fn create_position(
    env: &mut Env,
    owner: &Keypair,
    owner_ata: &Pubkey,
    nonce: u64,
    amount: u64,
    lock_tier_bps: u16,
) -> Result<Pubkey, TransactionError> {
    let position = position_pda(&owner.pubkey(), nonce);
    let data = ix::CreatePosition {
        args: CreatePositionArgs {
            nonce,
            amount,
            lock_tier_bps,
        },
    }
    .data();
    let metas = vec![
        AccountMeta::new(env.config, false),
        AccountMeta::new(position, false),
        AccountMeta::new_readonly(env.mint, false),
        AccountMeta::new_readonly(env.locked_vault_authority, false),
        AccountMeta::new(env.locked_cvnt_vault, false),
        AccountMeta::new(*owner_ata, false),
        AccountMeta::new(owner.pubkey(), true),
        AccountMeta::new_readonly(spl_token::ID, false),
        AccountMeta::new_readonly(system_program::ID, false),
    ];
    let payer = env.payer.insecure_clone();
    let owner_kp = owner.insecure_clone();
    send(
        &mut env.svm,
        &payer,
        &[Instruction {
            program_id: ID,
            accounts: metas,
            data,
        }],
        &[&owner_kp],
    )?;
    Ok(position)
}

pub fn increase_amount(
    env: &mut Env,
    owner: &Keypair,
    owner_ata: &Pubkey,
    nonce: u64,
    extra: u64,
) -> Result<(), TransactionError> {
    let position = position_pda(&owner.pubkey(), nonce);
    let data = ix::IncreaseAmount { extra }.data();
    let metas = vec![
        AccountMeta::new(env.config, false),
        AccountMeta::new(position, false),
        AccountMeta::new_readonly(env.mint, false),
        AccountMeta::new_readonly(env.locked_vault_authority, false),
        AccountMeta::new(env.locked_cvnt_vault, false),
        AccountMeta::new(*owner_ata, false),
        AccountMeta::new_readonly(owner.pubkey(), true),
        AccountMeta::new_readonly(spl_token::ID, false),
    ];
    let payer = env.payer.insecure_clone();
    let owner_kp = owner.insecure_clone();
    send(
        &mut env.svm,
        &payer,
        &[Instruction {
            program_id: ID,
            accounts: metas,
            data,
        }],
        &[&owner_kp],
    )
}

pub fn claim(env: &mut Env, owner: &Keypair, nonce: u64) -> Result<(), TransactionError> {
    let position = position_pda(&owner.pubkey(), nonce);
    let data = ix::Claim {}.data();
    let metas = vec![
        AccountMeta::new(env.config, false),
        AccountMeta::new(position, false),
        AccountMeta::new(env.reward_vault, false),
        AccountMeta::new(owner.pubkey(), true),
    ];
    let payer = env.payer.insecure_clone();
    let owner_kp = owner.insecure_clone();
    send(
        &mut env.svm,
        &payer,
        &[Instruction {
            program_id: ID,
            accounts: metas,
            data,
        }],
        &[&owner_kp],
    )
}

pub fn close_position(
    env: &mut Env,
    owner: &Keypair,
    owner_ata: &Pubkey,
    nonce: u64,
) -> Result<(), TransactionError> {
    let position = position_pda(&owner.pubkey(), nonce);
    let data = ix::ClosePosition {}.data();
    let metas = vec![
        AccountMeta::new(env.config, false),
        AccountMeta::new(position, false),
        AccountMeta::new_readonly(env.mint, false),
        AccountMeta::new_readonly(env.locked_vault_authority, false),
        AccountMeta::new(env.locked_cvnt_vault, false),
        AccountMeta::new(*owner_ata, false),
        AccountMeta::new(env.reward_vault, false),
        AccountMeta::new(owner.pubkey(), true),
        AccountMeta::new_readonly(spl_token::ID, false),
    ];
    let payer = env.payer.insecure_clone();
    let owner_kp = owner.insecure_clone();
    send(
        &mut env.svm,
        &payer,
        &[Instruction {
            program_id: ID,
            accounts: metas,
            data,
        }],
        &[&owner_kp],
    )
}

pub fn deposit_sol_fees(env: &mut Env, amount: u64) -> Result<(), TransactionError> {
    let data = ix::DepositSolFees { amount }.data();
    let metas = vec![
        AccountMeta::new(env.config, false),
        AccountMeta::new(env.fee_router, false),
        AccountMeta::new(env.reward_vault, false),
        AccountMeta::new(env.fee_router_keypair.pubkey(), true),
        AccountMeta::new_readonly(system_program::ID, false),
    ];
    let payer = env.payer.insecure_clone();
    let fee_signer = env.fee_router_keypair.insecure_clone();
    send(
        &mut env.svm,
        &payer,
        &[Instruction {
            program_id: ID,
            accounts: metas,
            data,
        }],
        &[&fee_signer],
    )
}

pub fn deposit_buylock_cvnt(
    env: &mut Env,
    depositor_ata: &Pubkey,
    amount: u64,
) -> Result<(), TransactionError> {
    let data = ix::DepositBuylockCvnt { amount }.data();
    let metas = vec![
        AccountMeta::new(env.config, false),
        AccountMeta::new_readonly(env.fee_router, false),
        AccountMeta::new_readonly(env.mint, false),
        AccountMeta::new_readonly(env.buylock_vault_authority, false),
        AccountMeta::new(env.buylock_cvnt_vault, false),
        AccountMeta::new(*depositor_ata, false),
        AccountMeta::new(env.fee_router_keypair.pubkey(), true),
        AccountMeta::new_readonly(spl_token::ID, false),
    ];
    let payer = env.payer.insecure_clone();
    let fee_signer = env.fee_router_keypair.insecure_clone();
    send(
        &mut env.svm,
        &payer,
        &[Instruction {
            program_id: ID,
            accounts: metas,
            data,
        }],
        &[&fee_signer],
    )
}

pub fn accrue(env: &mut Env) -> Result<(), TransactionError> {
    let data = ix::Accrue {}.data();
    let metas = vec![AccountMeta::new(env.config, false)];
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

pub fn set_pause(env: &mut Env, signer: &Keypair, paused: bool) -> Result<(), TransactionError> {
    let data = ix::SetPause { paused }.data();
    let metas = vec![
        AccountMeta::new(env.config, false),
        AccountMeta::new_readonly(signer.pubkey(), true),
    ];
    let payer = env.payer.insecure_clone();
    let signer_kp = signer.insecure_clone();
    send(
        &mut env.svm,
        &payer,
        &[Instruction {
            program_id: ID,
            accounts: metas,
            data,
        }],
        &[&signer_kp],
    )
}

pub fn rotate_fee_router(
    env: &mut Env,
    authority: &Keypair,
    args: RotateFeeRouterArgs,
) -> Result<(), TransactionError> {
    let data = ix::RotateFeeRouter { args }.data();
    let metas = vec![
        AccountMeta::new_readonly(env.config, false),
        AccountMeta::new(env.fee_router, false),
        AccountMeta::new_readonly(authority.pubkey(), true),
    ];
    let payer = env.payer.insecure_clone();
    let auth_kp = authority.insecure_clone();
    send(
        &mut env.svm,
        &payer,
        &[Instruction {
            program_id: ID,
            accounts: metas,
            data,
        }],
        &[&auth_kp],
    )
}

pub fn funded_owner(env: &mut Env, amount: u64) -> (Keypair, Pubkey) {
    let owner = Keypair::new();
    env.svm.airdrop(&owner.pubkey(), 10_000_000_000).unwrap();
    let payer = env.payer.insecure_clone();
    let ata = create_token_account(&mut env.svm, &payer, &env.mint, &owner.pubkey());
    mint_to(&mut env.svm, &env.payer.insecure_clone(), &env.mint, &ata, amount);
    (owner, ata)
}

pub fn config_state(env: &Env) -> Config {
    let acc = env.svm.get_account(&env.config).expect("config exists");
    let mut data = acc.data();
    Config::try_deserialize(&mut data).expect("config")
}

pub fn fee_router_state(env: &Env) -> FeeRouter {
    let acc = env.svm.get_account(&env.fee_router).expect("fee_router exists");
    let mut data = acc.data();
    FeeRouter::try_deserialize(&mut data).expect("fee_router")
}

pub fn position_state(env: &Env, position: &Pubkey) -> StakePosition {
    let acc = env.svm.get_account(position).expect("position exists");
    let mut data = acc.data();
    StakePosition::try_deserialize(&mut data).expect("position")
}

pub fn token_balance(env: &Env, account: &Pubkey) -> u64 {
    let acc = env.svm.get_account(account).expect("token account exists");
    spl_token::state::Account::unpack(acc.data())
        .expect("token account")
        .amount
}

pub fn sol_balance(env: &Env, account: &Pubkey) -> u64 {
    env.svm
        .get_account(account)
        .map(|a| a.lamports)
        .unwrap_or(0)
}

pub fn advance_clock(env: &mut Env, secs: i64) {
    let mut clock = env.svm.get_sysvar::<solana_sdk::clock::Clock>();
    clock.unix_timestamp += secs;
    env.svm.set_sysvar::<solana_sdk::clock::Clock>(&clock);
    env.svm.expire_blockhash();
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
    let mut signers: Vec<&Keypair> = vec![payer];
    signers.extend_from_slice(extra_signers);
    let tx = Transaction::new_signed_with_payer(
        ixs,
        Some(&payer.pubkey()),
        &signers,
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).map(|_| ()).map_err(|e| e.err)
}

pub fn custom_error(err: &TransactionError) -> Option<u32> {
    use solana_sdk::instruction::InstructionError;
    match err {
        TransactionError::InstructionError(_, InstructionError::Custom(code)) => Some(*code),
        _ => None,
    }
}

pub const E_PROTOCOL_PAUSED: u32 = 6000;
pub const E_LOCK_NOT_EXPIRED: u32 = 6001;
pub const E_LOCK_EXPIRED: u32 = 6002;
pub const E_INVALID_LOCK_TIER: u32 = 6003;
pub const E_LOCK_AMOUNT_TOO_SMALL: u32 = 6004;
pub const E_LOCK_CAP_EXCEEDED: u32 = 6005;
pub const E_UNAUTHORIZED_AUTHORITY: u32 = 6006;
pub const E_UNAUTHORIZED_FEE_ROUTER: u32 = 6007;
pub const E_RATE_LIMITED: u32 = 6008;
pub const E_DEPOSIT_AMOUNT_EXCEEDED: u32 = 6009;
pub const E_MATH_OVERFLOW: u32 = 6010;
pub const E_INVALID_MINT: u32 = 6011;
pub const E_INSUFFICIENT_REWARD_VAULT: u32 = 6012;
pub const E_NOTHING_TO_CLAIM: u32 = 6013;
pub const E_ZERO_AMOUNT: u32 = 6014;
pub const E_INVALID_PARAMETER: u32 = 6015;

pub const TIER_30D_BPS: u16 = 10_000;
pub const TIER_90D_BPS: u16 = 15_000;
pub const TIER_180D_BPS: u16 = 20_000;
pub const TIER_365D_BPS: u16 = 30_000;

pub const TIER_30D_SECS: i64 = 30 * 86_400;
pub const TIER_90D_SECS: i64 = 90 * 86_400;
pub const TIER_180D_SECS: i64 = 180 * 86_400;
pub const TIER_365D_SECS: i64 = 365 * 86_400;
