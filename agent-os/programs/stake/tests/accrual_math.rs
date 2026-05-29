mod common;

use common::*;

#[test]
fn deposit_rejected_when_no_active_stakers() {
    let mut env = boot();
    let err = deposit_sol_fees(&mut env, 500_000_000)
        .expect_err("deposit must reject when no stakers exist");
    assert_eq!(custom_error(&err), Some(E_NO_ACTIVE_STAKERS));

    let (owner, ata) = funded_owner(&mut env, MIN_LOCK_AMOUNT);
    create_position(&mut env, &owner, &ata, 1, MIN_LOCK_AMOUNT, TIER_30D_BPS)
        .expect("create");

    deposit_sol_fees(&mut env, 500_000_000).expect("deposit after first locker");
    let cfg = config_state(&env);
    assert_eq!(cfg.pending_sol_lamports, 0);
    assert_eq!(cfg.cumulative_sol_distributed, 500_000_000);
    assert!(cfg.acc_sol_per_weight > 0);
}

#[test]
fn multi_deposit_accrues_correctly() {
    let mut env = boot();
    let (owner, ata) = funded_owner(&mut env, MIN_LOCK_AMOUNT);
    create_position(&mut env, &owner, &ata, 1, MIN_LOCK_AMOUNT, TIER_30D_BPS)
        .expect("create");

    deposit_sol_fees(&mut env, 100_000_000).expect("deposit 1");
    advance_clock(&mut env, RATE_LIMIT_SECS);
    deposit_sol_fees(&mut env, 200_000_000).expect("deposit 2");
    advance_clock(&mut env, RATE_LIMIT_SECS);
    deposit_sol_fees(&mut env, 300_000_000).expect("deposit 3");

    let cfg = config_state(&env);
    assert_eq!(cfg.cumulative_sol_distributed, 600_000_000);
    assert_eq!(cfg.pending_sol_lamports, 0);

    let before = sol_balance(&env, &owner.pubkey());
    claim(&mut env, &owner, 1).expect("claim");
    let after = sol_balance(&env, &owner.pubkey());
    assert_eq!(after - before, 600_000_000);
}

#[test]
fn increase_amount_settles_pending_before_reseating_debt() {
    let mut env = boot();
    let (owner, ata) = funded_owner(&mut env, 5 * MIN_LOCK_AMOUNT);
    create_position(&mut env, &owner, &ata, 1, MIN_LOCK_AMOUNT, TIER_30D_BPS)
        .expect("create");

    deposit_sol_fees(&mut env, 1_000_000_000).expect("deposit");

    // Increase BEFORE claiming. The new weight should not retroactively
    // claim the prior fee — the prior fee should be settled into
    // unclaimed_lamports against the OLD weight.
    advance_clock(&mut env, RATE_LIMIT_SECS);
    increase_amount(&mut env, &owner, &ata, 1, MIN_LOCK_AMOUNT)
        .expect("increase");

    let pos = position_state(&env, &position_pda(&owner.pubkey(), 1));
    assert_eq!(pos.amount, 2 * MIN_LOCK_AMOUNT);
    // Pending was 1 SOL against weight=MIN_LOCK_AMOUNT (1x), so settled into
    // unclaimed_lamports should be exactly 1 SOL.
    assert_eq!(pos.unclaimed_lamports, 1_000_000_000);

    let before = sol_balance(&env, &owner.pubkey());
    claim(&mut env, &owner, 1).expect("claim");
    let after = sol_balance(&env, &owner.pubkey());
    assert_eq!(after - before, 1_000_000_000);
}

#[test]
fn accrue_is_idempotent_when_pending_is_zero() {
    let mut env = boot();
    let (owner, ata) = funded_owner(&mut env, MIN_LOCK_AMOUNT);
    create_position(&mut env, &owner, &ata, 1, MIN_LOCK_AMOUNT, TIER_30D_BPS)
        .expect("create");
    deposit_sol_fees(&mut env, 100_000_000).expect("deposit");

    let before = config_state(&env);
    accrue(&mut env).expect("accrue noop");
    let after = config_state(&env);
    assert_eq!(before.acc_sol_per_weight, after.acc_sol_per_weight);
    assert_eq!(before.cumulative_sol_distributed, after.cumulative_sol_distributed);
}

