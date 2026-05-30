mod common;

use anchor_lang::InstructionData;
use common::*;
use covenant_stake_program::{instruction as ix, InitializeArgs, ID};
use solana_sdk::instruction::{AccountMeta, Instruction};

#[test]
fn b1_initialize_rejects_non_upgrade_authority() {
    // Re-derive boot manually without running initialize, then attempt to
    // initialize with a signer that is NOT the injected upgrade authority.
    use litesvm::LiteSVM;
    use solana_sdk::signature::Keypair;
    use solana_sdk::signer::Signer;

    const SO_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/deploy/covenant_stake_program.so"
    );

    let mut svm = LiteSVM::new();
    svm.add_program_from_file(ID, SO_PATH).unwrap();

    let real_upgrade_authority = Keypair::new();
    svm.airdrop(&real_upgrade_authority.pubkey(), 100_000_000_000)
        .unwrap();
    let stranger = Keypair::new();
    svm.airdrop(&stranger.pubkey(), 100_000_000_000).unwrap();

    let mint = create_mint(&mut svm, &real_upgrade_authority);
    let locked_auth = locked_vault_auth_pda();
    let locked_vault = create_token_account(&mut svm, &real_upgrade_authority, &mint, &locked_auth);
    let buylock_auth = buylock_vault_auth_pda();
    let buylock_vault =
        create_token_account(&mut svm, &real_upgrade_authority, &mint, &buylock_auth);

    let program_data = inject_program_data(&mut svm, real_upgrade_authority.pubkey());

    let pause_kp = Keypair::new();
    let fr_kp = Keypair::new();

    let data = ix::Initialize {
        args: InitializeArgs {
            pause_authority: pause_kp.pubkey(),
            fee_router_authority: fr_kp.pubkey(),
            min_lock_amount: MIN_LOCK_AMOUNT,
            fee_router_max_deposit_lamports: MAX_DEPOSIT_LAMPORTS,
            fee_router_rate_limit_secs: RATE_LIMIT_SECS,
        },
    }
    .data();

    let metas = vec![
        AccountMeta::new(config_pda(), false),
        AccountMeta::new(fee_router_pda(), false),
        AccountMeta::new_readonly(locked_auth, false),
        AccountMeta::new(reward_vault_pda(), false),
        AccountMeta::new_readonly(buylock_auth, false),
        AccountMeta::new_readonly(mint, false),
        AccountMeta::new_readonly(locked_vault, false),
        AccountMeta::new_readonly(buylock_vault, false),
        AccountMeta::new(stranger.pubkey(), true),
        AccountMeta::new_readonly(program_data, false),
        AccountMeta::new_readonly(spl_token::ID, false),
        AccountMeta::new_readonly(solana_sdk::system_program::ID, false),
    ];

    let err = send(
        &mut svm,
        &stranger,
        &[Instruction {
            program_id: ID,
            accounts: metas,
            data,
        }],
        &[],
    )
    .expect_err("stranger initialize must be rejected");
    assert_eq!(custom_error(&err), Some(E_UNAUTHORIZED_AUTHORITY));
}

#[test]
fn update_authority_rotates_and_old_loses_access() {
    let mut env = boot();
    let new_auth = Keypair::new();
    env.svm.airdrop(&new_auth.pubkey(), 1_000_000_000).unwrap();

    let old_authority = env.payer.insecure_clone();
    update_authority(&mut env, &old_authority, new_auth.pubkey()).expect("rotate");
    assert_eq!(config_state(&env).authority, new_auth.pubkey());

    // Old authority can no longer rotate.
    let stranger = Keypair::new();
    let err = update_authority(&mut env, &old_authority, stranger.pubkey())
        .expect_err("old authority must be rejected after rotate");
    assert_eq!(custom_error(&err), Some(E_UNAUTHORIZED_AUTHORITY));

    // New authority can rotate fee_router (auth-gated).
    use covenant_stake_program::RotateFeeRouterArgs;
    rotate_fee_router(
        &mut env,
        &new_auth,
        RotateFeeRouterArgs {
            new_authority: Some(new_auth.pubkey()),
            new_max_deposit_lamports: None,
            new_rate_limit_secs: None,
        },
    )
    .expect("new authority can rotate");
}

#[test]
fn update_authority_rejects_non_authority() {
    let mut env = boot();
    let stranger = Keypair::new();
    env.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    let target = Keypair::new();
    let err = update_authority(&mut env, &stranger, target.pubkey())
        .expect_err("stranger cannot rotate authority");
    assert_eq!(custom_error(&err), Some(E_UNAUTHORIZED_AUTHORITY));
}

