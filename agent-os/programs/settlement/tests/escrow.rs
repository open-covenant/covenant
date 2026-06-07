//! Task-escrow state machine (optimistic release + provider claim + arbiter
//! dispute resolution). The whole file is gated on the `task-escrow` feature,
//! so the default build compiles it to nothing.
#![cfg(feature = "task-escrow")]

mod common;

use common::*;
use solana_sdk::signer::{keypair::Keypair, Signer};

const AGENT: [u8; 32] = [11u8; 32];

fn provider_account(env: &mut Env, provider: &Keypair) -> solana_sdk::pubkey::Pubkey {
    create_token_account(
        &mut env.svm,
        &env.payer.insecure_clone(),
        &env.mint,
        &provider.pubkey(),
    )
}

/// The core fix: a silent client cannot strand a delivered result. The
/// provider submits, the challenge window elapses with no dispute, and the
/// provider claims the escrow itself.
#[test]
fn submit_then_claim_after_window_pays_provider() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    let task_id = [51u8; 32];
    let provider = Keypair::new();
    let provider_covnt = provider_account(&mut env, &provider);

    let tc = task_setup(&mut env, &task_id, 1_000);
    create_task(&mut env, &AGENT, &task_id, &provider.pubkey(), 600, 100_000, 3_600, &tc)
        .expect("create");

    submit_result(&mut env, &tc, &provider, [9u8; 32]).expect("submit");
    assert_eq!(task_status(&env, &tc.task), ST_SUBMITTED);

    // Within the window, the provider cannot claim yet.
    bump_blockhash(&mut env);
    let err = claim_task(&mut env, &tc, &provider, &provider_covnt)
        .expect_err("claim before window must fail");
    assert_eq!(custom_error(&err), Some(E_CHALLENGE_WINDOW_OPEN));

    // After the window elapses with no dispute, the provider claims. Fresh
    // blockhash so this isn't deduped against the rejected early-claim tx.
    warp_unix(&mut env, 4_000); // > submitted_at(0) + 3_600
    bump_blockhash(&mut env);
    claim_task(&mut env, &tc, &provider, &provider_covnt).expect("claim");
    assert_eq!(token_balance(&env, &provider_covnt), 600);
    assert_eq!(token_balance(&env, &tc.escrow_vault), 0);
    assert_eq!(task_status(&env, &tc.task), ST_RELEASED);
}

/// Client disputes within the window; the arbiter resolves in the provider's
/// favor. A disputing client cannot unilaterally claw back funds.
#[test]
fn dispute_then_resolve_pays_provider() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    let task_id = [52u8; 32];
    let provider = Keypair::new();
    let provider_covnt = provider_account(&mut env, &provider);

    let tc = task_setup(&mut env, &task_id, 1_000);
    create_task(&mut env, &AGENT, &task_id, &provider.pubkey(), 600, 100_000, 3_600, &tc)
        .expect("create");
    submit_result(&mut env, &tc, &provider, [9u8; 32]).expect("submit");

    dispute_task(&mut env, &tc).expect("dispute within window");
    assert_eq!(task_status(&env, &tc.task), ST_DISPUTED);

    // While disputed, the provider cannot claim.
    bump_blockhash(&mut env);
    let err = claim_task(&mut env, &tc, &provider, &provider_covnt)
        .expect_err("claim while disputed must fail");
    assert_eq!(custom_error(&err), Some(E_WRONG_TASK_STATUS));

    resolve_task(&mut env, &tc, &provider_covnt, true).expect("arbiter pays provider");
    assert_eq!(token_balance(&env, &provider_covnt), 600);
    assert_eq!(token_balance(&env, &tc.escrow_vault), 0);
    assert_eq!(task_status(&env, &tc.task), ST_RELEASED);
}

/// Same dispute, but the arbiter refunds the client.
#[test]
fn dispute_then_resolve_refunds_client() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    let task_id = [53u8; 32];
    let provider = Keypair::new();
    let provider_covnt = provider_account(&mut env, &provider);

    let tc = task_setup(&mut env, &task_id, 1_000);
    create_task(&mut env, &AGENT, &task_id, &provider.pubkey(), 600, 100_000, 3_600, &tc)
        .expect("create");
    submit_result(&mut env, &tc, &provider, [9u8; 32]).expect("submit");
    dispute_task(&mut env, &tc).expect("dispute");

    resolve_task(&mut env, &tc, &provider_covnt, false).expect("arbiter refunds client");
    assert_eq!(token_balance(&env, &tc.client_covnt), 1_000); // 400 change + 600 refund
    assert_eq!(token_balance(&env, &provider_covnt), 0);
    assert_eq!(token_balance(&env, &tc.escrow_vault), 0);
    assert_eq!(task_status(&env, &tc.task), ST_REFUNDED);
}

