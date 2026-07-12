mod common;

use common::*;
use solana_sdk::pubkey::Pubkey;
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
    assert!(
        !position_exists(&env, &position),
        "position must be closed on unstake"
    );

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

    slash_stake(
        &mut env,
        &AGENT,
        &position,
        &stake_vault,
        &slash_vault,
        1_000,
    )
    .expect("full slash");
    assert_eq!(token_balance(&env, &slash_vault), 1_000);
    assert!(
        !stake_position(&env, &position).active,
        "fully slashed position is inactive"
    );
    assert_eq!(agent_stake(&env, &AGENT), 0);

    // Slash does not close the position; the owner reclaims rent + frees the PDA.
    close_position(&mut env, &position).expect("close fully-slashed position");
    assert!(!position_exists(&env, &position));

    let (position2, _, _) = stake_locked(&mut env, &AGENT, 250, 0);
    assert_eq!(position2, position);
    assert_eq!(agent_stake(&env, &AGENT), 250);
}

// Happy path: provider delivers, client approves, provider is paid.
#[test]
fn task_submit_then_release_pays_provider() {
    let mut env = boot();
    warp_unix(&mut env, 1_000);
    register_agent(&mut env, &AGENT);
    let task_id = [21u8; 32];
    let provider = Keypair::new();
    let client = env.payer.insecure_clone();
    let provider_covnt = create_token_account(
        &mut env.svm,
        &env.payer.insecure_clone(),
        &env.mint,
        &provider.pubkey(),
    );

    let tc = task_setup(&mut env, &task_id, 1_000);
    create_task(
        &mut env,
        &AGENT,
        &task_id,
        &provider.pubkey(),
        &Pubkey::default(),
        600,
        10_000,
        300,
        &tc,
    )
    .expect("create_task");
    assert_eq!(token_balance(&env, &tc.escrow_vault), 600);
    assert_eq!(token_balance(&env, &tc.client_covnt), 400);

    submit_task(&mut env, &tc, &provider, [6u8; 32], [7u8; 32]).expect("submit");
    assert_eq!(task_status(&env, &tc.task), 2); // TASK_SUBMITTED

    release_task(&mut env, &tc, &provider_covnt, &client).expect("release");
    assert_eq!(token_balance(&env, &provider_covnt), 600);
    assert_eq!(token_balance(&env, &tc.escrow_vault), 0);
    assert_eq!(task_status(&env, &tc.task), 3); // TASK_RELEASED
}

// Provider recourse: the client goes silent, so after the review window the
// provider self-claims the delivered work. This is the case that kept task
// escrow disabled before.
#[test]
fn task_claim_after_review_window_pays_provider() {
    let mut env = boot();
    warp_unix(&mut env, 1_000);
    register_agent(&mut env, &AGENT);
    let task_id = [23u8; 32];
    let provider = Keypair::new();
    let provider_covnt = create_token_account(
        &mut env.svm,
        &env.payer.insecure_clone(),
        &env.mint,
        &provider.pubkey(),
    );

    let tc = task_setup(&mut env, &task_id, 1_000);
    create_task(
        &mut env,
        &AGENT,
        &task_id,
        &provider.pubkey(),
        &Pubkey::default(),
        600,
        10_000,
        300,
        &tc,
    )
    .expect("create_task");
    submit_task(&mut env, &tc, &provider, [6u8; 32], [7u8; 32]).expect("submit"); // submitted_at = 1_000

    warp_unix(&mut env, 1_301); // past submitted_at + review_window
    claim_task(&mut env, &tc, &provider_covnt, &provider).expect("claim");
    assert_eq!(token_balance(&env, &provider_covnt), 600);
    assert_eq!(task_status(&env, &tc.task), 3); // TASK_RELEASED
}

