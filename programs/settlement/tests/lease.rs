mod common;

use anchor_lang::solana_program::hash::hashv;
use common::*;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;

/// One token per second at six decimals, over a ten-minute window.
const RATE: u64 = 1_000_000;
const WINDOW_SECS: u64 = 600;
const WINDOW: u64 = RATE * WINDOW_SECS;
const FUNDING: u64 = 1_000_000_000;
const OPENED_AT: i64 = 1_700_000_000;

fn booted(job_id: [u8; 16]) -> (Env, LeaseCtx) {
    let mut env = boot();
    warp_unix(&mut env, OPENED_AT);
    let lc = lease_setup(&mut env, job_id, FUNDING);
    (env, lc)
}

fn past_the_window(env: &mut Env) {
    warp_unix(env, OPENED_AT + WINDOW_SECS as i64);
}

#[test]
fn a_metered_lease_pays_the_operator_and_refunds_the_rest() {
    let (mut env, lc) = booted([1u8; 16]);
    open_lease(&mut env, &lc, RATE, WINDOW_SECS).expect("open");

    // The whole window is escrowed up front, so metering can only ever split
    // this figure, never add to it.
    assert_eq!(token_balance(&env, &lc.vault), WINDOW);
    assert_eq!(token_balance(&env, &lc.renter_tokens), FUNDING - WINDOW);

    let coordinator = lc.coordinator.insecure_clone();
    tick_lease(&mut env, &lc, &coordinator, 12_500, [9u8; 32]).expect("tick");

    past_the_window(&mut env);
    settle_lease(&mut env, &lc).expect("settle");

    // 12.5 seconds at one token per second, rounded up to the millisecond.
    let charged = 12_500_000;
    assert_eq!(token_balance(&env, &lc.operator_tokens), charged);
    assert_eq!(token_balance(&env, &lc.renter_tokens), FUNDING - charged);
    assert!(account_is_closed(&env, &lc.vault));

    let terms = lease_terms(&env, &lc.terms);
    assert!(terms.paid_operator && terms.paid_renter);
    assert_eq!(terms.funded_amount, WINDOW);
    assert_eq!(terms.mint, lc.mint);
}

#[test]
fn ticks_chain_into_a_replayable_provenance_root() {
    let (mut env, lc) = booted([2u8; 16]);
    open_lease(&mut env, &lc, RATE, WINDOW_SECS).expect("open");
    assert_eq!(lease_meter(&env, &lc.meter).provenance_root, [0u8; 32]);

    let coordinator = lc.coordinator.insecure_clone();
    let first = [3u8; 32];
    let second = [4u8; 32];
    tick_lease(&mut env, &lc, &coordinator, 1_000, first).expect("first tick");
    tick_lease(&mut env, &lc, &coordinator, 2_000, second).expect("second tick");

    let expected = hashv(&[&hashv(&[&[0u8; 32], &first]).to_bytes(), &second]).to_bytes();
    let meter = lease_meter(&env, &lc.meter);
    assert_eq!(meter.provenance_root, expected);
    assert_eq!(meter.metered_ms, 2_000);
}

#[test]
fn a_running_lease_cannot_be_settled() {
    // Without this the renter could settle at zero in the slot after the open
    // and keep a whole session of compute for nothing.
    let (mut env, lc) = booted([3u8; 16]);
    open_lease(&mut env, &lc, RATE, WINDOW_SECS).expect("open");

    let err = settle_lease(&mut env, &lc).expect_err("a live lease must not settle");
    assert_eq!(custom_error(&err), Some(E_LEASE_STILL_RUNNING));
    assert_eq!(token_balance(&env, &lc.vault), WINDOW);
}

#[test]
fn only_the_named_coordinator_can_move_the_meter() {
    let (mut env, lc) = booted([4u8; 16]);
    open_lease(&mut env, &lc, RATE, WINDOW_SECS).expect("open");

    let stranger = Keypair::new();
    env.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    let err = tick_lease(&mut env, &lc, &stranger, 600_000, [1u8; 32])
        .expect_err("a stranger must not drive the meter to the full window");
    assert_eq!(custom_error(&err), Some(E_UNAUTHORIZED));
    assert_eq!(lease_meter(&env, &lc.meter).metered_ms, 0);
}

