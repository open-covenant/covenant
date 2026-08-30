mod common;

use common::*;
use covenant_settlement_program::{
    COMPUTE_FUNDED, COMPUTE_REFUNDED, COMPUTE_SETTLED, COMPUTE_SETTLEMENT_GRACE_SECONDS,
    MIN_COMPUTE_ESCROW_SECONDS,
};
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;

const JOB: [u8; 32] = [71u8; 32];
const OTHER_JOB: [u8; 32] = [72u8; 32];
const MAX_USDC: u64 = 1_000_000;
const RECEIPT: [u8; 32] = [81u8; 32];
const REFUND: [u8; 32] = [91u8; 32];
const FUNDED_AT: i64 = 1_000;
const EXPIRES_AT: i64 = 2_000;
const REFUND_OPENS_AT: i64 = EXPIRES_AT + COMPUTE_SETTLEMENT_GRACE_SECONDS;

#[test]
fn settlement_pays_actual_and_refunds_remainder_atomically() {
    let mut env = boot();
    warp_unix(&mut env, FUNDED_AT);
    let compute = setup_compute_job(&mut env, &JOB, MAX_USDC, EXPIRES_AT);

    assert_eq!(compute_payment_config(&env).usdc_mint, compute.usdc_mint);
    assert_eq!(token_balance(&env, &compute.escrow_vault), MAX_USDC);
    assert_eq!(token_balance(&env, &compute.client_usdc), 2_000_000);

    settle_compute_job(
        &mut env,
        &compute,
        &compute.settlement_authority,
        &compute.provider_usdc,
        &compute.usdc_mint,
        400_000,
        RECEIPT,
    )
    .expect("settle compute job");

    assert_eq!(token_balance(&env, &compute.provider_usdc), 400_000);
    assert_eq!(token_balance(&env, &compute.client_usdc), 2_600_000);
    assert_eq!(token_balance(&env, &compute.escrow_vault), 0);

    let escrow = compute_escrow(&env, &compute.escrow);
    assert_eq!(escrow.status, COMPUTE_SETTLED);
    assert_eq!(escrow.job_id, JOB);
    assert_eq!(escrow.quote_commitment, [42u8; 32]);
    assert_eq!(escrow.provider, compute.provider);
    assert_eq!(escrow.provider_usdc, compute.provider_usdc);
    assert_eq!(escrow.escrow_vault, compute.escrow_vault);
    assert_eq!(escrow.max_usdc_amount, MAX_USDC);
    assert_eq!(escrow.actual_usdc_amount, 400_000);
    assert_eq!(escrow.refunded_usdc_amount, 600_000);
    assert_eq!(escrow.terminal_commitment, RECEIPT);
}

#[test]
fn exact_terminal_replay_is_idempotent_but_conflict_fails() {
    let mut env = boot();
    warp_unix(&mut env, FUNDED_AT);
    let compute = setup_compute_job(&mut env, &JOB, MAX_USDC, EXPIRES_AT);

    settle_compute_job(
        &mut env,
        &compute,
        &compute.settlement_authority,
        &compute.provider_usdc,
        &compute.usdc_mint,
        400_000,
        RECEIPT,
    )
    .expect("first settlement");
    bump_blockhash(&mut env);
    settle_compute_job(
        &mut env,
        &compute,
        &compute.settlement_authority,
        &compute.provider_usdc,
        &compute.usdc_mint,
        400_000,
        RECEIPT,
    )
    .expect("exact replay");
    assert_eq!(token_balance(&env, &compute.provider_usdc), 400_000);
    assert_eq!(token_balance(&env, &compute.client_usdc), 2_600_000);

    bump_blockhash(&mut env);
    let err = settle_compute_job(
        &mut env,
        &compute,
        &compute.settlement_authority,
        &compute.provider_usdc,
        &compute.usdc_mint,
        400_001,
        RECEIPT,
    )
    .expect_err("conflicting replay");
    assert_eq!(custom_error(&err), Some(E_COMPUTE_TERMINAL_MISMATCH));
}