// Dispute resolved for the provider: the arbiter releases mid-window.
#[test]
fn task_arbiter_release_pays_provider() {
    let mut env = boot();
    warp_unix(&mut env, 1_000);
    register_agent(&mut env, &AGENT);
    let task_id = [24u8; 32];
    let provider = Keypair::new();
    let arbiter = Keypair::new();
    let provider_covnt = create_token_account(
        &mut env.svm,
        &env.payer.insecure_clone(),
        &env.mint,
        &provider.pubkey(),
    );

    let tc = task_setup(&mut env, &task_id, 1_000);
    create_task(
        &mut env,
        &AGENT,
        &task_id,
        &provider.pubkey(),
        &arbiter.pubkey(),
        600,
        10_000,
        300,
        &tc,
    )
    .expect("create_task");
    submit_task(&mut env, &tc, &provider, [6u8; 32], [7u8; 32]).expect("submit");

    release_task(&mut env, &tc, &provider_covnt, &arbiter).expect("arbiter release");
    assert_eq!(token_balance(&env, &provider_covnt), 600);
    assert_eq!(task_status(&env, &tc.task), 3); // TASK_RELEASED
}

// Client recourse: the provider never submits, so after the deadline the
// client reclaims the escrow.
#[test]
fn task_refund_after_deadline_returns_client() {
    let mut env = boot();
    warp_unix(&mut env, 1_000);
    register_agent(&mut env, &AGENT);
    let task_id = [22u8; 32];
    let provider = Keypair::new();
    let client = env.payer.insecure_clone();

    let tc = task_setup(&mut env, &task_id, 1_000);
    create_task(
        &mut env,
        &AGENT,
        &task_id,
        &provider.pubkey(),
        &Pubkey::default(),
        600,
        5_000,
        300,
        &tc,
    )
    .expect("create_task");

    warp_unix(&mut env, 6_000); // past the deadline, provider never submitted
    refund_task(&mut env, &tc, &client).expect("refund");
    assert_eq!(token_balance(&env, &tc.client_covnt), 1_000); // fully refunded
    assert_eq!(task_status(&env, &tc.task), 4); // TASK_REFUNDED
}

// Dispute resolved for the client: the arbiter refunds during the window.
#[test]
fn task_arbiter_refund_within_window_returns_client() {
    let mut env = boot();
    warp_unix(&mut env, 1_000);
    register_agent(&mut env, &AGENT);
    let task_id = [25u8; 32];
    let provider = Keypair::new();
    let arbiter = Keypair::new();
    let client = env.payer.insecure_clone();

    let tc = task_setup(&mut env, &task_id, 1_000);
    create_task(
        &mut env,
        &AGENT,
        &task_id,
        &provider.pubkey(),
        &arbiter.pubkey(),
        600,
        10_000,
        300,
        &tc,
    )
    .expect("create_task");
    submit_task(&mut env, &tc, &provider, [6u8; 32], [7u8; 32]).expect("submit"); // submitted_at 1_000

    warp_unix(&mut env, 1_100); // inside the review window
    refund_task(&mut env, &tc, &arbiter).expect("arbiter refund");
    assert_eq!(token_balance(&env, &tc.client_covnt), 1_000);
    assert_eq!(task_status(&env, &tc.task), 4); // TASK_REFUNDED
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
fn min_stake_lock_enforced_on_stake() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    set_min_stake_lock(&mut env, 3_600).expect("set min lock");
    assert_eq!(config_account(&env).min_stake_lock, 3_600);

    // litesvm clock starts at 0, so lock_until must be >= 0 + 3600.
    let err = try_stake(&mut env, &AGENT, 500, 0).expect_err("lock_until below the floor");
    assert_eq!(custom_error(&err), Some(E_LOCK_TOO_SHORT));

    try_stake(&mut env, &AGENT, 500, 3_600).expect("lock_until at the floor is accepted");
    assert_eq!(agent_stake(&env, &AGENT), 500);
}

// Migration: a legacy 146-byte config grows to the current layout and gains
// min_stake_lock without losing existing fields.
#[test]
fn migrate_config_upgrades_legacy_layout() {
    let mut env = boot();
    plant_legacy_config(&mut env);
    migrate_config(&mut env, 7_200).expect("migrate legacy config");

    let cfg = config_account(&env); // only deserializes if the realloc succeeded
    assert_eq!(cfg.min_stake_lock, 7_200);
    assert_eq!(cfg.credits_per_covnt, 10); // preserved
    assert_eq!(cfg.authority, env.payer.pubkey()); // preserved
    assert!(!cfg.paused);
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
