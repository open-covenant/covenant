mod common;

use common::*;

#[test]
fn create_position_rejects_invalid_tier() {
    let mut env = boot();
    let (owner, ata) = funded_owner(&mut env, MIN_LOCK_AMOUNT);
    let err = create_position(&mut env, &owner, &ata, 1, MIN_LOCK_AMOUNT, 12_500)
        .expect_err("12500 bps is not a valid tier");
    assert_eq!(custom_error(&err), Some(E_INVALID_LOCK_TIER));
}

#[test]
fn create_position_rejects_amount_below_minimum() {
    let mut env = boot();
    let (owner, ata) = funded_owner(&mut env, MIN_LOCK_AMOUNT);
    let err = create_position(&mut env, &owner, &ata, 1, MIN_LOCK_AMOUNT - 1, TIER_30D_BPS)
        .expect_err("below min_lock_amount must reject");
    assert_eq!(custom_error(&err), Some(E_LOCK_AMOUNT_TOO_SMALL));
}

#[test]
fn create_position_rejected_while_paused() {
    let mut env = boot();
    let pause_kp = env.pause_keypair.insecure_clone();
    pause(&mut env, &pause_kp).expect("pause");

    let (owner, ata) = funded_owner(&mut env, MIN_LOCK_AMOUNT);
    let err = create_position(&mut env, &owner, &ata, 1, MIN_LOCK_AMOUNT, TIER_30D_BPS)
        .expect_err("paused");
    assert_eq!(custom_error(&err), Some(E_PROTOCOL_PAUSED));
}

#[test]
fn pause_authority_can_pause_but_not_unpause() {
    let mut env = boot();
    let pause_kp = env.pause_keypair.insecure_clone();
    pause(&mut env, &pause_kp).expect("pause via pause_authority");
    assert!(config_state(&env).paused);

    let err = unpause(&mut env, &pause_kp).expect_err("pause_authority cannot unpause");
    assert_eq!(custom_error(&err), Some(E_UNAUTHORIZED_AUTHORITY));

    let authority = env.payer.insecure_clone();
    unpause(&mut env, &authority).expect("authority can unpause");
    assert!(!config_state(&env).paused);
}

#[test]
fn unauthorized_signer_cannot_pause() {
    let mut env = boot();
    let stranger = Keypair::new();
    env.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    let err = pause(&mut env, &stranger).expect_err("stranger cannot pause");
    assert_eq!(custom_error(&err), Some(E_UNAUTHORIZED_AUTHORITY));
}

#[test]
fn increase_amount_rejected_after_lock_expiry() {
    let mut env = boot();
    let (owner, ata) = funded_owner(&mut env, 5 * MIN_LOCK_AMOUNT);
    create_position(&mut env, &owner, &ata, 1, MIN_LOCK_AMOUNT, TIER_30D_BPS)
        .expect("create");
    advance_clock(&mut env, TIER_30D_SECS + 1);
    let err = increase_amount(&mut env, &owner, &ata, 1, MIN_LOCK_AMOUNT)
        .expect_err("locked window closed");
    assert_eq!(custom_error(&err), Some(E_LOCK_EXPIRED));
}

#[test]
fn claim_rejects_when_nothing_to_claim() {
    let mut env = boot();
    let (owner, ata) = funded_owner(&mut env, MIN_LOCK_AMOUNT);
    create_position(&mut env, &owner, &ata, 1, MIN_LOCK_AMOUNT, TIER_30D_BPS)
        .expect("create");
    let err = claim(&mut env, &owner, 1).expect_err("no fees deposited");
    assert_eq!(custom_error(&err), Some(E_NOTHING_TO_CLAIM));
}

#[test]
fn other_owner_cannot_claim_position() {
    let mut env = boot();
    let (alice, alice_ata) = funded_owner(&mut env, MIN_LOCK_AMOUNT);
    create_position(&mut env, &alice, &alice_ata, 1, MIN_LOCK_AMOUNT, TIER_30D_BPS)
        .expect("create");
    deposit_sol_fees(&mut env, 100_000_000).expect("deposit");

    let bob = Keypair::new();
    env.svm.airdrop(&bob.pubkey(), 1_000_000_000).unwrap();
    let err = claim(&mut env, &bob, 1).expect_err("bob cannot claim alice's position");
    // Position PDA derives from owner+nonce; bob's signed claim with nonce=1 hits
    // a non-existent PDA — anchor reports AccountNotInitialized (3012).
    let code = custom_error(&err).expect("custom err");
    assert!(
        code == 3012 || code == E_UNAUTHORIZED_AUTHORITY,
        "expected AccountNotInitialized or UnauthorizedAuthority, got {}",
        code
    );
}
