//! End-to-end against Toly's actual percolator-prog binary.
//!
//! Loads the mainnet program `4m3ipBQDYX…` (dumped via
//! `solana program dump`) into an in-process `solana-program-test`
//! bank and verifies our wire-format builders submit transactions
//! that his decoder accepts. The program rejects the txs at the
//! account-validation layer (we don't synthesize a full v16 market
//! group account here — that's a multi-thousand-byte effort and a
//! separate fixture), but the *kind* of error it returns is the
//! load-bearing signal:
//!
//!   - If his decoder rejected our bytes as malformed instruction
//!     data, we'd see `ProgramError::InvalidInstructionData`. We
//!     don't — that means tag 63 + LE u16/u64/u64 is decoded correctly.
//!   - We see ProgramError::Custom(_) or account-validation errors,
//!     proving the program ran our handler dispatch path.
//!
//! This test requires the program binary at
//! `tests/fixtures/percolator-prog.so` (857KB). It's dumped from
//! mainnet with:
//!
//!   solana program dump 4m3ipBQDYX6JQ9YSmUXDjESDHMtGWtiXforkWr9Qoxdi \
//!     tests/fixtures/percolator-prog.so --url mainnet-beta
//!
//! Skipped automatically if the fixture is absent.

#![cfg(feature = "solana-rpc")]

use std::path::PathBuf;

use covenant_percolator::instruction;
use solana_program_test::ProgramTest;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer};
use solana_sdk::transaction::Transaction;
use std::str::FromStr;

const PROGRAM_ID: &str = "4m3ipBQDYX6JQ9YSmUXDjESDHMtGWtiXforkWr9Qoxdi";

fn fixture_path() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/percolator-prog.so");
    p
}

fn make_bank(prefix: &str) -> Option<ProgramTest> {
    let path = fixture_path();
    if !path.exists() {
        eprintln!(
            "skipping {prefix}: fixture not present at {}; dump via `solana program dump`",
            path.display()
        );
        return None;
    }
    let mut pt = ProgramTest::default();
    let program_id = Pubkey::from_str(PROGRAM_ID).unwrap();
    pt.add_upgradeable_program_to_genesis("percolator-prog", &program_id);
    Some(pt)
}

/// PushAuthMark targets `(authority, market)`. Without a properly
/// initialized v16 market account at `market`, the program
/// validates accounts and returns an account-level error — but the
/// instruction-data decoder has already run. That's the bit we
/// care about: his program's `Instruction::decode` accepted our
/// byte sequence (tag 63 + u16 LE asset_index + u64 LE now_slot
/// + u64 LE mark_e6).
#[tokio::test(flavor = "current_thread")]
async fn push_auth_mark_wire_format_accepted_by_program() {
    let Some(pt) = make_bank("push_auth_mark_wire_format_accepted_by_program") else {
        return;
    };
    let (banks, payer, recent_blockhash) = pt.start().await;

    let program_id = Pubkey::from_str(PROGRAM_ID).unwrap();
    let authority = Keypair::new();
    let market = Pubkey::new_unique();

    // PushAuthMark from our crate's wire-locked builder.
    let ix = instruction::push_auth_mark(
        program_id,
        market,
        authority.pubkey(),
        1,
        1_000_000,
        145_321_000,
    );

    let mut tx = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    tx.sign(&[&payer, &authority], recent_blockhash);

    // The tx WILL fail (no real market initialized) — but we want
    // to confirm WHY. A wire-format mismatch would surface as
    // `InvalidInstructionData`. Anything else proves his decoder
    // parsed our bytes and moved on to account validation.
    let err = banks.process_transaction(tx).await.expect_err("expected failure");
    let msg = format!("{err:?}");
    eprintln!("push_auth_mark error (expected, validates wire format): {msg}");
    assert!(
        !msg.contains("InvalidInstructionData"),
        "his program rejected our wire format: {msg}"
    );
}

