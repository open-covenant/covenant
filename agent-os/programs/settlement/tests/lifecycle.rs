mod common;

use common::*;
use solana_sdk::signer::{keypair::Keypair, Signer};

const AGENT: [u8; 32] = [11u8; 32];

#[test]
fn credit_buy_then_consume_tracks_balance() {
    let mut env = boot();
    let credits = open_credit_account(&mut env);
    let owner_covnt = funded_covnt(&mut env, 1_000);

    buy_credits(&mut env, &credits, &owner_covnt, 5).expect("buy");
    assert_eq!(credit_balance(&env, &credits), 50); // credits_per_covnt = 10

    consume_credits(&mut env, &credits, 30).expect("consume");
    assert_eq!(credit_balance(&env, &credits), 20);

    consume_credits(&mut env, &credits, 20).expect("consume rest");
    assert_eq!(credit_balance(&env, &credits), 0);
}

// Regression for the re-stake lockout: before `unstake` closed the position,
// a second stake to the same (agent, owner) PDA failed on `init`.
#[test]
fn stake_unstake_then_restake_succeeds() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);

    let (position, stake_vault, owner_covnt) = stake_locked(&mut env, &AGENT, 1_000, 0);
    assert_eq!(agent_stake(&env, &AGENT), 1_000);
    assert_eq!(token_balance(&env, &stake_vault), 1_000);

    unstake(&mut env, &AGENT, &position, &stake_vault, &owner_covnt).expect("unstake");
    assert_eq!(token_balance(&env, &owner_covnt), 1_000); // funds returned
    assert_eq!(agent_stake(&env, &AGENT), 0);
    assert!(!position_exists(&env, &position), "position must be closed on unstake");

    // The PDA is free again, so re-staking against the same agent works.
    let (position2, vault2, _) = stake_locked(&mut env, &AGENT, 400, 0);
    assert_eq!(position2, position, "same canonical PDA");
    assert_eq!(token_balance(&env, &vault2), 400);
    assert_eq!(agent_stake(&env, &AGENT), 400);
}

#[test]
fn full_slash_then_close_then_restake() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    let (position, stake_vault, _) = stake_locked(&mut env, &AGENT, 1_000, 0);
    let slash_vault = create_token_account(
        &mut env.svm,
        &env.payer.insecure_clone(),
        &env.mint,
        &env.payer.pubkey(),
    );

    slash_stake(&mut env, &AGENT, &position, &stake_vault, &slash_vault, 1_000).expect("full slash");
    assert_eq!(token_balance(&env, &slash_vault), 1_000);
    assert!(!stake_position(&env, &position).active, "fully slashed position is inactive");
    assert_eq!(agent_stake(&env, &AGENT), 0);

    // Slash does not close the position; the owner reclaims rent + frees the PDA.
    close_position(&mut env, &position).expect("close fully-slashed position");
    assert!(!position_exists(&env, &position));

    let (position2, _, _) = stake_locked(&mut env, &AGENT, 250, 0);
    assert_eq!(position2, position);
    assert_eq!(agent_stake(&env, &AGENT), 250);
}

#[test]
fn task_create_and_release_pays_provider() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    let task_id = [21u8; 32];
    let provider = Keypair::new().pubkey();
    let provider_covnt =
        create_token_account(&mut env.svm, &env.payer.insecure_clone(), &env.mint, &provider);

    let tc = task_setup(&mut env, &task_id, 1_000);
    create_task(&mut env, &AGENT, &task_id, &provider, 600, 10_000, &tc).expect("create_task");
    assert_eq!(token_balance(&env, &tc.escrow_vault), 600);
    assert_eq!(token_balance(&env, &tc.client_covnt), 400);

    release_task(&mut env, &tc, &provider_covnt).expect("release");
    assert_eq!(token_balance(&env, &provider_covnt), 600);
    assert_eq!(token_balance(&env, &tc.escrow_vault), 0);
    assert_eq!(task_status(&env, &tc.task), 2); // TASK_RELEASED
}

#[test]
fn task_refund_after_deadline_returns_client() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    let task_id = [22u8; 32];
    let provider = Keypair::new().pubkey();

    let tc = task_setup(&mut env, &task_id, 1_000);
    create_task(&mut env, &AGENT, &task_id, &provider, 600, 5_000, &tc).expect("create_task");

    warp_unix(&mut env, 6_000); // past the deadline
    refund_task(&mut env, &tc).expect("refund");
    assert_eq!(token_balance(&env, &tc.client_covnt), 1_000); // fully refunded
    assert_eq!(task_status(&env, &tc.task), 3); // TASK_REFUNDED
}

#[test]
fn burn_covnt_destroys_supply() {
    let mut env = boot();
    let owner_covnt = funded_covnt(&mut env, 1_000);
    burn_covnt(&mut env, &owner_covnt, 250).expect("burn");
    assert_eq!(token_balance(&env, &owner_covnt), 750);
}

#[test]
fn anchor_receipt_batch_records() {
    let mut env = boot();
    anchor_receipt_batch(&mut env, &[31u8; 32], 42).expect("anchor batch");
}

#[test]
fn set_agent_inactive_blocks_staking() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    set_agent_active(&mut env, &AGENT, false).expect("deactivate");

    let err = try_stake(&mut env, &AGENT, 500, 0).expect_err("inactive agent must reject stake");
    assert_eq!(custom_error(&err), Some(E_AGENT_INACTIVE));
}

#[test]
fn set_credits_per_covnt_reprices_buys() {
    let mut env = boot();
    set_credits_per_covnt(&mut env, 25).expect("reprice");
    let credits = open_credit_account(&mut env);
    let owner_covnt = funded_covnt(&mut env, 1_000);
    buy_credits(&mut env, &credits, &owner_covnt, 4).expect("buy at new rate");
    assert_eq!(credit_balance(&env, &credits), 100); // 4 * 25
}

#[test]
fn update_authority_transfers_control() {
    let mut env = boot();
    let new_auth = Keypair::new();
    update_authority(&mut env, &new_auth.pubkey()).expect("rotate authority");

    // Old authority (payer) can no longer drive an authority-gated update.
    let err = set_credits_per_covnt(&mut env, 7)
        .expect_err("old authority must be rejected after rotation");
    assert_eq!(custom_error(&err), Some(E_UNAUTHORIZED));
}

#[test]
fn update_slash_authority_and_treasury() {
    let mut env = boot();
    let new_slash = Keypair::new();
    update_slash_authority(&mut env, &new_slash.pubkey()).expect("rotate slash authority");
    assert_eq!(config_account(&env).slash_authority, new_slash.pubkey());

    let new_treasury = create_token_account(
        &mut env.svm,
        &env.payer.insecure_clone(),
        &env.mint,
        &env.payer.pubkey(),
    );
    update_treasury(&mut env, &new_treasury).expect("rotate treasury");
    assert_eq!(config_account(&env).treasury, new_treasury);
}
