mod common;

use anchor_lang::{Discriminator, InstructionData};
use common::*;
use covenant_settlement_program::{
    instruction as ix, vote_message, VoteRound, ID, MAX_ROUND_VOTES, ROUND_OPEN, VOTE_FAIL,
    VOTE_PASS,
};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_program, sysvar,
    transaction::TransactionError,
};

const E_ROUND_CLOSED: u32 = 6029;
const E_ROUND_FULL: u32 = 6030;
const E_ALREADY_VOTED: u32 = 6031;
const E_VOTE_NOT_SIGNED: u32 = 6032;

const TASK: [u8; 16] = [4u8; 16];
const PATCH: [u8; 32] = [8u8; 32];

struct Round {
    coordinator: Keypair,
    validator: Pubkey,
    address: Pubkey,
}

fn round_pda(coordinator: &Pubkey, task_id: &[u8; 16]) -> Pubkey {
    Pubkey::find_program_address(&[b"round", coordinator.as_ref(), task_id], &ID).0
}

fn new_round() -> Round {
    let coordinator = Keypair::new();
    let address = round_pda(&coordinator.pubkey(), &TASK);
    Round {
        coordinator,
        validator: Pubkey::new_unique(),
        address,
    }
}

fn open_round_at(
    env: &mut Env,
    round: &Round,
    signer: &Keypair,
    address: Pubkey,
) -> Result<(), TransactionError> {
    let data = ix::OpenRound {
        task_id: TASK,
        patch_sha256: PATCH,
    }
    .data();
    let metas = vec![
        AccountMeta::new(env.payer.pubkey(), true),
        AccountMeta::new_readonly(signer.pubkey(), true),
        AccountMeta::new_readonly(round.validator, false),
        AccountMeta::new(address, false),
        AccountMeta::new_readonly(system_program::ID, false),
    ];
    let payer = env.payer.insecure_clone();
    send(
        &mut env.svm,
        &payer,
        &[Instruction {
            program_id: ID,
            accounts: metas,
            data,
        }],
        &[signer],
    )
}

fn open_round(env: &mut Env, round: &Round) -> Result<(), TransactionError> {
    let coordinator = round.coordinator.insecure_clone();
    open_round_at(env, round, &coordinator, round.address)
}

/// What the Ed25519 program checks, laid out as the web3 client lays it out:
/// header, key, signature, message, every offset pointing into this data.
fn signature_check(voter: &Pubkey, signature: &[u8; 64], message: &[u8]) -> Instruction {
    let here = u16::MAX;
    let mut data = vec![1u8, 0];
    // signature, key, message offsets, each with its instruction index
    for field in [48, here, 16, here, 112, message.len() as u16, here] {
        data.extend_from_slice(&field.to_le_bytes());
    }
    data.extend_from_slice(voter.as_ref());
    data.extend_from_slice(signature);
    data.extend_from_slice(message);
    Instruction {
        program_id: solana_sdk::ed25519_program::ID,
        accounts: vec![],
        data,
    }
}

fn signed_check(voter: &Keypair, message: &[u8]) -> Instruction {
    let signature: [u8; 64] = voter.sign_message(message).into();
    signature_check(&voter.pubkey(), &signature, message)
}

fn record_vote(round: &Round, signer: &Keypair, passed: bool) -> Instruction {
    Instruction {
        program_id: ID,
        accounts: vec![
            AccountMeta::new(round.address, false),
            AccountMeta::new_readonly(signer.pubkey(), true),
            AccountMeta::new_readonly(sysvar::instructions::ID, false),
        ],
        data: ix::RecordVote { passed }.data(),
    }
}

fn send_as(env: &mut Env, signer: &Keypair, ixs: &[Instruction]) -> Result<(), TransactionError> {
    let payer = env.payer.insecure_clone();
    let signer = signer.insecure_clone();
    let result = send(&mut env.svm, &payer, ixs, &[&signer]);
    bump_blockhash(env);
    result
}

fn vote(
    env: &mut Env,
    round: &Round,
    voter: &Keypair,
    passed: bool,
) -> Result<(), TransactionError> {
    let check = signed_check(voter, &vote_message(&TASK, &PATCH, passed));
    let record = record_vote(round, &round.coordinator, passed);
    let coordinator = round.coordinator.insecure_clone();
    send_as(env, &coordinator, &[check, record])
}

fn settle(
    env: &mut Env,
    round: &Round,
    signer: &Keypair,
    payer: Pubkey,
) -> Result<(), TransactionError> {
    let data = ix::SettleRound {}.data();
    let metas = vec![
        AccountMeta::new(round.address, false),
        AccountMeta::new_readonly(signer.pubkey(), true),
        AccountMeta::new(payer, false),
    ];
    send_as(
        env,
        signer,
        &[Instruction {
            program_id: ID,
            accounts: metas,
            data,
        }],
    )
}

fn read_round(env: &Env, address: &Pubkey) -> VoteRound {
    let account = env.svm.get_account(address).expect("round exists");
    assert_eq!(account.owner, ID);
    assert_eq!(&account.data[..8], VoteRound::DISCRIMINATOR);
    *bytemuck::from_bytes::<VoteRound>(&account.data[8..])
}

