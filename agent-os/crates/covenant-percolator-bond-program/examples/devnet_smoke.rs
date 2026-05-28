//! Live devnet smoke test.
//!
//! Hits the deployed bond program at
//! `DMy5XmGmYbBzvtRefRyqJTwBwFvo2WHEwrC3fgfLtEGE` (Solana devnet)
//! and walks Initialize → Deposit → Slash → Verify. Demonstrates
//! the same code paths the banks_client lifecycle test exercises,
//! but against a real on-chain artifact.
//!
//! Run:
//!     cargo run -p covenant-percolator-bond-program --example devnet_smoke
//!
//! Requires the operator keypair at `~/.config/solana/id.json` to
//! hold ≥ 0.1 SOL on devnet (airdrop or transfer as needed).

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use covenant_percolator_bond::evidence::AttestedAction;
use covenant_percolator_bond::instruction;
use covenant_percolator_bond::scope::{ActionMask, BondScope};
use covenant_percolator_bond::state::BondAccount;
use covenant_percolator_bond::{BOND_SEED, SLASH_SEED};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{read_keypair_file, Keypair, Signer};
use solana_sdk::transaction::Transaction;

const PROGRAM_ID_STR: &str = "DMy5XmGmYbBzvtRefRyqJTwBwFvo2WHEwrC3fgfLtEGE";
const RPC_URL: &str = "https://api.devnet.solana.com";

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let program_id = Pubkey::from_str(PROGRAM_ID_STR)?;
    let rpc = RpcClient::new_with_commitment(RPC_URL.to_string(), CommitmentConfig::confirmed());

    // Operator keypair (pays rent + signs initialize, set_paused).
    let mut path = PathBuf::from(std::env::var("HOME")?);
    path.push(".config");
    path.push("solana");
    path.push("id.json");
    let operator = read_keypair_file(&path).map_err(|e| anyhow::anyhow!("read keypair: {e}"))?;
    let operator_pk = operator.pubkey();
    println!("operator = {operator_pk}");

    // Use a fresh keeper key for each run so we get a fresh bond
    // PDA (no collisions with prior runs).
    let keeper = Keypair::new();
    let keeper_pk = keeper.pubkey();
    println!("keeper = {keeper_pk}");

    let recipient = Pubkey::new_unique();
    let scope = BondScope {
        version: 1,
        market: [0x11; 32],
        allowed_actions: ActionMask(ActionMask::PUSH_MARK | ActionMask::CRANK),
        allowed_assets: Some(vec![0, 1]),
        max_actions_per_tick: 4,
    };

    let (bond_pda, _) =
        Pubkey::find_program_address(&[BOND_SEED, keeper_pk.as_ref()], &program_id);
    println!("bond_pda = {bond_pda}");

    // 1) Initialize.
    println!("\n[1/4] initialize…");
    let bh = rpc.get_latest_blockhash().await?;
    let ix = instruction::initialize_bond(
        program_id,
        bond_pda,
        operator_pk,
        keeper_pk,
        scope.hash(),
        recipient,
        rpc.get_slot().await?,
    );
    let mut tx = Transaction::new_with_payer(&[ix], Some(&operator_pk));
    tx.sign(&[&operator], bh);
    let sig = rpc.send_and_confirm_transaction(&tx).await?;
    println!("    initialize sig: {sig}");

    // Read it back.
    let acc = rpc.get_account(&bond_pda).await?;
    let bond = BondAccount::decode(&acc.data).map_err(|e| anyhow::anyhow!("decode: {e}"))?;
    println!(
        "    on-chain bond: keeper={:?}…, scope_hash={:?}…, lamports={}",
        &bond.keeper[..4],
        &bond.scope_hash.0[..4],
        bond.lamports
    );

    // 2) Deposit 0.05 SOL.
    println!("\n[2/4] deposit 0.05 SOL…");
    let bh = rpc.get_latest_blockhash().await?;
    let ix = instruction::deposit(program_id, bond_pda, operator_pk, 50_000_000);
    let mut tx = Transaction::new_with_payer(&[ix], Some(&operator_pk));
    tx.sign(&[&operator], bh);
    let sig = rpc.send_and_confirm_transaction(&tx).await?;
    println!("    deposit sig: {sig}");
    let acc = rpc.get_account(&bond_pda).await?;
    let bond = BondAccount::decode(&acc.data).unwrap();
    println!("    on-chain bond lamports: {}", bond.lamports);

    // 3) Slash on out-of-scope asset 7.
    println!("\n[3/4] slash (out-of-scope asset 7)…");
    let evidence = covenant_percolator_bond::SlashEvidence {
        scope: scope.clone(),
        action: AttestedAction {
            receipt_id: [0xAB; 16],
            executed_slot: rpc.get_slot().await? - 1,
            market: scope.market,
            action_bit: ActionMask::PUSH_MARK,
            asset_index: Some(7),
        },
    };
    let (receipt_pda, _) = Pubkey::find_program_address(
        &[SLASH_SEED, bond_pda.as_ref(), &[0xAB; 16]],
        &program_id,
    );
    let bh = rpc.get_latest_blockhash().await?;
    let ix = instruction::slash(program_id, bond_pda, recipient, receipt_pda, operator_pk, &evidence);
    let mut tx = Transaction::new_with_payer(&[ix], Some(&operator_pk));
    tx.sign(&[&operator], bh);
    let sig = rpc.send_and_confirm_transaction(&tx).await?;
    println!("    slash sig: {sig}");

    // 4) Verify final state.
    println!("\n[4/4] verify…");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let acc = rpc.get_account(&bond_pda).await?;
    let bond = BondAccount::decode(&acc.data).unwrap();
    println!(
        "    final bond: slashed={}, lamports={}",
        bond.slashed, bond.lamports
    );
    let recipient_balance = rpc.get_balance(&recipient).await?;
    println!("    recipient holds {recipient_balance} lamports");

    assert_eq!(bond.slashed, 1);
    assert_eq!(bond.lamports, 0);
    assert!(recipient_balance >= 50_000_000);
    println!("\nall checks passed against live devnet program {PROGRAM_ID_STR}");
    Ok(())
}