#[test]
fn the_meter_only_moves_forward() {
    let (mut env, lc) = booted([5u8; 16]);
    open_lease(&mut env, &lc, RATE, WINDOW_SECS).expect("open");

    let coordinator = lc.coordinator.insecure_clone();
    tick_lease(&mut env, &lc, &coordinator, 5_000, [1u8; 32]).expect("tick");
    let err = tick_lease(&mut env, &lc, &coordinator, 4_000, [2u8; 32])
        .expect_err("a meter may not be wound back");
    assert_eq!(custom_error(&err), Some(E_METER_WENT_BACKWARDS));
    assert_eq!(lease_meter(&env, &lc.meter).metered_ms, 5_000);
}

#[test]
fn a_voided_lease_pays_the_operator_nothing() {
    let (mut env, lc) = booted([6u8; 16]);
    open_lease(&mut env, &lc, RATE, WINDOW_SECS).expect("open");

    let coordinator = lc.coordinator.insecure_clone();
    tick_lease(&mut env, &lc, &coordinator, WINDOW_SECS * 1_000, [7u8; 32]).expect("tick");
    void_lease(&mut env, &lc, &coordinator).expect("void");

    bump_blockhash(&mut env);
    let err = void_lease(&mut env, &lc, &coordinator).expect_err("a void is not repeatable");
    assert_eq!(custom_error(&err), Some(E_LEASE_ALREADY_VOIDED));

    // A void also opens settlement immediately: the window has not elapsed.
    settle_lease(&mut env, &lc).expect("settle");
    assert_eq!(token_balance(&env, &lc.operator_tokens), 0);
    assert_eq!(token_balance(&env, &lc.renter_tokens), FUNDING);
}

#[test]
fn only_the_named_coordinator_can_void() {
    let (mut env, lc) = booted([7u8; 16]);
    open_lease(&mut env, &lc, RATE, WINDOW_SECS).expect("open");

    let renter = lc.renter.insecure_clone();
    let err = void_lease(&mut env, &lc, &renter)
        .expect_err("a renter must not be able to cancel their own bill");
    assert_eq!(custom_error(&err), Some(E_UNAUTHORIZED));
    assert!(!lease_terms(&env, &lc.terms).voided);
}

#[test]
fn a_paused_protocol_refuses_a_new_lease() {
    let (mut env, lc) = booted([8u8; 16]);
    set_pause(&mut env, true);

    let err = open_lease(&mut env, &lc, RATE, WINDOW_SECS)
        .expect_err("a paused protocol must not take new escrow");
    assert_eq!(custom_error(&err), Some(E_PROTOCOL_PAUSED));
    assert_eq!(token_balance(&env, &lc.renter_tokens), FUNDING);
}

#[test]
fn a_pause_cannot_strand_escrow_that_is_already_in_the_vault() {
    // The other half of the pause decision: stopping new leases must not stop
    // an open one from paying out, or the switch becomes a freeze on funds.
    let (mut env, lc) = booted([9u8; 16]);
    open_lease(&mut env, &lc, RATE, WINDOW_SECS).expect("open");
    let coordinator = lc.coordinator.insecure_clone();
    tick_lease(&mut env, &lc, &coordinator, 10_000, [1u8; 32]).expect("tick");

    past_the_window(&mut env);
    set_pause(&mut env, true);

    settle_lease(&mut env, &lc).expect("settlement must survive a pause");
    assert_eq!(token_balance(&env, &lc.operator_tokens), 10_000_000);
    assert_eq!(token_balance(&env, &lc.renter_tokens), FUNDING - 10_000_000);
}