#[test]
fn terminal_replay_remains_idempotent_after_authority_rotation() {
    let mut env = boot();
    warp_unix(&mut env, FUNDED_AT);
    let compute = setup_compute_job(&mut env, &JOB, MAX_USDC, EXPIRES_AT);

    settle_compute_job(
        &mut env,
        &compute,
        &compute.settlement_authority,
        &compute.provider_usdc,
        &compute.usdc_mint,
        400_000,
        RECEIPT,
    )
    .expect("first settlement");

    let next_authority = Keypair::new();
    bump_blockhash(&mut env);
    update_compute_settlement_authority(&mut env, &next_authority.pubkey())
        .expect("rotate settlement authority");

    bump_blockhash(&mut env);
    settle_compute_job(
        &mut env,
        &compute,
        &compute.settlement_authority,
        &compute.provider_usdc,
        &compute.usdc_mint,
        400_000,
        RECEIPT,
    )
    .expect("terminal authority exact replay");

    bump_blockhash(&mut env);
    let err = settle_compute_job(
        &mut env,
        &compute,
        &next_authority,
        &compute.provider_usdc,
        &compute.usdc_mint,
        400_000,
        RECEIPT,
    )
    .expect_err("different authority cannot replay a terminal outcome");
    assert_eq!(custom_error(&err), Some(E_UNAUTHORIZED));
}

#[test]
fn overcharge_is_rejected_without_moving_funds() {
    let mut env = boot();
    warp_unix(&mut env, FUNDED_AT);
    let compute = setup_compute_job(&mut env, &JOB, MAX_USDC, EXPIRES_AT);

    let err = settle_compute_job(
        &mut env,
        &compute,
        &compute.settlement_authority,
        &compute.provider_usdc,
        &compute.usdc_mint,
        MAX_USDC + 1,
        RECEIPT,
    )
    .expect_err("overcharge");
    assert_eq!(custom_error(&err), Some(E_COMPUTE_OVERCHARGE));
    assert_eq!(token_balance(&env, &compute.provider_usdc), 0);
    assert_eq!(token_balance(&env, &compute.client_usdc), 2_000_000);
    assert_eq!(token_balance(&env, &compute.escrow_vault), MAX_USDC);
    assert_eq!(compute_escrow(&env, &compute.escrow).status, COMPUTE_FUNDED);
}

#[test]
fn settlement_rejects_wrong_authority_destination_and_mint() {
    let mut env = boot();
    warp_unix(&mut env, FUNDED_AT);
    let compute = setup_compute_job(&mut env, &JOB, MAX_USDC, EXPIRES_AT);
    let stranger = Keypair::new();

    let err = settle_compute_job(
        &mut env,
        &compute,
        &stranger,
        &compute.provider_usdc,
        &compute.usdc_mint,
        400_000,
        RECEIPT,
    )
    .expect_err("wrong authority");
    assert_eq!(custom_error(&err), Some(E_UNAUTHORIZED));

    let payer = env.payer.insecure_clone();
    let alternate_vault =
        create_token_account(&mut env.svm, &payer, &compute.usdc_mint, &compute.escrow);
    let err = settle_compute_job_with_vault(
        &mut env,
        &compute,
        &compute.settlement_authority,
        &alternate_vault,
        &compute.provider_usdc,
        &compute.usdc_mint,
        400_000,
        RECEIPT,
    )
    .expect_err("escrow vault is immutable");
    assert_eq!(custom_error(&err), Some(E_CONSTRAINT_ADDRESS));

    let alternate_provider_usdc =
        create_token_account(&mut env.svm, &payer, &compute.usdc_mint, &compute.provider);
    let err = settle_compute_job(
        &mut env,
        &compute,
        &compute.settlement_authority,
        &alternate_provider_usdc,
        &compute.usdc_mint,
        400_000,
        RECEIPT,
    )
    .expect_err("provider destination is immutable");
    assert_eq!(custom_error(&err), Some(E_CONSTRAINT_ADDRESS));

    let wrong_mint = create_mint_with_decimals(&mut env.svm, &payer, 6);
    let err = settle_compute_job(
        &mut env,
        &compute,
        &compute.settlement_authority,
        &compute.provider_usdc,
        &wrong_mint,
        400_000,
        RECEIPT,
    )
    .expect_err("wrong payment mint");
    assert_eq!(custom_error(&err), Some(E_WRONG_MINT));
    assert_eq!(token_balance(&env, &compute.escrow_vault), MAX_USDC);
}

