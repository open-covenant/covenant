mod common;

use common::*;
use solana_sdk::signer::Signer;

#[test]
fn buy_credits_succeeds_on_canonical_pda() {
    let mut env = boot();
    let credits = open_credit_account(&mut env);
    let owner_covnt = funded_covnt(&mut env, 1_000);

    buy_credits(&mut env, &credits, &owner_covnt, 5).expect("buy against canonical PDA");

    // credits_per_covnt = 10
    assert_eq!(credit_balance(&env, &credits), 50);
}

#[test]
fn buy_credits_rejects_non_pda_credit_account() {
    let mut env = boot();
    let owner = env.payer.pubkey();
    let phantom = plant_phantom_credit_account(&mut env, &owner);
    let owner_covnt = funded_covnt(&mut env, 1_000);

    let err = buy_credits(&mut env, &phantom, &owner_covnt, 5)
        .expect_err("a non-canonical credit account must be rejected by the seeds constraint");
    assert_eq!(custom_error(&err), Some(E_CONSTRAINT_SEEDS));

    // no COVNT left the owner's account
    assert_eq!(token_balance(&env, &owner_covnt), 1_000);
}
