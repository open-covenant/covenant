mod common;

use common::*;
use solana_sdk::signer::Signer;

const AGENT: [u8; 32] = [7u8; 32];

#[test]
fn slash_stake_succeeds_when_not_paused() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    let (position, stake_vault) = stake(&mut env, &AGENT, 1_000);
    let slash_vault = create_token_account(
        &mut env.svm,
        &env.payer.insecure_clone(),
        &env.mint,
        &env.payer.pubkey(),
    );

    slash_stake(&mut env, &AGENT, &position, &stake_vault, &slash_vault, 400)
        .expect("slash should succeed while unpaused");

    assert_eq!(token_balance(&env, &stake_vault), 600);
    assert_eq!(token_balance(&env, &slash_vault), 400);
}

#[test]
fn slash_stake_rejected_when_paused() {
    let mut env = boot();
    register_agent(&mut env, &AGENT);
    let (position, stake_vault) = stake(&mut env, &AGENT, 1_000);
    let slash_vault = create_token_account(
        &mut env.svm,
        &env.payer.insecure_clone(),
        &env.mint,
        &env.payer.pubkey(),
    );

    set_pause(&mut env, true);

    let err = slash_stake(&mut env, &AGENT, &position, &stake_vault, &slash_vault, 400)
        .expect_err("slash must be blocked during a protocol pause");
    assert_eq!(custom_error(&err), Some(E_PROTOCOL_PAUSED));

    // stake untouched: pause halted the transfer before any token movement.
    assert_eq!(token_balance(&env, &stake_vault), 1_000);
    assert_eq!(token_balance(&env, &slash_vault), 0);
}