#[test]
fn authority_rotation_revokes_the_previous_settler() {
    let mut env = boot();
    warp_unix(&mut env, FUNDED_AT);
    let compute = setup_compute_job(&mut env, &JOB, MAX_USDC, EXPIRES_AT);
    let next_authority = Keypair::new();

    update_compute_settlement_authority(&mut env, &next_authority.pubkey())
        .expect("rotate settlement authority");
    bump_blockhash(&mut env);
    let err = settle_compute_job(
        &mut env,
        &compute,
        &compute.settlement_authority,
        &compute.provider_usdc,
        &compute.usdc_mint,
        400_000,
        RECEIPT,
    )
    .expect_err("previous authority is revoked");
    assert_eq!(custom_error(&err), Some(E_UNAUTHORIZED));

    bump_blockhash(&mut env);
    settle_compute_job(
        &mut env,
        &compute,
        &next_authority,
        &compute.provider_usdc,
        &compute.usdc_mint,
        400_000,
        RECEIPT,
    )
    .expect("rotated authority settles");
    assert_eq!(token_balance(&env, &compute.provider_usdc), 400_000);
}

#[test]
fn authority_failure_refund_is_full_and_idempotent() {
    let mut env = boot();
    warp_unix(&mut env, FUNDED_AT);
    let compute = setup_compute_job(&mut env, &JOB, MAX_USDC, EXPIRES_AT);

    refund_compute_job(&mut env, &compute, &compute.settlement_authority, REFUND)
        .expect("authority failure refund");
    assert_eq!(token_balance(&env, &compute.client_usdc), 3_000_000);
    assert_eq!(token_balance(&env, &compute.escrow_vault), 0);
    let escrow = compute_escrow(&env, &compute.escrow);
    assert_eq!(escrow.status, COMPUTE_REFUNDED);
    assert_eq!(escrow.actual_usdc_amount, 0);
    assert_eq!(escrow.refunded_usdc_amount, MAX_USDC);

    bump_blockhash(&mut env);
    refund_compute_job(&mut env, &compute, &compute.settlement_authority, REFUND)
        .expect("exact refund replay");
    bump_blockhash(&mut env);
    let err = refund_compute_job(
        &mut env,
        &compute,
        &compute.settlement_authority,
        [92u8; 32],
    )
    .expect_err("conflicting refund replay");
    assert_eq!(custom_error(&err), Some(E_COMPUTE_TERMINAL_MISMATCH));
}

#[test]
fn client_refund_requires_expiry_and_returns_full_deposit() {
    let mut env = boot();
    warp_unix(&mut env, FUNDED_AT);
    let compute = setup_compute_job(&mut env, &JOB, MAX_USDC, EXPIRES_AT);
    let client = env.payer.insecure_clone();

    let err = refund_compute_job(&mut env, &compute, &client, REFUND)
        .expect_err("client cannot refund early");
    assert_eq!(custom_error(&err), Some(E_COMPUTE_REFUND_UNAUTHORIZED));
    assert_eq!(token_balance(&env, &compute.escrow_vault), MAX_USDC);

    warp_unix(&mut env, REFUND_OPENS_AT);
    bump_blockhash(&mut env);
    let err = settle_compute_job(
        &mut env,
        &compute,
        &compute.settlement_authority,
        &compute.provider_usdc,
        &compute.usdc_mint,
        400_000,
        RECEIPT,
    )
    .expect_err("expired escrow cannot settle");
    assert_eq!(custom_error(&err), Some(E_COMPUTE_ESCROW_EXPIRED));

    bump_blockhash(&mut env);
    refund_compute_job(&mut env, &compute, &client, REFUND).expect("expired client refund");
    assert_eq!(token_balance(&env, &compute.client_usdc), 3_000_000);
    assert_eq!(token_balance(&env, &compute.escrow_vault), 0);
    assert_eq!(
        compute_escrow(&env, &compute.escrow).status,
        COMPUTE_REFUNDED
    );
}

#[test]
fn expired_client_refund_remains_available_while_paused() {
    let mut env = boot();
    warp_unix(&mut env, FUNDED_AT);
    let compute = setup_compute_job(&mut env, &JOB, MAX_USDC, EXPIRES_AT);
    let client = env.payer.insecure_clone();

    warp_unix(&mut env, REFUND_OPENS_AT);
    set_pause(&mut env, true);
    bump_blockhash(&mut env);
    refund_compute_job(&mut env, &compute, &client, REFUND)
        .expect("paused protocol cannot strand expired client funds");

    assert_eq!(token_balance(&env, &compute.client_usdc), 3_000_000);
    assert_eq!(token_balance(&env, &compute.escrow_vault), 0);
    assert_eq!(
        compute_escrow(&env, &compute.escrow).status,
        COMPUTE_REFUNDED
    );
}

