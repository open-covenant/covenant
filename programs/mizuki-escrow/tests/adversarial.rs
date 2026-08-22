use crate::{
    instruction::{EscrowInstruction, ResolveArgs},
    state::EscrowStatus,
    test_common::*,
};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    signature::Signer,
};

#[test]
fn claimant_cannot_release_refund_or_replace_authority() {
    let mut env = boot();
    fund(&mut env).unwrap();
    bind(&mut env).unwrap();

    let claimant = env.claimant.insecure_clone();
    let release = release_ix(claimant.pubkey(), BOUNTY, claimant.pubkey());
    assert!(send(&mut env.svm, &env.payer, &[release], &[&claimant]).is_err());

    warp(&mut env.svm, CLAIM_EXPIRY);
    let refund = refund_ix(claimant.pubkey(), BOUNTY);
    assert!(send(&mut env.svm, &env.payer, &[refund], &[&claimant]).is_err());
    assert_eq!(state(&env).status, EscrowStatus::Bound);
}

#[test]
fn authority_signature_is_required_even_when_fee_payer_signed() {
    let mut env = boot();
    fund(&mut env).unwrap();
    bind(&mut env).unwrap();
    let mut ix = release_ix(env.authority.pubkey(), BOUNTY, env.claimant.pubkey());
    ix.accounts[0] = AccountMeta::new_readonly(env.authority.pubkey(), false);
    assert!(send(&mut env.svm, &env.payer, &[ix], &[]).is_err());
    assert_eq!(state(&env).status, EscrowStatus::Bound);
}

#[test]
fn claimant_destination_is_immutable() {
    let mut env = boot();
    fund(&mut env).unwrap();
    bind(&mut env).unwrap();
    let ix = release_ix(env.authority.pubkey(), BOUNTY, env.stranger.pubkey());
    assert!(send(&mut env.svm, &env.payer, &[ix], &[&env.authority]).is_err());
    assert_eq!(state(&env).status, EscrowStatus::Bound);
}

#[test]
fn a_bounty_can_never_bind_a_second_claimant() {
    let mut env = boot();
    fund(&mut env).unwrap();
    bind(&mut env).unwrap();
    let second = bind_ix(
        env.authority.pubkey(),
        BOUNTY,
        env.stranger.pubkey(),
        CLAIM_EXPIRY + 1,
    );
    assert!(send(
        &mut env.svm,
        &env.payer,
        std::slice::from_ref(&second),
        &[&env.authority]
    )
    .is_err());

    warp(&mut env.svm, CLAIM_EXPIRY);
    let refund = refund_ix(env.authority.pubkey(), BOUNTY);
    send(&mut env.svm, &env.payer, &[refund], &[&env.authority]).unwrap();
    assert!(send(&mut env.svm, &env.payer, &[second], &[&env.authority]).is_err());
    assert_eq!(guard(&env).status, EscrowStatus::Refunded);
}

#[test]
fn terminal_resolution_is_exactly_once() {
    let mut env = boot();
    fund(&mut env).unwrap();
    bind(&mut env).unwrap();
    let release = release_ix(env.authority.pubkey(), BOUNTY, env.claimant.pubkey());
    send(
        &mut env.svm,
        &env.payer,
        std::slice::from_ref(&release),
        &[&env.authority],
    )
    .unwrap();
    env.svm.expire_blockhash();
    assert!(send(&mut env.svm, &env.payer, &[release], &[&env.authority]).is_err());
    warp(&mut env.svm, CLAIM_EXPIRY);
    let refund = refund_ix(env.authority.pubkey(), BOUNTY);
    assert!(send(&mut env.svm, &env.payer, &[refund], &[&env.authority]).is_err());
    assert_eq!(guard(&env).status, EscrowStatus::Released);
}