#[test]
fn pause_guards_each_mutating_user_ix() {
    let mut env = boot();
    let (owner, ata) = funded_owner(&mut env, 5 * MIN_LOCK_AMOUNT);
    create_position(&mut env, &owner, &ata, 1, MIN_LOCK_AMOUNT, TIER_30D_BPS)
        .expect("pre-pause create");

    // claim must be pause-gated (rewards distribution while paused is risky).
    let pause_kp = env.pause_keypair.insecure_clone();
    pause(&mut env, &pause_kp).expect("pause");

    let err = create_position(&mut env, &owner, &ata, 2, MIN_LOCK_AMOUNT, TIER_30D_BPS)
        .expect_err("create_position paused");
    assert_eq!(custom_error(&err), Some(E_PROTOCOL_PAUSED));

    let err = increase_amount(&mut env, &owner, &ata, 1, MIN_LOCK_AMOUNT)
        .expect_err("increase_amount paused");
    assert_eq!(custom_error(&err), Some(E_PROTOCOL_PAUSED));

    let err = claim(&mut env, &owner, 1).expect_err("claim paused");
    assert_eq!(custom_error(&err), Some(E_PROTOCOL_PAUSED));

    let err = deposit_sol_fees(&mut env, 100_000_000).expect_err("deposit paused");
    assert_eq!(custom_error(&err), Some(E_PROTOCOL_PAUSED));
}

#[test]
fn close_position_works_during_pause_after_lock_expires() {
    let mut env = boot();
    let (owner, ata) = funded_owner(&mut env, MIN_LOCK_AMOUNT);
    create_position(&mut env, &owner, &ata, 1, MIN_LOCK_AMOUNT, TIER_30D_BPS).expect("create");

    advance_clock(&mut env, TIER_30D_SECS + 1);

    let pause_kp = env.pause_keypair.insecure_clone();
    pause(&mut env, &pause_kp).expect("pause");

    // Vested principal must be returnable even while paused.
    close_position(&mut env, &owner, &ata, 1).expect("close during pause");
    assert_eq!(token_balance(&env, &ata), MIN_LOCK_AMOUNT);
}

#[test]
fn accrue_works_during_pause() {
    let mut env = boot();
    let (owner, ata) = funded_owner(&mut env, MIN_LOCK_AMOUNT);
    create_position(&mut env, &owner, &ata, 1, MIN_LOCK_AMOUNT, TIER_30D_BPS).expect("create");
    deposit_sol_fees(&mut env, 100_000_000).expect("deposit");
    let pause_kp = env.pause_keypair.insecure_clone();
    pause(&mut env, &pause_kp).expect("pause");
    accrue(&mut env).expect("accrue is unaffected by pause");
}

#[test]
fn update_min_lock_amount_enforces_floor() {
    let mut env = boot();
    let authority = env.payer.insecure_clone();
    update_min_lock_amount(&mut env, &authority, 5 * MIN_LOCK_AMOUNT).expect("raise minimum");
    assert_eq!(config_state(&env).min_lock_amount, 5 * MIN_LOCK_AMOUNT);

    // New lower amount now rejected.
    let (owner, ata) = funded_owner(&mut env, 5 * MIN_LOCK_AMOUNT);
    let err = create_position(&mut env, &owner, &ata, 1, MIN_LOCK_AMOUNT, TIER_30D_BPS)
        .expect_err("below new floor");
    assert_eq!(custom_error(&err), Some(E_LOCK_AMOUNT_TOO_SMALL));
}

#[test]
fn update_min_lock_amount_rejects_zero() {
    let mut env = boot();
    let authority = env.payer.insecure_clone();
    let err =
        update_min_lock_amount(&mut env, &authority, 0).expect_err("zero min must be rejected");
    assert_eq!(custom_error(&err), Some(E_INVALID_PARAMETER));
}

#[test]
fn update_max_active_locks_cannot_go_below_current_count() {
    let mut env = boot();
    let authority = env.payer.insecure_clone();

    // Open two positions.
    let (a, a_ata) = funded_owner(&mut env, MIN_LOCK_AMOUNT);
    let (b, b_ata) = funded_owner(&mut env, MIN_LOCK_AMOUNT);
    create_position(&mut env, &a, &a_ata, 1, MIN_LOCK_AMOUNT, TIER_30D_BPS).unwrap();
    create_position(&mut env, &b, &b_ata, 1, MIN_LOCK_AMOUNT, TIER_30D_BPS).unwrap();

    let err = update_max_active_locks(&mut env, &authority, 1)
        .expect_err("cannot reduce cap below current count");
    assert_eq!(custom_error(&err), Some(E_INVALID_PARAMETER));

    // Raising the cap works.
    update_max_active_locks(&mut env, &authority, 1000).expect("raise cap");
    assert_eq!(config_state(&env).max_active_locks, 1000);
}

#[test]
fn update_authority_zero_pubkey_bricks_protocol_so_not_recommended_but_allowed() {
    // Documentation test: the program does not reject Pubkey::default() in
    // update_authority. Operators must guard this externally. We assert the
    // observable behavior so future hardening lands a regression here.
    let mut env = boot();
    let authority = env.payer.insecure_clone();
    update_authority(&mut env, &authority, solana_sdk::pubkey::Pubkey::default())
        .expect("currently allowed — see audit M-level item");
    assert_eq!(
        config_state(&env).authority,
        solana_sdk::pubkey::Pubkey::default()
    );
}