#[test]
fn a_vault_donation_cannot_inflate_the_settlement_refund() {
    let mut env = boot();
    warp_unix(&mut env, FUNDED_AT);
    let compute = setup_compute_job(&mut env, &JOB, MAX_USDC, EXPIRES_AT);
    let payer = env.payer.insecure_clone();
    mint_to(
        &mut env.svm,
        &payer,
        &compute.usdc_mint,
        &compute.escrow_vault,
        500_000,
    );

    settle_compute_job(
        &mut env,
        &compute,
        &compute.settlement_authority,
        &compute.provider_usdc,
        &compute.usdc_mint,
        400_000,
        RECEIPT,
    )
    .expect("settle a donated vault");

    assert_eq!(token_balance(&env, &compute.provider_usdc), 400_000);
    assert_eq!(token_balance(&env, &compute.client_usdc), 2_600_000);
    assert_eq!(token_balance(&env, &compute.escrow_vault), 500_000);
    let escrow = compute_escrow(&env, &compute.escrow);
    assert_eq!(escrow.actual_usdc_amount, 400_000);
    assert_eq!(escrow.refunded_usdc_amount, 600_000);
    assert_eq!(
        escrow.actual_usdc_amount + escrow.refunded_usdc_amount,
        escrow.max_usdc_amount
    );
}

#[test]
fn a_vault_donation_cannot_inflate_a_failure_refund() {
    let mut env = boot();
    warp_unix(&mut env, FUNDED_AT);
    let compute = setup_compute_job(&mut env, &JOB, MAX_USDC, EXPIRES_AT);
    let payer = env.payer.insecure_clone();
    mint_to(
        &mut env.svm,
        &payer,
        &compute.usdc_mint,
        &compute.escrow_vault,
        500_000,
    );

    refund_compute_job(&mut env, &compute, &compute.settlement_authority, REFUND)
        .expect("authority failure refund");

    assert_eq!(token_balance(&env, &compute.client_usdc), 3_000_000);
    assert_eq!(token_balance(&env, &compute.escrow_vault), 500_000);
    let escrow = compute_escrow(&env, &compute.escrow);
    assert_eq!(escrow.refunded_usdc_amount, MAX_USDC);
    assert_eq!(escrow.status, COMPUTE_REFUNDED);
}

#[test]
fn vault_dust_does_not_brick_the_job_id() {
    let mut env = boot();
    warp_unix(&mut env, FUNDED_AT);
    let compute = setup_compute_job_with_dust(&mut env, &JOB, MAX_USDC, EXPIRES_AT, 1);

    assert_eq!(token_balance(&env, &compute.escrow_vault), MAX_USDC + 1);
    assert_eq!(compute_escrow(&env, &compute.escrow).status, COMPUTE_FUNDED);

    settle_compute_job(
        &mut env,
        &compute,
        &compute.settlement_authority,
        &compute.provider_usdc,
        &compute.usdc_mint,
        400_000,
        RECEIPT,
    )
    .expect("settle a dusted vault");
    assert_eq!(token_balance(&env, &compute.provider_usdc), 400_000);
    assert_eq!(token_balance(&env, &compute.client_usdc), 2_600_000);
    assert_eq!(token_balance(&env, &compute.escrow_vault), 1);
}

#[test]
fn escrows_are_scoped_to_their_client() {
    let mut env = boot();
    warp_unix(&mut env, FUNDED_AT);
    let compute = setup_compute_job(&mut env, &JOB, MAX_USDC, EXPIRES_AT);
    let other = Keypair::new();

    bump_blockhash(&mut env);
    let escrow = fund_compute_job_as(&mut env, &compute, &other, &JOB, MAX_USDC, EXPIRES_AT)
        .expect("a second client funds the same job id");

    assert_ne!(escrow, compute.escrow);
    assert_eq!(compute_escrow(&env, &escrow).client, other.pubkey());
    assert_eq!(compute_escrow(&env, &compute.escrow).status, COMPUTE_FUNDED);
}