#[test]
fn tier_multipliers_apply_correctly_in_weight_math() {
    let mut env = boot();
    let amount = MIN_LOCK_AMOUNT;

    // Four lockers, one per tier.
    let (a, a_ata) = funded_owner(&mut env, amount);
    let (b, b_ata) = funded_owner(&mut env, amount);
    let (c, c_ata) = funded_owner(&mut env, amount);
    let (d, d_ata) = funded_owner(&mut env, amount);

    create_position(&mut env, &a, &a_ata, 1, amount, TIER_7D_BPS).expect("a");
    create_position(&mut env, &b, &b_ata, 1, amount, TIER_30D_BPS).expect("b");
    create_position(&mut env, &c, &c_ata, 1, amount, TIER_90D_BPS).expect("c");
    create_position(&mut env, &d, &d_ata, 1, amount, TIER_180D_BPS).expect("d");

    let cfg = config_state(&env);
    let expected = (amount as u128) * (5_000 + 10_000 + 15_000 + 20_000) / 10_000;
    assert_eq!(cfg.total_weight, expected);
    assert_eq!(cfg.active_lock_count, 4);

    // 7.5 SOL split across two deposits to fit under MAX_DEPOSIT_LAMPORTS cap.
    deposit_sol_fees(&mut env, 5_000_000_000).expect("deposit 1");
    advance_clock(&mut env, RATE_LIMIT_SECS);
    deposit_sol_fees(&mut env, 2_500_000_000).expect("deposit 2");

    let baseline_a = sol_balance(&env, &a.pubkey());
    let baseline_b = sol_balance(&env, &b.pubkey());
    let baseline_c = sol_balance(&env, &c.pubkey());
    let baseline_d = sol_balance(&env, &d.pubkey());

    claim(&mut env, &a, 1).expect("a claim");
    claim(&mut env, &b, 1).expect("b claim");
    claim(&mut env, &c, 1).expect("c claim");
    claim(&mut env, &d, 1).expect("d claim");

    let share_a = sol_balance(&env, &a.pubkey()) - baseline_a;
    let share_b = sol_balance(&env, &b.pubkey()) - baseline_b;
    let share_c = sol_balance(&env, &c.pubkey()) - baseline_c;
    let share_d = sol_balance(&env, &d.pubkey()) - baseline_d;

    let tol = 1_000_000;
    assert!(share_a.abs_diff(750_000_000) < tol);
    assert!(share_b.abs_diff(1_500_000_000) < tol);
    assert!(share_c.abs_diff(2_250_000_000) < tol);
    assert!(share_d.abs_diff(3_000_000_000) < tol);
}

#[test]
fn b2_regression_no_active_stakers_blocks_deposit() {
    // Defense in depth: even after a position is opened and closed, if
    // total_weight returns to 0, deposits must reject again.
    let mut env = boot();
    let (owner, ata) = funded_owner(&mut env, MIN_LOCK_AMOUNT);
    create_position(&mut env, &owner, &ata, 1, MIN_LOCK_AMOUNT, TIER_30D_BPS)
        .expect("create");
    deposit_sol_fees(&mut env, 100_000_000).expect("deposit while active");
    claim(&mut env, &owner, 1).expect("claim");
    advance_clock(&mut env, TIER_30D_SECS + 1);
    close_position(&mut env, &owner, &ata, 1).expect("close");

    let err = deposit_sol_fees(&mut env, 50_000_000)
        .expect_err("post-close deposit must reject");
    assert_eq!(custom_error(&err), Some(E_NO_ACTIVE_STAKERS));
}
