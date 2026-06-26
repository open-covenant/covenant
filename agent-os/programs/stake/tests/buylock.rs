mod common;

use common::*;

#[test]
fn deposit_buylock_cvnt_routes_to_vault_and_counts() {
    let mut env = boot();
    let amount = 10_000_000_000; // 10k CVNT
    let payer = env.payer.insecure_clone();
    let depositor_ata =
        create_token_account(&mut env.svm, &payer, &env.mint, &env.fee_router_keypair.pubkey());
    mint_to(&mut env.svm, &payer, &env.mint, &depositor_ata, amount);

    deposit_buylock_cvnt(&mut env, &depositor_ata, amount).expect("deposit");

    assert_eq!(token_balance(&env, &env.buylock_cvnt_vault), amount);
    assert_eq!(token_balance(&env, &depositor_ata), 0);
    assert_eq!(config_state(&env).cumulative_buylock_cvnt, amount);
}

#[test]
fn deposit_buylock_cvnt_rejects_non_fee_router_signer() {
    let mut env = boot();
    let stranger = Keypair::new();
    env.svm.airdrop(&stranger.pubkey(), 5_000_000_000).unwrap();
    let amount = 1_000_000_000;
    let payer = env.payer.insecure_clone();
    let stranger_ata = create_token_account(&mut env.svm, &payer, &env.mint, &stranger.pubkey());
    mint_to(&mut env.svm, &payer, &env.mint, &stranger_ata, amount);

    let data = anchor_lang::InstructionData::data(
        &covenant_stake_program::instruction::DepositBuylockCvnt { amount },
    );
    let metas = vec![
        solana_sdk::instruction::AccountMeta::new(env.config, false),
        solana_sdk::instruction::AccountMeta::new_readonly(env.fee_router, false),
        solana_sdk::instruction::AccountMeta::new_readonly(env.mint, false),
        solana_sdk::instruction::AccountMeta::new_readonly(env.buylock_vault_authority, false),
        solana_sdk::instruction::AccountMeta::new(env.buylock_cvnt_vault, false),
        solana_sdk::instruction::AccountMeta::new(stranger_ata, false),
        solana_sdk::instruction::AccountMeta::new(stranger.pubkey(), true),
        solana_sdk::instruction::AccountMeta::new_readonly(spl_token::ID, false),
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
    .expect_err("stranger");
    assert_eq!(custom_error(&err), Some(E_UNAUTHORIZED_FEE_ROUTER));
}

#[test]
fn buylock_vault_untouched_by_user_actions() {
    // User-facing actions (stake/claim/close) must never decrement the buylock
    // vault. Only the authority-gated `withdraw_buylock` (sunset) can move it.
    let mut env = boot();
    let amount = 5_000_000_000;
    let payer = env.payer.insecure_clone();
    let depositor_ata =
        create_token_account(&mut env.svm, &payer, &env.mint, &env.fee_router_keypair.pubkey());
    mint_to(&mut env.svm, &payer, &env.mint, &depositor_ata, amount);
    deposit_buylock_cvnt(&mut env, &depositor_ata, amount).expect("deposit");

    let balance_before = token_balance(&env, &env.buylock_cvnt_vault);

    let (owner, owner_ata) = funded_owner(&mut env, MIN_LOCK_AMOUNT);
    create_position(&mut env, &owner, &owner_ata, 1, MIN_LOCK_AMOUNT, TIER_30D_BPS)
        .expect("create");
    advance_clock(&mut env, RATE_LIMIT_SECS);
    deposit_sol_fees(&mut env, 100_000_000).expect("deposit fees");
    claim(&mut env, &owner, 1).expect("claim");
    advance_clock(&mut env, TIER_30D_SECS + 1);
    close_position(&mut env, &owner, &owner_ata, 1).expect("close");

    assert_eq!(token_balance(&env, &env.buylock_cvnt_vault), balance_before);
}

fn withdraw_buylock_ix(
    env: &Env,
    dest_ata: solana_sdk::pubkey::Pubkey,
    authority: solana_sdk::pubkey::Pubkey,
    amount: u64,
) -> solana_sdk::instruction::Instruction {
    let data = anchor_lang::InstructionData::data(
        &covenant_stake_program::instruction::WithdrawBuylock { amount },
    );
    let metas = vec![
        solana_sdk::instruction::AccountMeta::new_readonly(env.config, false),
        solana_sdk::instruction::AccountMeta::new_readonly(env.mint, false),
        solana_sdk::instruction::AccountMeta::new_readonly(env.buylock_vault_authority, false),
        solana_sdk::instruction::AccountMeta::new(env.buylock_cvnt_vault, false),
        solana_sdk::instruction::AccountMeta::new(dest_ata, false),
        solana_sdk::instruction::AccountMeta::new_readonly(authority, true),
        solana_sdk::instruction::AccountMeta::new_readonly(spl_token::ID, false),
    ];
    solana_sdk::instruction::Instruction {
        program_id: covenant_stake_program::ID,
        accounts: metas,
        data,
    }
}

#[test]
fn withdraw_buylock_moves_funds_for_authority() {
    let mut env = boot();
    let amount = 7_000_000_000;
    let payer = env.payer.insecure_clone();
    let depositor_ata =
        create_token_account(&mut env.svm, &payer, &env.mint, &env.fee_router_keypair.pubkey());
    mint_to(&mut env.svm, &payer, &env.mint, &depositor_ata, amount);
    deposit_buylock_cvnt(&mut env, &depositor_ata, amount).expect("deposit");

    let recipient = Keypair::new();
    let dest_ata = create_token_account(&mut env.svm, &payer, &env.mint, &recipient.pubkey());

    // env.payer is the config authority (it initialized the program).
    let ix = withdraw_buylock_ix(&env, dest_ata, payer.pubkey(), amount);
    send(&mut env.svm, &payer, &[ix], &[]).expect("withdraw_buylock");

    assert_eq!(token_balance(&env, &env.buylock_cvnt_vault), 0);
    assert_eq!(token_balance(&env, &dest_ata), amount);
}

#[test]
fn withdraw_buylock_rejects_non_authority() {
    let mut env = boot();
    let amount = 3_000_000_000;
    let payer = env.payer.insecure_clone();
    let depositor_ata =
        create_token_account(&mut env.svm, &payer, &env.mint, &env.fee_router_keypair.pubkey());
    mint_to(&mut env.svm, &payer, &env.mint, &depositor_ata, amount);
    deposit_buylock_cvnt(&mut env, &depositor_ata, amount).expect("deposit");

    let stranger = Keypair::new();
    env.svm.airdrop(&stranger.pubkey(), 5_000_000_000).unwrap();
    let dest_ata = create_token_account(&mut env.svm, &payer, &env.mint, &stranger.pubkey());

    let ix = withdraw_buylock_ix(&env, dest_ata, stranger.pubkey(), amount);
    let err = send(&mut env.svm, &payer, &[ix], &[&stranger]).expect_err("non-authority rejected");
    assert_eq!(custom_error(&err), Some(E_UNAUTHORIZED_AUTHORITY));
    assert_eq!(token_balance(&env, &env.buylock_cvnt_vault), amount);
}