#[test]
fn funding_requires_a_minimum_settlement_window() {
    let mut env = boot();
    warp_unix(&mut env, FUNDED_AT);
    let compute = setup_compute_job(&mut env, &JOB, MAX_USDC, EXPIRES_AT);
    let other = Keypair::new();

    bump_blockhash(&mut env);
    let err = fund_compute_job_as(
        &mut env,
        &compute,
        &other,
        &OTHER_JOB,
        MAX_USDC,
        FUNDED_AT + MIN_COMPUTE_ESCROW_SECONDS - 1,
    )
    .expect_err("expiry inside the minimum window");
    assert_eq!(custom_error(&err), Some(E_INVALID_COMPUTE_EXPIRY));

    bump_blockhash(&mut env);
    fund_compute_job_as(
        &mut env,
        &compute,
        &other,
        &OTHER_JOB,
        MAX_USDC,
        FUNDED_AT + MIN_COMPUTE_ESCROW_SECONDS,
    )
    .expect("expiry exactly at the minimum window");
}

#[test]
fn settlement_survives_the_grace_window_that_gates_the_client_refund() {
    let mut env = boot();
    warp_unix(&mut env, FUNDED_AT);
    let compute = setup_compute_job(&mut env, &JOB, MAX_USDC, EXPIRES_AT);
    let client = env.payer.insecure_clone();

    warp_unix(&mut env, EXPIRES_AT + 1);
    bump_blockhash(&mut env);
    let err = refund_compute_job(&mut env, &compute, &client, REFUND)
        .expect_err("client refund waits out the grace window");
    assert_eq!(custom_error(&err), Some(E_COMPUTE_REFUND_UNAUTHORIZED));

    bump_blockhash(&mut env);
    settle_compute_job(
        &mut env,
        &compute,
        &compute.settlement_authority,
        &compute.provider_usdc,
        &compute.usdc_mint,
        400_000,
        RECEIPT,
    )
    .expect("late settlement inside the grace window");
    assert_eq!(token_balance(&env, &compute.provider_usdc), 400_000);
    assert_eq!(token_balance(&env, &compute.escrow_vault), 0);
}

#[test]
fn refund_replay_is_idempotent_across_signers() {
    let mut env = boot();
    warp_unix(&mut env, FUNDED_AT);
    let compute = setup_compute_job(&mut env, &JOB, MAX_USDC, EXPIRES_AT);
    let client = env.payer.insecure_clone();

    warp_unix(&mut env, REFUND_OPENS_AT);
    bump_blockhash(&mut env);
    refund_compute_job(&mut env, &compute, &client, REFUND).expect("expired client refund");

    bump_blockhash(&mut env);
    refund_compute_job(&mut env, &compute, &compute.settlement_authority, REFUND)
        .expect("the authority's in-flight refund lands second");

    assert_eq!(token_balance(&env, &compute.client_usdc), 3_000_000);
    assert_eq!(token_balance(&env, &compute.escrow_vault), 0);
    let escrow = compute_escrow(&env, &compute.escrow);
    assert_eq!(escrow.terminal_authority, client.pubkey());
    assert_eq!(escrow.refunded_usdc_amount, MAX_USDC);
}

#[test]
fn authority_rotation_is_blocked_while_paused() {
    let mut env = boot();
    let payer = env.payer.insecure_clone();
    let usdc_mint = create_mint_with_decimals(&mut env.svm, &payer, 6);
    initialize_compute_payments(&mut env, &usdc_mint, &Keypair::new().pubkey())
        .expect("initialize compute payments");

    set_pause(&mut env, true);
    bump_blockhash(&mut env);
    let err = update_compute_settlement_authority(&mut env, &Keypair::new().pubkey())
        .expect_err("rotation while paused");
    assert_eq!(custom_error(&err), Some(E_PROTOCOL_PAUSED));
}

#[test]
fn compute_payment_config_rejects_non_usdc_mint_and_zero_authority() {
    let mut env = boot();
    let settlement_authority = Keypair::new();
    let covnt_mint = env.mint;

    let err = initialize_compute_payments(&mut env, &covnt_mint, &settlement_authority.pubkey())
        .expect_err("zero-decimal COVNT is not USDC");
    assert_eq!(custom_error(&err), Some(E_INVALID_USDC_MINT));

    bump_blockhash(&mut env);
    let payer = env.payer.insecure_clone();
    let usdc_mint = create_mint_with_decimals(&mut env.svm, &payer, 6);
    let err =
        initialize_compute_payments(&mut env, &usdc_mint, &solana_sdk::pubkey::Pubkey::default())
            .expect_err("zero settlement authority");
    assert_eq!(custom_error(&err), Some(E_INVALID_COMPUTE_AUTHORITY));
}
