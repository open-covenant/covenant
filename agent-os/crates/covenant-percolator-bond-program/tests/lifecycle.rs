//! End-to-end banks-client test for the bond program.
//!
//! Boots a `ProgramTest` with our entrypoint, then drives the full
//! lifecycle (Initialize → Deposit → Slash → verify) via real
//! signed transactions. This is the "deployable artifact actually
//! works" test: the same code path that `cargo build-sbf` ships in
//! the `.so` runs here.

use covenant_percolator_bond::evidence::AttestedAction;
use covenant_percolator_bond::instruction;
use covenant_percolator_bond::scope::{ActionMask, BondScope};
use covenant_percolator_bond::state::BondAccount;
use covenant_percolator_bond::{BOND_SEED, SLASH_SEED};
use covenant_percolator_bond_program::process_instruction;
use solana_program_test::{processor, ProgramTest};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer};
use solana_sdk::transaction::Transaction;

fn bond_scope() -> BondScope {
    BondScope {
        version: 1,
        market: [0x11; 32],
        allowed_actions: ActionMask(ActionMask::PUSH_MARK | ActionMask::CRANK),
        allowed_assets: Some(vec![0, 1]),
        max_actions_per_tick: 4,
    }
}

#[tokio::test]
async fn full_lifecycle_initialize_deposit_slash() {
    let program_id = Pubkey::new_unique();
    let mut pt = ProgramTest::new(
        "covenant_percolator_bond_program",
        program_id,
        processor!(process_instruction),
    );

    let operator = Keypair::new();
    let keeper = Keypair::new();
    let recipient = Pubkey::new_unique();
    // Pre-fund the operator so they can pay rent + the slasher's PDA.
    pt.add_account(
        operator.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );

    let (banks, _payer, recent_blockhash) = pt.start().await;

    let (bond_pda, _) =
        Pubkey::find_program_address(&[BOND_SEED, keeper.pubkey().as_ref()], &program_id);

    // 1) Initialize. The scope_hash here is the slasher's commitment;
    // it must match what evidence is later submitted against.
    let scope = bond_scope();
    let scope_hash = scope.hash();
    let init_ix = instruction::initialize_bond(
        program_id,
        bond_pda,
        operator.pubkey(),
        keeper.pubkey(),
        scope_hash,
        recipient,
        100, // created_slot
    );
    let mut tx = Transaction::new_with_payer(&[init_ix], Some(&operator.pubkey()));
    tx.sign(&[&operator], recent_blockhash);
    banks.process_transaction(tx).await.expect("initialize");

    // Read back the bond account: ours, scope hash matches, lamports=0.
    let acc = banks.get_account(bond_pda).await.unwrap().expect("bond exists");
    let bond = BondAccount::decode(&acc.data).expect("decode");
    assert_eq!(bond.keeper, keeper.pubkey().to_bytes());
    assert_eq!(bond.operator, operator.pubkey().to_bytes());
    assert_eq!(bond.scope_hash, scope_hash);
    assert_eq!(bond.lamports, 0);
    assert_eq!(bond.slashed, 0);

    // 2) Deposit 2 SOL. The on-chain account's `lamports` field
    // grows to track the bond escrow.
    let deposit_ix = instruction::deposit(program_id, bond_pda, operator.pubkey(), 2_000_000_000);
    let mut tx = Transaction::new_with_payer(&[deposit_ix], Some(&operator.pubkey()));
    tx.sign(&[&operator], recent_blockhash);
    banks.process_transaction(tx).await.expect("deposit");

    let acc = banks.get_account(bond_pda).await.unwrap().unwrap();
    let bond = BondAccount::decode(&acc.data).unwrap();
    assert_eq!(bond.lamports, 2_000_000_000);

    // 3) Slash. Keeper executed PushAuthMark on asset 7 — outside
    // the operator-signed allowed_assets `[0, 1]`. Anyone can submit
    // this evidence; we use the operator as the rent payer for
    // simplicity, but it could be anyone holding SOL.
    let evidence = covenant_percolator_bond::SlashEvidence {
        scope: scope.clone(),
        action: AttestedAction {
            receipt_id: [0xCA; 16],
            executed_slot: 200,
            market: scope.market,
            action_bit: ActionMask::PUSH_MARK,
            asset_index: Some(7),
        },
    };
    let (receipt_pda, _) = Pubkey::find_program_address(
        &[SLASH_SEED, bond_pda.as_ref(), &[0xCA; 16]],
        &program_id,
    );
    let slash_ix = instruction::slash(
        program_id,
        bond_pda,
        recipient,
        receipt_pda,
        operator.pubkey(),
        &evidence,
    );
    let mut tx = Transaction::new_with_payer(&[slash_ix], Some(&operator.pubkey()));
    tx.sign(&[&operator], recent_blockhash);
    banks.process_transaction(tx).await.expect("slash");

    // 4) Verify: bond.slashed = 1, lamports zero, recipient holds
    // the slashed amount.
    let acc = banks.get_account(bond_pda).await.unwrap().unwrap();
    let bond = BondAccount::decode(&acc.data).unwrap();
    assert_eq!(bond.slashed, 1);
    assert_eq!(bond.lamports, 0);

    let recipient_acc = banks.get_account(recipient).await.unwrap().unwrap();
    assert!(
        recipient_acc.lamports >= 2_000_000_000,
        "recipient should hold the slashed bond"
    );

    // 5) Re-slash with FRESH evidence (different receipt_id, so the
    // PDA replay layer doesn't short-circuit) must STILL fail —
    // because `bond.slashed = 1` after the first drain. This is the
    // important property: a slashed bond cannot be drained twice
    // even via "new" evidence.
    let evidence2 = covenant_percolator_bond::SlashEvidence {
        scope: scope.clone(),
        action: AttestedAction {
            receipt_id: [0xDD; 16],
            executed_slot: 250,
            market: scope.market,
            action_bit: ActionMask::CRANK,
            asset_index: Some(99),
        },
    };
    let (receipt_pda2, _) = Pubkey::find_program_address(
        &[SLASH_SEED, bond_pda.as_ref(), &[0xDD; 16]],
        &program_id,
    );
    // Different evidence → different ix data → different tx signature,
    // so the bank doesn't dedupe even at the same blockhash. The
    // program-level rejection is what we're testing.
    let slash_ix = instruction::slash(
        program_id,
        bond_pda,
        recipient,
        receipt_pda2,
        operator.pubkey(),
        &evidence2,
    );
    let mut tx = Transaction::new_with_payer(&[slash_ix], Some(&operator.pubkey()));
    tx.sign(&[&operator], recent_blockhash);
    banks
        .process_transaction(tx)
        .await
        .expect_err("re-slash on slashed bond must fail");
}