#[test]
fn the_renter_refund_leaves_the_operator_share_reserved() {
    let (mut env, lc) = booted([10u8; 16]);
    open_lease(&mut env, &lc, RATE, WINDOW_SECS).expect("open");
    let coordinator = lc.coordinator.insecure_clone();
    tick_lease(&mut env, &lc, &coordinator, 12_500, [1u8; 32]).expect("tick");
    past_the_window(&mut env);

    let charged = 12_500_000;
    claim_renter_refund(&mut env, &lc).expect("renter refund");
    assert_eq!(token_balance(&env, &lc.renter_tokens), FUNDING - charged);
    assert_eq!(token_balance(&env, &lc.vault), charged);

    claim_operator_share(&mut env, &lc).expect("operator share");
    assert_eq!(token_balance(&env, &lc.operator_tokens), charged);
    assert_eq!(token_balance(&env, &lc.vault), 0);

    let err = claim_operator_share(&mut env, &lc).expect_err("a share pays once");
    assert_eq!(custom_error(&err), Some(E_LEASE_SHARE_ALREADY_PAID));
}

#[test]
fn an_unmetered_lease_refunds_the_whole_window() {
    // A lease whose meter never moved, settled after its window: the operator
    // is owed nothing and the renter gets everything back.
    let (mut env, lc) = booted([11u8; 16]);
    open_lease(&mut env, &lc, RATE, WINDOW_SECS).expect("open");
    past_the_window(&mut env);

    settle_lease(&mut env, &lc).expect("settle");
    assert_eq!(token_balance(&env, &lc.operator_tokens), 0);
    assert_eq!(token_balance(&env, &lc.renter_tokens), FUNDING);
}

#[test]
fn an_inflated_meter_cannot_charge_past_the_escrow() {
    let (mut env, lc) = booted([12u8; 16]);
    open_lease(&mut env, &lc, RATE, WINDOW_SECS).expect("open");

    let coordinator = lc.coordinator.insecure_clone();
    tick_lease(&mut env, &lc, &coordinator, u64::MAX, [1u8; 32]).expect("tick");
    past_the_window(&mut env);

    settle_lease(&mut env, &lc).expect("settle");
    assert_eq!(token_balance(&env, &lc.operator_tokens), WINDOW);
    assert_eq!(token_balance(&env, &lc.renter_tokens), FUNDING - WINDOW);
}

#[test]
fn a_lease_longer_than_a_day_is_refused() {
    let (mut env, lc) = booted([13u8; 16]);
    let err = open_lease(&mut env, &lc, 1, 86_401).expect_err("a day is the ceiling");
    assert_eq!(custom_error(&err), Some(E_BAD_DURATION));

    let err = open_lease(&mut env, &lc, 0, WINDOW_SECS).expect_err("a free lease is not a lease");
    assert_eq!(custom_error(&err), Some(E_ZERO_RATE));
    assert_eq!(token_balance(&env, &lc.renter_tokens), FUNDING);
}

#[test]
fn a_settled_lease_does_not_settle_again() {
    let (mut env, lc) = booted([14u8; 16]);
    open_lease(&mut env, &lc, RATE, WINDOW_SECS).expect("open");
    let coordinator = lc.coordinator.insecure_clone();
    tick_lease(&mut env, &lc, &coordinator, 1_000, [1u8; 32]).expect("tick");
    past_the_window(&mut env);

    claim_operator_share(&mut env, &lc).expect("operator share");
    claim_renter_refund(&mut env, &lc).expect("renter refund");

    let err = settle_lease(&mut env, &lc).expect_err("a paid-out lease is finished");
    assert_eq!(custom_error(&err), Some(E_LEASE_ALREADY_SETTLED));
}

#[test]
fn a_lease_does_not_touch_the_covnt_subsystem() {
    // The two subsystems share only `Config.paused`. A lease that opens, meters
    // and settles must leave the COVNT treasury exactly where it was.
    let (mut env, lc) = booted([15u8; 16]);
    let treasury_before = token_balance(&env, &env.treasury);

    open_lease(&mut env, &lc, RATE, WINDOW_SECS).expect("open");
    let coordinator = lc.coordinator.insecure_clone();
    tick_lease(&mut env, &lc, &coordinator, 30_000, [1u8; 32]).expect("tick");
    past_the_window(&mut env);
    settle_lease(&mut env, &lc).expect("settle");

    assert_eq!(token_balance(&env, &env.treasury), treasury_before);
    let config = config_account(&env);
    assert_eq!(config.covnt_mint, env.mint);
    assert_ne!(config.covnt_mint, lc.mint);
}