#[test]
fn wrong_bounty_state_vault_and_extra_accounts_are_rejected() {
    let mut env = boot();
    fund(&mut env).unwrap();
    bind(&mut env).unwrap();

    let wrong_bounty = [55; 32];
    let wrong = release_ix(env.authority.pubkey(), wrong_bounty, env.claimant.pubkey());
    assert!(send(&mut env.svm, &env.payer, &[wrong], &[&env.authority]).is_err());

    let mut wrong_vault = release_ix(env.authority.pubkey(), BOUNTY, env.claimant.pubkey());
    wrong_vault.accounts[2] = AccountMeta::new(env.stranger.pubkey(), false);
    assert!(send(&mut env.svm, &env.payer, &[wrong_vault], &[&env.authority]).is_err());

    let mut extra = release_ix(env.authority.pubkey(), BOUNTY, env.claimant.pubkey());
    extra
        .accounts
        .push(AccountMeta::new_readonly(env.stranger.pubkey(), false));
    assert!(send(&mut env.svm, &env.payer, &[extra], &[&env.authority]).is_err());
    assert_eq!(state(&env).status, EscrowStatus::Bound);
}

#[test]
fn malformed_data_and_zero_evidence_are_rejected() {
    let mut env = boot();
    fund(&mut env).unwrap();
    bind(&mut env).unwrap();
    let mut ix = release_ix(env.authority.pubkey(), BOUNTY, env.claimant.pubkey());
    ix.data.push(0);
    assert!(send(&mut env.svm, &env.payer, &[ix], &[&env.authority]).is_err());

    let mut zero = release_ix(env.authority.pubkey(), BOUNTY, env.claimant.pubkey());
    zero.data = EscrowInstruction::Release(ResolveArgs {
        bounty_id: BOUNTY,
        resolution_evidence: [0; 32],
    })
    .encode();
    assert!(send(&mut env.svm, &env.payer, &[zero], &[&env.authority]).is_err());

    let unknown = Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![],
        data: vec![4],
    };
    assert!(send(&mut env.svm, &env.payer, &[unknown], &[]).is_err());
    assert_eq!(state(&env).status, EscrowStatus::Bound);
}

#[test]
fn fund_rejects_expired_offer_wrong_bump_and_reinitialization() {
    let mut env = boot();
    let expired = fund_ix(env.authority.pubkey(), BOUNTY, NOW);
    assert!(send(&mut env.svm, &env.payer, &[expired], &[&env.authority]).is_err());

    let mut wrong_bump = fund_ix(env.authority.pubkey(), BOUNTY, OFFER_EXPIRY);
    let last = wrong_bump.data.len() - 1;
    wrong_bump.data[last] ^= 1;
    assert!(send(&mut env.svm, &env.payer, &[wrong_bump], &[&env.authority]).is_err());

    fund(&mut env).unwrap();
    let replay = fund_ix(env.authority.pubkey(), BOUNTY, OFFER_EXPIRY);
    assert!(send(&mut env.svm, &env.payer, &[replay], &[&env.authority]).is_err());
    assert_eq!(guard(&env).status, EscrowStatus::Funded);
}

#[test]
fn bind_rejects_expired_offer_and_reserved_destinations() {
    let mut env = boot();
    fund(&mut env).unwrap();
    let state_address = state_pda(&env.authority.pubkey(), &BOUNTY).0;
    let vault_address = vault(&env);
    for invalid in [env.authority.pubkey(), state_address, vault_address] {
        let ix = bind_ix(env.authority.pubkey(), BOUNTY, invalid, CLAIM_EXPIRY);
        assert!(send(&mut env.svm, &env.payer, &[ix], &[&env.authority]).is_err());
    }

    warp(&mut env.svm, OFFER_EXPIRY);
    let expired = bind_ix(
        env.authority.pubkey(),
        BOUNTY,
        env.claimant.pubkey(),
        CLAIM_EXPIRY,
    );
    assert!(send(&mut env.svm, &env.payer, &[expired], &[&env.authority]).is_err());
    assert_eq!(state(&env).status, EscrowStatus::Funded);
}

#[test]
fn insufficient_principal_never_marks_escrow_terminal() {
    let mut env = boot();
    fund(&mut env).unwrap();
    bind(&mut env).unwrap();
    let vault_address = vault(&env);
    let mut account = env.svm.get_account(&vault_address).unwrap();
    account.lamports = AMOUNT - 1;
    env.svm.set_account(vault_address, account).unwrap();

    let ix = release_ix(env.authority.pubkey(), BOUNTY, env.claimant.pubkey());
    assert!(send(&mut env.svm, &env.payer, &[ix], &[&env.authority]).is_err());
    assert_eq!(state(&env).status, EscrowStatus::Bound);
    assert_eq!(guard(&env).status, EscrowStatus::Bound);
}