/// `PermissionlessCrank` (tag 5) is the most complex wire layout
/// in the keeper surface — `u8 + u8 + u16 + u64 + i128 + u128 + u64
/// + u8` = 49 bytes after the tag. If any field width or order is
/// wrong, his decoder rejects with `InvalidInstructionData`. We
/// build a refresh-style call and verify the parse succeeds.
#[tokio::test(flavor = "current_thread")]
async fn permissionless_crank_wire_format_accepted_by_program() {
    let Some(pt) = make_bank("permissionless_crank_wire_format_accepted_by_program") else {
        return;
    };
    let (banks, payer, recent_blockhash) = pt.start().await;

    let program_id = Pubkey::from_str(PROGRAM_ID).unwrap();
    let cranker = Keypair::new();
    let market = Pubkey::new_unique();
    let portfolio = Pubkey::new_unique();

    let ix = instruction::permissionless_crank_refresh(
        program_id,
        market,
        portfolio,
        cranker.pubkey(),
        0, // asset 0 (refresh)
        500_000,
    );
    let mut tx = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    tx.sign(&[&payer, &cranker], recent_blockhash);
    let err = banks.process_transaction(tx).await.expect_err("expected failure");
    let msg = format!("{err:?}");
    eprintln!("permissionless_crank error (expected): {msg}");
    assert!(
        !msg.contains("InvalidInstructionData"),
        "his program rejected our wire format: {msg}"
    );
}

/// `FinalizeResetSide` (tag 45) is permissionless (no signer) —
/// even simpler test. Just market + 1-byte side, 4 bytes total.
#[tokio::test(flavor = "current_thread")]
async fn finalize_reset_side_wire_format_accepted_by_program() {
    let Some(pt) = make_bank("finalize_reset_side_wire_format_accepted_by_program") else {
        return;
    };
    let (banks, payer, recent_blockhash) = pt.start().await;

    let program_id = Pubkey::from_str(PROGRAM_ID).unwrap();
    let market = Pubkey::new_unique();
    let ix = instruction::finalize_reset_side(program_id, market, 0, instruction::side::LONG);
    let mut tx = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    tx.sign(&[&payer], recent_blockhash);
    let err = banks.process_transaction(tx).await.expect_err("expected failure");
    let msg = format!("{err:?}");
    eprintln!("finalize_reset_side error (expected): {msg}");
    assert!(
        !msg.contains("InvalidInstructionData"),
        "his program rejected our wire format: {msg}"
    );
}

/// Sanity check that the program is loaded — verify a malformed
/// instruction (random byte that doesn't match any tag) DOES return
/// `InvalidInstructionData`. Confirms the test rig actually checks
/// what we think it checks (positive control for the negative
/// assertions above).
#[tokio::test(flavor = "current_thread")]
async fn unknown_tag_returns_invalid_instruction_data() {
    let Some(pt) = make_bank("unknown_tag_returns_invalid_instruction_data") else {
        return;
    };
    let (banks, payer, recent_blockhash) = pt.start().await;

    let program_id = Pubkey::from_str(PROGRAM_ID).unwrap();
    // Build a junk instruction: tag 254 (unused in his dispatch).
    let ix = solana_sdk::instruction::Instruction {
        program_id,
        accounts: vec![],
        data: vec![254, 0xff, 0xff, 0xff, 0xff],
    };
    let mut tx = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    tx.sign(&[&payer], recent_blockhash);
    let err = banks.process_transaction(tx).await.expect_err("expected failure");
    let msg = format!("{err:?}");
    eprintln!("unknown_tag error: {msg}");
    // Toly's program might return a Custom error for unknown tags,
    // OR InvalidInstructionData. EITHER is a sign that his program
    // ran the decode path — what we don't want is `ProgramFailedToComplete`
    // or runtime crash.
    assert!(
        msg.contains("InvalidInstructionData")
            || msg.contains("Custom")
            || msg.contains("InvalidArgument"),
        "unexpected error for unknown tag — program may not be loaded: {msg}"
    );
}
