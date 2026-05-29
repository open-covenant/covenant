mod common;

use common::*;

#[test]
fn boot_initializes_config_and_fee_router() {
    let env = boot();

    let cfg = config_state(&env);
    assert_eq!(cfg.authority, env.payer.pubkey());
    assert_eq!(cfg.pause_authority, env.pause_keypair.pubkey());
    assert_eq!(cfg.covnt_mint, env.mint);
    assert_eq!(cfg.min_lock_amount, MIN_LOCK_AMOUNT);
    assert_eq!(cfg.max_active_locks, 500);
    assert_eq!(cfg.active_lock_count, 0);
    assert_eq!(cfg.total_weight, 0);
    assert_eq!(cfg.acc_sol_per_weight, 0);
    assert_eq!(cfg.pending_sol_lamports, 0);
    assert!(!cfg.paused);

    let fr = fee_router_state(&env);
    assert_eq!(fr.authority, env.fee_router_keypair.pubkey());
    assert_eq!(fr.max_deposit_lamports, MAX_DEPOSIT_LAMPORTS);
    assert_eq!(fr.rate_limit_secs, RATE_LIMIT_SECS);
    assert_eq!(fr.last_deposit_ts, i64::MIN);

    assert_eq!(token_balance(&env, &env.locked_cvnt_vault), 0);
    assert_eq!(token_balance(&env, &env.buylock_cvnt_vault), 0);
}

#[test]
fn create_position_locks_principal_and_records_weight() {
    let mut env = boot();
    let amount = 5_000_000_000; // 5000 CVNT
    let (owner, ata) = funded_owner(&mut env, amount);

    let position = create_position(&mut env, &owner, &ata, 1, amount, TIER_90D_BPS)
        .expect("create_position");

    let pos = position_state(&env, &position);
    assert_eq!(pos.owner, owner.pubkey());
    assert_eq!(pos.nonce, 1);
    assert_eq!(pos.amount, amount);
    assert_eq!(pos.multiplier_bps, TIER_90D_BPS);
    let expected_weight = (amount as u128) * (TIER_90D_BPS as u128) / 10_000;
    assert_eq!(pos.weight, expected_weight);
    assert_eq!(pos.unclaimed_lamports, 0);
    assert_eq!(pos.reward_debt, 0);

    let cfg = config_state(&env);
    assert_eq!(cfg.total_weight, expected_weight);
    assert_eq!(cfg.active_lock_count, 1);
    assert_eq!(token_balance(&env, &ata), 0);
    assert_eq!(token_balance(&env, &env.locked_cvnt_vault), amount);
}

#[test]
fn deposit_fees_distributes_to_single_locker_and_claim_pays_out() {
    let mut env = boot();
    let amount = MIN_LOCK_AMOUNT;
    let (owner, ata) = funded_owner(&mut env, amount);
    create_position(&mut env, &owner, &ata, 1, amount, TIER_30D_BPS)
        .expect("create_position");

    let fee_amount = 1_000_000_000; // 1 SOL
    deposit_sol_fees(&mut env, fee_amount).expect("deposit_sol_fees");

    let cfg = config_state(&env);
    assert_eq!(cfg.pending_sol_lamports, 0);
    assert_eq!(cfg.cumulative_sol_distributed, fee_amount);
    assert!(cfg.acc_sol_per_weight > 0);

    let owner_sol_before = sol_balance(&env, &owner.pubkey());
    claim(&mut env, &owner, 1).expect("claim");
    let owner_sol_after = sol_balance(&env, &owner.pubkey());

    assert_eq!(owner_sol_after - owner_sol_before, fee_amount);
}

#[test]
fn close_position_returns_principal_after_lock_expires() {
    let mut env = boot();
    let amount = MIN_LOCK_AMOUNT;
    let (owner, ata) = funded_owner(&mut env, amount);
    create_position(&mut env, &owner, &ata, 1, amount, TIER_30D_BPS)
        .expect("create_position");

    let err = close_position(&mut env, &owner, &ata, 1)
        .expect_err("close before lock_end should fail");
    assert_eq!(custom_error(&err), Some(E_LOCK_NOT_EXPIRED));

    advance_clock(&mut env, TIER_30D_SECS + 1);
    close_position(&mut env, &owner, &ata, 1).expect("close after lock_end");

    assert_eq!(token_balance(&env, &ata), amount);
    assert_eq!(token_balance(&env, &env.locked_cvnt_vault), 0);
    let cfg = config_state(&env);
    assert_eq!(cfg.active_lock_count, 0);
    assert_eq!(cfg.total_weight, 0);
}

#[test]
fn close_paid_out_unclaimed_lamports() {
    let mut env = boot();
    let amount = MIN_LOCK_AMOUNT;
    let (owner, ata) = funded_owner(&mut env, amount);
    create_position(&mut env, &owner, &ata, 1, amount, TIER_30D_BPS)
        .expect("create_position");

    let fee_amount = 500_000_000;
    deposit_sol_fees(&mut env, fee_amount).expect("deposit_sol_fees");

    advance_clock(&mut env, TIER_30D_SECS + 1);
    let owner_before = sol_balance(&env, &owner.pubkey());
    close_position(&mut env, &owner, &ata, 1).expect("close");
    let owner_after = sol_balance(&env, &owner.pubkey());

    let delta = owner_after - owner_before;
    assert!(delta >= fee_amount, "owner should receive at least the fee amount");
}

#[test]
fn two_lockers_split_fees_pro_rata_by_weight() {
    let mut env = boot();

    // Alice: 1000 CVNT @ 1x = 1e9 weight
    // Bob:   1000 CVNT @ 2x = 2e9 weight
    // Total = 3e9 weight, fee = 3 SOL
    // Alice receives 1 SOL, Bob receives 2 SOL.
    let alice_amount = MIN_LOCK_AMOUNT;
    let bob_amount = MIN_LOCK_AMOUNT;

    let (alice, alice_ata) = funded_owner(&mut env, alice_amount);
    let (bob, bob_ata) = funded_owner(&mut env, bob_amount);

    create_position(&mut env, &alice, &alice_ata, 1, alice_amount, TIER_30D_BPS)
        .expect("alice create");
    create_position(&mut env, &bob, &bob_ata, 1, bob_amount, TIER_180D_BPS)
        .expect("bob create");

    let fee_amount = 3_000_000_000;
    deposit_sol_fees(&mut env, fee_amount).expect("deposit");

    let alice_before = sol_balance(&env, &alice.pubkey());
    let bob_before = sol_balance(&env, &bob.pubkey());
    claim(&mut env, &alice, 1).expect("alice claim");
    claim(&mut env, &bob, 1).expect("bob claim");
    let alice_after = sol_balance(&env, &alice.pubkey());
    let bob_after = sol_balance(&env, &bob.pubkey());

    let alice_share = alice_after - alice_before;
    let bob_share = bob_after - bob_before;

    let expected_alice = fee_amount / 3;
    let expected_bob = 2 * fee_amount / 3;
    let tolerance = 100_000; // 0.0001 SOL rounding tolerance

    assert!(
        alice_share.abs_diff(expected_alice) < tolerance,
        "alice {} ≠ expected {}",
        alice_share,
        expected_alice
    );
    assert!(
        bob_share.abs_diff(expected_bob) < tolerance,
        "bob {} ≠ expected {}",
        bob_share,
        expected_bob
    );
}
