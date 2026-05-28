mod common;

use common::*;
use covenant_stake_program::RotateFeeRouterArgs;

#[test]
fn deposit_sol_fees_rejects_non_fee_router_signer() {
    let mut env = boot();
    let stranger = Keypair::new();
    env.svm.airdrop(&stranger.pubkey(), 5_000_000_000).unwrap();

    let data = anchor_lang::InstructionData::data(
        &covenant_stake_program::instruction::DepositSolFees { amount: 100_000_000 },
    );
    let metas = vec![
        solana_sdk::instruction::AccountMeta::new(env.config, false),
        solana_sdk::instruction::AccountMeta::new(env.fee_router, false),
        solana_sdk::instruction::AccountMeta::new(env.reward_vault, false),
        solana_sdk::instruction::AccountMeta::new(stranger.pubkey(), true),
        solana_sdk::instruction::AccountMeta::new_readonly(solana_sdk::system_program::ID, false),
    ];
    let payer = env.payer.insecure_clone();
    let err = send(
        &mut env.svm,
        &payer,
        &[solana_sdk::instruction::Instruction {
            program_id: covenant_stake_program::ID,
            accounts: metas,
            data,
        }],
        &[&stranger],
    )
    .expect_err("stranger cannot deposit");
    assert_eq!(custom_error(&err), Some(E_UNAUTHORIZED_FEE_ROUTER));
}

#[test]
fn deposit_sol_fees_rejects_above_max_cap() {
    let mut env = boot();
    // Need at least one locker so internal_accrue exercises the math path.
    let (owner, ata) = funded_owner(&mut env, MIN_LOCK_AMOUNT);
    create_position(&mut env, &owner, &ata, 1, MIN_LOCK_AMOUNT, TIER_30D_BPS)
        .expect("create");

    let err = deposit_sol_fees(&mut env, MAX_DEPOSIT_LAMPORTS + 1)
        .expect_err("exceeds cap");
    assert_eq!(custom_error(&err), Some(E_DEPOSIT_AMOUNT_EXCEEDED));
}

#[test]
fn deposit_sol_fees_rate_limited() {
    let mut env = boot();
    let (owner, ata) = funded_owner(&mut env, MIN_LOCK_AMOUNT);
    create_position(&mut env, &owner, &ata, 1, MIN_LOCK_AMOUNT, TIER_30D_BPS)
        .expect("create");

    deposit_sol_fees(&mut env, 100_000_000).expect("first deposit");
    let err = deposit_sol_fees(&mut env, 100_000_000).expect_err("rate limited");
    assert_eq!(custom_error(&err), Some(E_RATE_LIMITED));

    advance_clock(&mut env, RATE_LIMIT_SECS);
    deposit_sol_fees(&mut env, 100_000_000).expect("after rate limit clears");
}

#[test]
fn rotate_fee_router_changes_authorized_signer() {
    let mut env = boot();
    let (owner, ata) = funded_owner(&mut env, MIN_LOCK_AMOUNT);
    create_position(&mut env, &owner, &ata, 1, MIN_LOCK_AMOUNT, TIER_30D_BPS)
        .expect("create");

    let new_router = Keypair::new();
    env.svm.airdrop(&new_router.pubkey(), 5_000_000_000).unwrap();

    let authority = env.payer.insecure_clone();
    rotate_fee_router(
        &mut env,
        &authority,
        RotateFeeRouterArgs {
            new_authority: Some(new_router.pubkey()),
            new_max_deposit_lamports: None,
            new_rate_limit_secs: None,
        },
    )
    .expect("rotate");

    // Old router rejected
    let err = deposit_sol_fees(&mut env, 100_000_000).expect_err("old router rejected");
    assert_eq!(custom_error(&err), Some(E_UNAUTHORIZED_FEE_ROUTER));

    // New router accepted
    env.fee_router_keypair = new_router;
    deposit_sol_fees(&mut env, 100_000_000).expect("new router accepted");
}

#[test]
fn rotate_fee_router_rejects_non_authority() {
    let mut env = boot();
    let stranger = Keypair::new();
    env.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    let err = rotate_fee_router(
        &mut env,
        &stranger,
        RotateFeeRouterArgs {
            new_authority: Some(Keypair::new().pubkey()),
            new_max_deposit_lamports: None,
            new_rate_limit_secs: None,
        },
    )
    .expect_err("stranger");
    assert_eq!(custom_error(&err), Some(E_UNAUTHORIZED_AUTHORITY));
}