fn code(result: Result<(), TransactionError>) -> Option<u32> {
    custom_error(&result.expect_err("should be refused"))
}

#[test]
fn a_round_records_signed_votes_and_settles_with_its_rent_back() {
    let mut env = boot();
    let round = new_round();
    open_round(&mut env, &round).expect("open");

    let opened = read_round(&env, &round.address);
    assert_eq!(opened.coordinator, round.coordinator.pubkey());
    assert_eq!(opened.er_validator, round.validator);
    assert_eq!(opened.payer, env.payer.pubkey());
    assert_eq!((opened.task_id, opened.patch_sha256), (TASK, PATCH));
    assert_eq!((opened.count, opened.result), (0, ROUND_OPEN));

    let (a, b) = (Keypair::new(), Keypair::new());
    vote(&mut env, &round, &a, true).expect("first vote");
    vote(&mut env, &round, &b, false).expect("second vote");

    let voted = read_round(&env, &round.address);
    assert_eq!(voted.count, 2);
    assert_eq!(&voted.voters[..2], &[a.pubkey(), b.pubkey()]);
    assert_eq!(&voted.votes[..2], &[VOTE_PASS, VOTE_FAIL]);
    // The stored signature is the voter's own, so anyone can recount.
    let pass: [u8; 64] = a.sign_message(&vote_message(&TASK, &PATCH, true)).into();
    assert_eq!(voted.signatures[0], pass);

    let rent = env.svm.get_account(&round.address).unwrap().lamports;
    let payer = env.payer.pubkey();
    let before = env.svm.get_balance(&payer).unwrap();
    let coordinator = round.coordinator.insecure_clone();
    settle(&mut env, &round, &coordinator, payer).expect("settle");
    assert!(account_is_closed(&env, &round.address));
    let after = env.svm.get_balance(&payer).unwrap();
    // The payer also pays this transaction's fee.
    assert!(after > before && after <= before + rent);
}

#[test]
fn a_forged_signature_does_not_vote() {
    let mut env = boot();
    let round = new_round();
    open_round(&mut env, &round).unwrap();
    let voter = Keypair::new();
    let message = vote_message(&TASK, &PATCH, true);
    let mut signature: [u8; 64] = voter.sign_message(&message).into();
    signature[0] ^= 1;
    let check = signature_check(&voter.pubkey(), &signature, &message);
    let record = record_vote(&round, &round.coordinator, true);
    let coordinator = round.coordinator.insecure_clone();
    // The precompile itself refuses it, before the program runs.
    assert_eq!(
        send_as(&mut env, &coordinator, &[check, record]),
        Err(TransactionError::InstructionError(
            0,
            solana_sdk::instruction::InstructionError::Custom(2)
        ))
    );
    assert_eq!(read_round(&env, &round.address).count, 0);
}

#[test]
fn a_signature_on_the_opposite_verdict_does_not_count() {
    let mut env = boot();
    let round = new_round();
    open_round(&mut env, &round).unwrap();
    let voter = Keypair::new();
    let check = signed_check(&voter, &vote_message(&TASK, &PATCH, true));
    let record = record_vote(&round, &round.coordinator, false);
    let coordinator = round.coordinator.insecure_clone();
    assert_eq!(
        code(send_as(&mut env, &coordinator, &[check, record])),
        Some(E_VOTE_NOT_SIGNED)
    );
}

#[test]
fn a_vote_signed_for_another_patch_or_task_does_not_count() {
    let mut env = boot();
    let round = new_round();
    open_round(&mut env, &round).unwrap();
    let voter = Keypair::new();
    let coordinator = round.coordinator.insecure_clone();
    for message in [
        vote_message(&TASK, &[9u8; 32], true),
        vote_message(&[5u8; 16], &PATCH, true),
    ] {
        let check = signed_check(&voter, &message);
        let record = record_vote(&round, &round.coordinator, true);
        assert_eq!(
            code(send_as(&mut env, &coordinator, &[check, record])),
            Some(E_VOTE_NOT_SIGNED)
        );
    }
}

#[test]
fn a_vote_needs_its_signature_check_directly_before_it() {
    let mut env = boot();
    let round = new_round();
    open_round(&mut env, &round).unwrap();
    let coordinator = round.coordinator.insecure_clone();

    let alone = record_vote(&round, &round.coordinator, true);
    assert_eq!(
        code(send_as(&mut env, &coordinator, &[alone])),
        Some(E_VOTE_NOT_SIGNED)
    );

    // One check cannot carry two votes: the second follows the first vote,
    // not a signature check.
    let voter = Keypair::new();
    let check = signed_check(&voter, &vote_message(&TASK, &PATCH, true));
    let first = record_vote(&round, &round.coordinator, true);
    let second = record_vote(&round, &round.coordinator, true);
    assert_eq!(
        code(send_as(&mut env, &coordinator, &[check, first, second])),
        Some(E_VOTE_NOT_SIGNED)
    );
}