/// Client may release voluntarily and early, straight from SUBMITTED, without
/// waiting out the challenge window.
#[test]
fn client_release_from_submitted_is_allowed() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    let task_id = [54u8; 32];
    let provider = Keypair::new();
    let provider_covnt = provider_account(&mut env, &provider);

    let tc = task_setup(&mut env, &task_id, 1_000);
    create_task(&mut env, &AGENT, &task_id, &provider.pubkey(), 600, 100_000, 3_600, &tc)
        .expect("create");
    submit_result(&mut env, &tc, &provider, [9u8; 32]).expect("submit");

    release_task(&mut env, &tc, &provider_covnt).expect("client releases early");
    assert_eq!(token_balance(&env, &provider_covnt), 600);
    assert_eq!(task_status(&env, &tc.task), ST_RELEASED);
}

#[test]
fn dispute_after_window_rejected() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    let task_id = [55u8; 32];
    let provider = Keypair::new();

    let tc = task_setup(&mut env, &task_id, 1_000);
    create_task(&mut env, &AGENT, &task_id, &provider.pubkey(), 600, 100_000, 3_600, &tc)
        .expect("create");
    submit_result(&mut env, &tc, &provider, [9u8; 32]).expect("submit");

    warp_unix(&mut env, 4_000); // past submitted_at(0) + 3_600
    let err = dispute_task(&mut env, &tc).expect_err("dispute after window must fail");
    assert_eq!(custom_error(&err), Some(E_CHALLENGE_WINDOW_ELAPSED));
}

#[test]
fn submit_by_non_provider_rejected() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    let task_id = [56u8; 32];
    let provider = Keypair::new();
    let imposter = Keypair::new();

    let tc = task_setup(&mut env, &task_id, 1_000);
    create_task(&mut env, &AGENT, &task_id, &provider.pubkey(), 600, 100_000, 3_600, &tc)
        .expect("create");
    let err = submit_result(&mut env, &tc, &imposter, [9u8; 32])
        .expect_err("only the named provider may submit");
    assert_eq!(custom_error(&err), Some(E_PROVIDER_MISMATCH));
}

#[test]
fn submit_after_deadline_rejected() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    let task_id = [57u8; 32];
    let provider = Keypair::new();

    let tc = task_setup(&mut env, &task_id, 1_000);
    create_task(&mut env, &AGENT, &task_id, &provider.pubkey(), 600, 5_000, 3_600, &tc)
        .expect("create");
    warp_unix(&mut env, 6_000); // past deadline
    let err = submit_result(&mut env, &tc, &provider, [9u8; 32])
        .expect_err("submit after deadline must fail");
    assert_eq!(custom_error(&err), Some(E_TASK_EXPIRED));
}

#[test]
fn create_task_invalid_challenge_window_rejected() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    let task_id = [58u8; 32];
    let provider = Keypair::new();
    let tc = task_setup(&mut env, &task_id, 1_000);
    let err = create_task(&mut env, &AGENT, &task_id, &provider.pubkey(), 600, 100_000, 0, &tc)
        .expect_err("zero challenge window must fail");
    assert_eq!(custom_error(&err), Some(E_INVALID_CHALLENGE_WINDOW));
    assert_eq!(token_balance(&env, &tc.client_covnt), 1_000); // no funds moved
}

#[test]
fn create_task_past_deadline_rejected() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    let task_id = [59u8; 32];
    let provider = Keypair::new();
    warp_unix(&mut env, 10_000);
    let tc = task_setup(&mut env, &task_id, 1_000);
    let err = create_task(&mut env, &AGENT, &task_id, &provider.pubkey(), 600, 5_000, 3_600, &tc)
        .expect_err("past deadline must fail");
    assert_eq!(custom_error(&err), Some(E_INVALID_DEADLINE));
}