#[tokio::test]
async fn well_behaved_keeper_keeps_bond() {
    let program_id = Pubkey::new_unique();
    let mut pt = ProgramTest::new(
        "covenant_percolator_bond_program",
        program_id,
        processor!(process_instruction),
    );
    let operator = Keypair::new();
    let keeper = Keypair::new();
    let recipient = Pubkey::new_unique();
    pt.add_account(
        operator.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let (banks, _payer, recent_blockhash) = pt.start().await;
    let (bond_pda, _) =
        Pubkey::find_program_address(&[BOND_SEED, keeper.pubkey().as_ref()], &program_id);

    let scope = bond_scope();
    let init_ix = instruction::initialize_bond(
        program_id,
        bond_pda,
        operator.pubkey(),
        keeper.pubkey(),
        scope.hash(),
        recipient,
        100,
    );
    let mut tx = Transaction::new_with_payer(&[init_ix], Some(&operator.pubkey()));
    tx.sign(&[&operator], recent_blockhash);
    banks.process_transaction(tx).await.unwrap();

    let deposit_ix = instruction::deposit(program_id, bond_pda, operator.pubkey(), 1_000_000_000);
    let mut tx = Transaction::new_with_payer(&[deposit_ix], Some(&operator.pubkey()));
    tx.sign(&[&operator], recent_blockhash);
    banks.process_transaction(tx).await.unwrap();

    // Slasher claims the keeper acted on asset 0 — but that's
    // permitted by the scope. The verifier rejects with NoViolation
    // (mapped to Custom(4)), the bond stays intact.
    let evidence = covenant_percolator_bond::SlashEvidence {
        scope: scope.clone(),
        action: AttestedAction {
            receipt_id: [0xBB; 16],
            executed_slot: 200,
            market: scope.market,
            action_bit: ActionMask::PUSH_MARK,
            asset_index: Some(0), // in-scope
        },
    };
    let (receipt_pda, _) = Pubkey::find_program_address(
        &[SLASH_SEED, bond_pda.as_ref(), &[0xBB; 16]],
        &program_id,
    );
    let slash_ix = instruction::slash(
        program_id,
        bond_pda,
        recipient,
        receipt_pda,
        operator.pubkey(),
        &evidence,
    );
    let mut tx = Transaction::new_with_payer(&[slash_ix], Some(&operator.pubkey()));
    tx.sign(&[&operator], recent_blockhash);
    banks
        .process_transaction(tx)
        .await
        .expect_err("slash on in-scope action must fail");

    let acc = banks.get_account(bond_pda).await.unwrap().unwrap();
    let bond = BondAccount::decode(&acc.data).unwrap();
    assert_eq!(bond.slashed, 0, "well-behaved keeper bond preserved");
    assert_eq!(bond.lamports, 1_000_000_000);
}