#[test]
fn a_signature_check_over_other_bytes_is_refused() {
    let mut env = boot();
    let round = new_round();
    open_round(&mut env, &round).unwrap();
    let voter = Keypair::new();
    let message = vote_message(&TASK, &PATCH, true);
    let coordinator = round.coordinator.insecure_clone();

    // The check verifies a signature over bytes held by instruction 0 (here
    // itself, so the precompile passes), and the program refuses anything not
    // carried in the check's own data.
    let mut check = signed_check(&voter, &message);
    check.data[14..16].copy_from_slice(&0u16.to_le_bytes());
    let record = record_vote(&round, &round.coordinator, true);
    assert_eq!(
        code(send_as(&mut env, &coordinator, &[check, record])),
        Some(E_VOTE_NOT_SIGNED)
    );
    assert_eq!(read_round(&env, &round.address).count, 0);

    // Two signatures in one check.
    let mut check = signed_check(&voter, &message);
    check.data[0] = 2;
    let record = record_vote(&round, &round.coordinator, true);
    assert!(send_as(&mut env, &coordinator, &[check, record]).is_err());
    assert_eq!(read_round(&env, &round.address).count, 0);
}

#[test]
fn no_key_votes_twice() {
    let mut env = boot();
    let round = new_round();
    open_round(&mut env, &round).unwrap();
    let voter = Keypair::new();
    vote(&mut env, &round, &voter, true).unwrap();
    assert_eq!(
        code(vote(&mut env, &round, &voter, false)),
        Some(E_ALREADY_VOTED)
    );
}

#[test]
fn a_round_holds_five_votes() {
    let mut env = boot();
    let round = new_round();
    open_round(&mut env, &round).unwrap();
    for _ in 0..MAX_ROUND_VOTES {
        vote(&mut env, &round, &Keypair::new(), true).unwrap();
    }
    assert_eq!(
        code(vote(&mut env, &round, &Keypair::new(), true)),
        Some(E_ROUND_FULL)
    );
}

#[test]
fn only_the_rounds_coordinator_records_votes() {
    let mut env = boot();
    let round = new_round();
    open_round(&mut env, &round).unwrap();
    let stranger = Keypair::new();
    let voter = Keypair::new();
    let check = signed_check(&voter, &vote_message(&TASK, &PATCH, true));
    let record = record_vote(&round, &stranger, true);
    assert_eq!(
        code(send_as(&mut env, &stranger, &[check, record])),
        Some(E_UNAUTHORIZED)
    );
}

#[test]
fn nobody_else_can_open_the_coordinators_round() {
    let mut env = boot();
    let round = new_round();
    let stranger = Keypair::new();
    assert_eq!(
        code(open_round_at(&mut env, &round, &stranger, round.address)),
        Some(E_UNAUTHORIZED)
    );
    open_round(&mut env, &round).expect("the real open still lands");
}

#[test]
fn a_round_opens_once_and_an_address_funded_first_still_opens() {
    let mut env = boot();
    let round = new_round();
    env.svm.airdrop(&round.address, 5_000_000).unwrap();
    open_round(&mut env, &round).expect("open over a pre-funded address");
    assert_eq!(read_round(&env, &round.address).count, 0);
    bump_blockhash(&mut env);
    assert!(open_round(&mut env, &round).is_err());
}

#[test]
fn only_the_coordinator_settles_and_only_to_the_payer() {
    let mut env = boot();
    let payer = env.payer.pubkey();
    let round = new_round();
    open_round(&mut env, &round).unwrap();
    let stranger = Keypair::new();
    let coordinator = round.coordinator.insecure_clone();
    assert_eq!(
        code(settle(&mut env, &round, &stranger, payer)),
        Some(E_UNAUTHORIZED)
    );
    assert_eq!(
        code(settle(&mut env, &round, &coordinator, stranger.pubkey())),
        Some(E_UNAUTHORIZED)
    );
    settle(&mut env, &round, &coordinator, payer).expect("settle");
}

#[test]
fn a_settled_round_takes_no_more_votes() {
    let mut env = boot();
    let payer = env.payer.pubkey();
    let round = new_round();
    open_round(&mut env, &round).unwrap();
    let coordinator = round.coordinator.insecure_clone();
    settle(&mut env, &round, &coordinator, payer).unwrap();
    assert!(vote(&mut env, &round, &Keypair::new(), true).is_err());
}

#[test]
fn a_closed_round_refuses_votes() {
    // `close_round` runs only on the rollup; plant the state it leaves behind.
    let mut env = boot();
    let round = new_round();
    open_round(&mut env, &round).unwrap();
    let mut account = env.svm.get_account(&round.address).unwrap();
    let state = bytemuck::from_bytes_mut::<VoteRound>(&mut account.data[8..]);
    state.result = covenant_settlement_program::ROUND_PASSED;
    env.svm.set_account(round.address, account).unwrap();
    assert_eq!(
        code(vote(&mut env, &round, &Keypair::new(), true)),
        Some(E_ROUND_CLOSED)
    );
}
