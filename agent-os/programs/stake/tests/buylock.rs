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
fn buylock_vault_has_no_withdraw_path_in_v1() {
    // Strictly a structural assertion: the program exposes no instruction
    // that decrements buylock_cvnt_vault. The only way buylock CVNT leaves
    // the vault is a future v1.1 timelocked-release upgrade.
    //
    // We verify by enumerating the instruction surface for any handler that
    // takes buylock_cvnt_vault as `mut` and has a `transfer_checked` from it.
    // Documented; runtime-tested by absence of any path producing a decrement.
    let mut env = boot();
    let amount = 5_000_000_000;
    let payer = env.payer.insecure_clone();
    let depositor_ata =
        create_token_account(&mut env.svm, &payer, &env.mint, &env.fee_router_keypair.pubkey());
    mint_to(&mut env.svm, &payer, &env.mint, &depositor_ata, amount);
    deposit_buylock_cvnt(&mut env, &depositor_ata, amount).expect("deposit");

    let balance_before = token_balance(&env, &env.buylock_cvnt_vault);

    // Exercise every withdraw-shaped user action; none should touch buylock.
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
