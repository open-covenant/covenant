//! Live, env-gated devnet e2e for the daemon-driven settlement flush.
//!
//! Proves the real cycle end to end: seed one unsettled receipt into a
//! `JsonlReceiptStore`, run the production `flush_once` with a real
//! `SubprocessSettlementSigner`, which spawns the `covenant-settlement-signer`
//! sidecar to anchor the batch on the deployed devnet settlement program and
//! confirm it on-chain, then marks the batch confirmed with the real
//! signature, slot, and block timestamp. A second flush proves idempotency:
//! every receipt now carries a `batch_id`, so there is nothing left to
//! anchor and the signer is never dispatched — no double-anchor.
//!
//! Not hermetic. It needs the built sidecar and a funded devnet key that is
//! the settlement program's `config.authority`, so it is gated on four env
//! vars and returns early (passing) when any is absent. Run it with:
//!   COVENANT_SETTLEMENT_SIGNER_BIN=/abs/path/covenant-settlement-signer \
//!   COVENANT_SETTLEMENT_KEYPAIR=/abs/path/devnet-settlement-signer.json \
//!   COVENANT_SETTLEMENT_RPC_URL=https://api.devnet.solana.com \
//!   COVENANT_SETTLEMENT_PROGRAM_ID=cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y \
//!   cargo test -p covenantd --test live_settlement_devnet_flush \
//!     -- --ignored --nocapture

use covenant_settlement::{JsonlReceiptStore, Settlement};
use covenant_types::{AgentId, ResourceKind, SettlementReceipt};
use covenantd::settlement_signer::{
    flush_once, FlushOutcome, SettlementFlushConfig, SubprocessSettlementSigner,
};
use uuid::Uuid;

/// A fresh unsettled receipt. The v4 id feeds the receipt hash, so every run
/// derives a distinct batch id and anchors a new PDA rather than colliding
/// with a prior run's batch.
fn unique_receipt() -> SettlementReceipt {
    SettlementReceipt {
        id: Uuid::new_v4(),
        payer: AgentId::new("settlement-e2e@local", [7u8; 32]),
        resource: ResourceKind::Compute,
        memory_record_id: None,
        credits_consumed: 1,
        settled_at: 1_700_000_000_000,
        chain: None,
        cluster: None,
        batch_id: None,
        merkle_root: None,
        tx_sig: None,
        slot: None,
        confirmed_at: None,
        onchain_sig: None,
    }
}

#[tokio::test]
#[ignore = "live+funded: drives flush_once -> settlement-signer sidecar -> devnet anchor"]
async fn live_settlement_devnet_flush_anchors_and_is_idempotent() {
    let (Ok(signer_binary), Ok(rpc_url), Ok(program_id), Ok(keypair_path)) = (
        std::env::var("COVENANT_SETTLEMENT_SIGNER_BIN"),
        std::env::var("COVENANT_SETTLEMENT_RPC_URL"),
        std::env::var("COVENANT_SETTLEMENT_PROGRAM_ID"),
        std::env::var("COVENANT_SETTLEMENT_KEYPAIR"),
    ) else {
        eprintln!(
            "skip: set COVENANT_SETTLEMENT_SIGNER_BIN + COVENANT_SETTLEMENT_RPC_URL + \
             COVENANT_SETTLEMENT_PROGRAM_ID + COVENANT_SETTLEMENT_KEYPAIR \
             (a funded devnet config authority) to run this"
        );
        return;
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("receipts").join("working.jsonl");
    let store = JsonlReceiptStore::open(path).await.expect("open store");
    store
        .record(unique_receipt())
        .await
        .expect("record receipt");

    let config = SettlementFlushConfig {
        enabled: true,
        cluster: "devnet".to_string(),
        rpc_url,
        program_id,
        keypair_path,
        signer_binary,
        ..Default::default()
    };
    let signer = SubprocessSettlementSigner::from_config(&config).expect("build devnet signer");

    // First flush: real anchor + confirm on devnet, then mark_batch_confirmed.
    let signature = match flush_once(&store, &signer, &config.cluster, config.limit)
        .await
        .expect("flush should anchor the batch")
    {
        FlushOutcome::Anchored {
            batch_id,
            signature,
            slot,
            confirmed_at,
            receipts_updated,
            already_anchored,
        } => {
            assert_eq!(
                receipts_updated, 1,
                "the one seeded receipt must be settled"
            );
            assert!(
                !signature.is_empty(),
                "a confirmed anchor must carry a signature"
            );
            assert!(
                slot > 0,
                "a confirmed anchor must carry the slot it landed in"
            );
            assert!(
                confirmed_at > 0,
                "a confirmed anchor must carry the on-chain block time"
            );
            eprintln!(
                "devnet settlement anchor: batch_id={batch_id} signature={signature} \
                 slot={slot} confirmed_at={confirmed_at} already_anchored={already_anchored}"
            );
            signature
        }
        FlushOutcome::NoReceipts => panic!("expected an anchored batch, got NoReceipts"),
    };

    // The receipt now carries the on-chain confirmation the anchor produced.
    let settled = store.recent(256).await.expect("read settled receipts");
    assert!(
        settled.iter().all(|r| {
            r.batch_id.is_some()
                && r.tx_sig.as_deref() == Some(signature.as_str())
                && r.slot.is_some()
                && r.confirmed_at.is_some()
        }),
        "every receipt must carry batch_id + tx_sig + slot + confirmed_at after a confirmed flush"
    );

    // Second flush: nothing unsettled remains, so the batch is empty and the
    // signer is never dispatched — proving idempotency, not a double-anchor.
    let reflush = flush_once(&store, &signer, &config.cluster, config.limit)
        .await
        .expect("reflush of a settled store must succeed");
    assert_eq!(
        reflush,
        FlushOutcome::NoReceipts,
        "a fully-settled store has nothing left to flush"
    );
}
