mod common;

use common::*;
use covenant_settlement_program::ID;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::signer::{keypair::Keypair, Signer};

const AGENT: [u8; 32] = [13u8; 32];

#[test]
fn buy_zero_rejected() {
    let mut env = boot();
    let credits = open_credit_account(&mut env);
    let owner_covnt = funded_covnt(&mut env, 1_000);
    let err = buy_credits(&mut env, &credits, &owner_covnt, 0).expect_err("zero buy");
    assert_eq!(custom_error(&err), Some(E_ZERO_AMOUNT));
    assert_eq!(token_balance(&env, &owner_covnt), 1_000);
}

#[test]
fn consume_more_than_balance_rejected() {
    let mut env = boot();
    let credits = open_credit_account(&mut env);
    let owner_covnt = funded_covnt(&mut env, 1_000);
    buy_credits(&mut env, &credits, &owner_covnt, 5).expect("buy"); // 50 credits
    let err = consume_credits(&mut env, &credits, 51).expect_err("overspend");
    assert_eq!(custom_error(&err), Some(E_INSUFFICIENT_CREDITS));
    assert_eq!(credit_balance(&env, &credits), 50);
}

#[test]
fn unstake_before_lock_expiry_rejected() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    let (position, stake_vault, owner_covnt) = stake_locked(&mut env, &AGENT, 1_000, 10_000_000);
    let err = unstake(&mut env, &AGENT, &position, &stake_vault, &owner_covnt)
        .expect_err("locked stake cannot be withdrawn");
    assert_eq!(custom_error(&err), Some(E_STAKE_LOCKED));
    assert_eq!(token_balance(&env, &stake_vault), 1_000);
}

#[test]
fn slash_more_than_staked_rejected() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    let (position, stake_vault, _) = stake_locked(&mut env, &AGENT, 500, 0);
    let slash_vault = create_token_account(
        &mut env.svm,
        &env.payer.insecure_clone(),
        &env.mint,
        &env.payer.pubkey(),
    );
    let err = slash_stake(&mut env, &AGENT, &position, &stake_vault, &slash_vault, 600)
        .expect_err("cannot slash beyond the position");
    assert_eq!(custom_error(&err), Some(E_INSUFFICIENT_STAKE));
    assert_eq!(token_balance(&env, &stake_vault), 500);
}

#[test]
fn close_active_position_rejected() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    let (position, _, _) = stake_locked(&mut env, &AGENT, 500, 0);
    let err = close_position(&mut env, &position).expect_err("active position cannot be closed");
    assert_eq!(custom_error(&err), Some(E_STAKE_STILL_ACTIVE));
}

// Default build: the escrow is compiled but hard-disabled, so create_task
// reverts before any token movement.
#[cfg(not(feature = "task-escrow"))]
#[test]
fn create_task_rejected_when_escrow_disabled() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    let task_id = [44u8; 32];
    let provider = Keypair::new().pubkey();
    let tc = task_setup(&mut env, &task_id, 1_000);
    let err = create_task(&mut env, &AGENT, &task_id, &provider, 600, 10_000, &tc)
        .expect_err("create_task must be disabled without the task-escrow feature");
    assert_eq!(custom_error(&err), Some(E_TASKS_DISABLED));
    assert_eq!(token_balance(&env, &tc.client_covnt), 1_000); // no funds moved
}

#[cfg(feature = "task-escrow")]
#[test]
fn release_after_deadline_rejected() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    let task_id = [41u8; 32];
    let provider = Keypair::new().pubkey();
    let provider_covnt =
        create_token_account(&mut env.svm, &env.payer.insecure_clone(), &env.mint, &provider);
    let tc = task_setup(&mut env, &task_id, 1_000);
    create_task(&mut env, &AGENT, &task_id, &provider, 600, 5_000, &tc).expect("create");
    warp_unix(&mut env, 6_000);
    let err = release_task(&mut env, &tc, &provider_covnt).expect_err("release past deadline");
    assert_eq!(custom_error(&err), Some(E_TASK_EXPIRED));
    assert_eq!(token_balance(&env, &tc.escrow_vault), 600);
}

#[cfg(feature = "task-escrow")]
#[test]
fn refund_before_deadline_rejected() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    let task_id = [42u8; 32];
    let provider = Keypair::new().pubkey();
    let tc = task_setup(&mut env, &task_id, 1_000);
    create_task(&mut env, &AGENT, &task_id, &provider, 600, 10_000, &tc).expect("create");
    let err = refund_task(&mut env, &tc).expect_err("refund before deadline");
    assert_eq!(custom_error(&err), Some(E_TASK_NOT_EXPIRED));
}

#[cfg(feature = "task-escrow")]
#[test]
fn double_release_rejected() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    let task_id = [43u8; 32];
    let provider = Keypair::new().pubkey();
    let provider_covnt =
        create_token_account(&mut env.svm, &env.payer.insecure_clone(), &env.mint, &provider);
    let tc = task_setup(&mut env, &task_id, 1_000);
    create_task(&mut env, &AGENT, &task_id, &provider, 600, 10_000, &tc).expect("create");
    release_task(&mut env, &tc, &provider_covnt).expect("first release");
    bump_blockhash(&mut env); // distinct signature for the retry
    let err = release_task(&mut env, &tc, &provider_covnt).expect_err("second release");
    assert_eq!(custom_error(&err), Some(E_WRONG_TASK_STATUS));
}

// has_one = authority gate: a stranger signing an admin update is rejected.
#[test]
fn update_authority_by_stranger_rejected() {
    let mut env = boot();
    let stranger = Keypair::new();
    let data = covenant_settlement_program::instruction::UpdateAuthority {
        new_authority: stranger.pubkey(),
    };
    use anchor_lang::InstructionData;
    let metas = vec![
        AccountMeta::new(env.config, false),
        AccountMeta::new_readonly(stranger.pubkey(), true),
    ];
    let payer = env.payer.insecure_clone();
    let err = send(
        &mut env.svm,
        &payer,
        &[Instruction { program_id: ID, accounts: metas, data: data.data() }],
        &[&stranger],
    )
    .expect_err("stranger cannot rotate authority");
    assert_eq!(custom_error(&err), Some(E_UNAUTHORIZED));
}
