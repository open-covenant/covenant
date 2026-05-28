//! End-to-end pipeline: synthetic keeper action → wire-locked
//! instruction → recorded submission. Locks the contract between
//! the decision layer, the on-chain builders, and the sender so a
//! drift in any of the three breaks the chain.
//!
//! Requires `--features solana-rpc` (the sender pulls in solana-client).

#![cfg(feature = "solana-rpc")]

use covenant_percolator::onchain::{build_instruction, BuildContext};
use covenant_percolator::sender::{RecordingSender, Sender, SenderError};
use covenant_percolator::state::KeeperAction;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signature::Signer;

fn ctx(payer: &Keypair) -> BuildContext {
    BuildContext {
        program_id: Pubkey::new_unique(),
        market: Pubkey::new_unique(),
        keeper: payer.pubkey(),
        portfolio: Some(Pubkey::new_unique()),
        now_slot: 12_345,
        default_side: 0,
    }
}

#[tokio::test]
async fn push_auth_mark_action_round_trips_through_sender() {
    let payer = Keypair::new();
    let ix = build_instruction(
        &KeeperAction::PushAuthMark {
            asset_index: 1,
            mark_e6: 999_888,
        },
        &ctx(&payer),
    )
    .unwrap();
    // Tag byte locked to v16 PushAuthMark.
    assert_eq!(ix.data[0], 63);

    let sender = RecordingSender::new();
    let outcome = sender.submit(vec![ix.clone()], &payer, &[]).await.unwrap();
    assert_eq!(outcome.attempts, 1);

    let submissions = sender.submissions().await;
    assert_eq!(submissions.len(), 1);
    assert_eq!(submissions[0][0].data, ix.data);
}

#[tokio::test]
async fn sender_propagates_non_transient_errors() {
    let payer = Keypair::new();
    let ix = build_instruction(&KeeperAction::Crank, &ctx(&payer)).unwrap();
    let sender = RecordingSender::new()
        .with_failures([SenderError::Rpc("InsufficientFundsForFee".into())])
        .await;
    let err = sender.submit(vec![ix], &payer, &[]).await.unwrap_err();
    assert!(matches!(err, SenderError::Rpc(_)));
    // Nothing recorded — the failed submit didn't get logged.
    assert!(sender.submissions().await.is_empty());
}

#[tokio::test]
async fn recovery_finalize_needs_no_portfolio_and_still_submits() {
    let payer = Keypair::new();
    let mut c = ctx(&payer);
    c.portfolio = None;
    let ix = build_instruction(
        &KeeperAction::RecoveryFinalize {
            asset_index: 0,
            side: 1,
        },
        &c,
    )
    .unwrap();
    assert_eq!(ix.data[0], 45);
    let sender = RecordingSender::new();
    sender.submit(vec![ix], &payer, &[]).await.unwrap();
    assert_eq!(sender.submissions().await.len(), 1);
}
