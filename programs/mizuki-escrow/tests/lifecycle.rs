use crate::{
    state::{EscrowStatus, GUARD_LEN},
    test_common::*,
};
use solana_sdk::signature::Signer;

#[test]
fn funded_bounty_binds_once_and_releases_exact_principal() {
    let mut env = boot();
    fund(&mut env).unwrap();
    assert_eq!(state(&env).status, EscrowStatus::Funded);

    bind(&mut env).unwrap();
    let bound = state(&env);
    assert_eq!(bound.status, EscrowStatus::Bound);
    assert_eq!(bound.claimant, env.claimant.pubkey());

    let claimant_before = balance(&env, &env.claimant.pubkey());
    let vault_address = vault(&env);
    let vault_before = balance(&env, &vault_address);
    let state_address = state_pda(&env.authority.pubkey(), &BOUNTY).0;
    let state_rent = balance(&env, &state_address);
    let authority_before = balance(&env, &env.authority.pubkey());
    let ix = release_ix(env.authority.pubkey(), BOUNTY, env.claimant.pubkey());
    send(&mut env.svm, &env.payer, &[ix], &[&env.authority]).unwrap();

    assert_eq!(guard(&env).status, EscrowStatus::Released);
    assert_eq!(
        balance(&env, &env.claimant.pubkey()) - claimant_before,
        AMOUNT
    );
    assert_eq!(balance(&env, &vault_address), 0);
    assert_eq!(balance(&env, &state_address), 0);
    let guard_address = guard_pda(&env.authority.pubkey(), &BOUNTY).0;
    assert_eq!(
        balance(&env, &guard_address),
        env.svm.minimum_balance_for_rent_exemption(GUARD_LEN)
    );
    assert_eq!(
        balance(&env, &env.authority.pubkey()) - authority_before,
        vault_before - AMOUNT + state_rent
    );
}

#[test]
fn unbound_refund_requires_offer_expiry_and_returns_exact_principal() {
    let mut env = boot();
    fund(&mut env).unwrap();
    let vault_address = vault(&env);
    let state_address = state_pda(&env.authority.pubkey(), &BOUNTY).0;
    let ix = refund_ix(env.authority.pubkey(), BOUNTY);
    assert!(send(
        &mut env.svm,
        &env.payer,
        std::slice::from_ref(&ix),
        &[&env.authority]
    )
    .is_err());

    warp(&mut env.svm, OFFER_EXPIRY);
    let authority_before = balance(&env, &env.authority.pubkey());
    let reclaimed = balance(&env, &vault_address) + balance(&env, &state_address);
    send(&mut env.svm, &env.payer, &[ix], &[&env.authority]).unwrap();
    assert_eq!(guard(&env).status, EscrowStatus::Refunded);
    assert_eq!(
        balance(&env, &env.authority.pubkey()) - authority_before,
        reclaimed
    );
    assert_eq!(balance(&env, &vault_address), 0);
    assert_eq!(balance(&env, &state_address), 0);
}

#[test]
fn bound_refund_requires_claim_expiry() {
    let mut env = boot();
    fund(&mut env).unwrap();
    bind(&mut env).unwrap();
    let ix = refund_ix(env.authority.pubkey(), BOUNTY);
    assert!(send(
        &mut env.svm,
        &env.payer,
        std::slice::from_ref(&ix),
        &[&env.authority]
    )
    .is_err());

    warp(&mut env.svm, CLAIM_EXPIRY);
    send(&mut env.svm, &env.payer, &[ix], &[&env.authority]).unwrap();
    assert_eq!(guard(&env).status, EscrowStatus::Refunded);
}

#[test]
fn unsolicited_lamports_cannot_block_release_or_change_principal() {
    let mut env = boot();
    fund(&mut env).unwrap();
    bind(&mut env).unwrap();
    let vault_address = vault(&env);
    let mut account = env.svm.get_account(&vault_address).unwrap();
    account.lamports += 1;
    env.svm.set_account(vault_address, account).unwrap();

    let vault_before = balance(&env, &vault_address);
    let claimant_before = balance(&env, &env.claimant.pubkey());
    let ix = release_ix(env.authority.pubkey(), BOUNTY, env.claimant.pubkey());
    send(&mut env.svm, &env.payer, &[ix], &[&env.authority]).unwrap();
    assert!(vault_before > AMOUNT);
    assert_eq!(balance(&env, &vault_address), 0);
    assert_eq!(
        balance(&env, &env.claimant.pubkey()) - claimant_before,
        AMOUNT
    );
}

#[test]
fn release_and_refund_are_mutually_exclusive_at_claim_expiry() {
    let mut env = boot();
    fund(&mut env).unwrap();
    bind(&mut env).unwrap();
    warp(&mut env.svm, CLAIM_EXPIRY);

    let release = release_ix(env.authority.pubkey(), BOUNTY, env.claimant.pubkey());
    assert!(send(&mut env.svm, &env.payer, &[release], &[&env.authority]).is_err());
    assert_eq!(state(&env).status, EscrowStatus::Bound);

    let refund = refund_ix(env.authority.pubkey(), BOUNTY);
    send(&mut env.svm, &env.payer, &[refund], &[&env.authority]).unwrap();
    assert_eq!(guard(&env).status, EscrowStatus::Refunded);
}

#[test]
fn refund_cannot_front_run_release_before_claim_expiry() {
    let mut env = boot();
    fund(&mut env).unwrap();
    bind(&mut env).unwrap();
    warp(&mut env.svm, CLAIM_EXPIRY - 1);

    let refund = refund_ix(env.authority.pubkey(), BOUNTY);
    assert!(send(&mut env.svm, &env.payer, &[refund], &[&env.authority]).is_err());
    let release = release_ix(env.authority.pubkey(), BOUNTY, env.claimant.pubkey());
    send(&mut env.svm, &env.payer, &[release], &[&env.authority]).unwrap();
    assert_eq!(guard(&env).status, EscrowStatus::Released);
}
