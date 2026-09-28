//! Hermetic proof of the one-call money-trail verification
//! (`verify_payout`): a stub coordinator serving this buyer's history
//! rows and a stub Solana RPC serving a `jsonParsed` `getTransaction`
//! fixture, with real ed25519 signatures on every envelope and receipt.
//! Covers each verdict on the trail — no receipt, no payout due,
//! pending, off-chain record, chain-verified — and the refusals: books
//! that contradict the chain, a receipt that fails re-verification, an
//! on-chain pointer with no RPC configured, an unknown job.

use std::time::Duration;

use axum::routing::{get, post};
use axum::{Json, Router};
use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency, A2ATaskStatus};
use covenant_compute_buyer::{verify_payout, BuyerConfig, BuyerError};
use covenant_compute_protocol::{
    output_hash_hex, CapabilityRequirement, JobEnvelopePayload, JobKind, JobMeter,
    SignedJobEnvelope, SignedWorkReceipt, WorkReceiptPayload,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const MINT: &str = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";
const OPERATOR_WALLET: &str = "9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRSTEA";
const COORDINATOR_WALLET: &str = "7Np41oeYqPefeNQEHSv1UDhYrehxin3NStELsSKCT4K2";

fn envelope_and_receipt(
    price: u64,
    status: A2ATaskStatus,
) -> (LocalIdentity, SignedJobEnvelope, SignedWorkReceipt) {
    let buyer = LocalIdentity::generate("buyer@verify-test");
    let operator = LocalIdentity::generate("operator@verify-test");
    let job_id = Uuid::new_v4();
    let payload = JobEnvelopePayload {
        job_id,
        buyer: buyer.agent_id(),
        kind: JobKind::InferenceCall,
        capability_requirement: CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id: None,
            kind: JobKind::InferenceCall,
            max_duration_secs: 30,
            min_reputation_bps: None,
        },
        input: vec![Content::text("in")],
        price_micro_usdc: price,
        deadline_ms: 30_000,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "verify-payout-test"),
        issued_at_ms: 1,
        referral_code: None,
        stream: false,
    };
    let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
    let job_hash_hex = {
        let digest = Sha256::digest(envelope.payload_json.as_bytes());
        digest.iter().map(|b| format!("{b:02x}")).collect()
    };
    let receipt = SignedWorkReceipt::sign(
        WorkReceiptPayload {
            job_id,
            operator: operator.agent_id(),
            job_hash_hex,
            result_hash_hex: output_hash_hex(&[Content::text("out")]),
            meter: JobMeter {
                wall_ms: 5,
                tokens_in: None,
                tokens_out: None,
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc: price,
            status,
            executed_at_ms: 2,
            node_audit_root_hex: "cc".repeat(32),
        },
        &operator,
    )
    .unwrap();
    (buyer, envelope, receipt)
}

fn row(
    envelope: &SignedJobEnvelope,
    status: &str,
    receipt: Option<&SignedWorkReceipt>,
    payout: Option<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "job_id": envelope.payload.job_id,
        "status": status,
        "price_micro_usdc": envelope.payload.price_micro_usdc,
        "funding_source": "organic",
        "issued_at_ms": envelope.payload.issued_at_ms,
        "envelope": envelope,
        "receipt": receipt,
        "payout": payout,
    })
}

fn payout_view(amount: u64, tx_signature: Option<&str>, memo: &str) -> serde_json::Value {
    serde_json::json!({
        "amount_micro_usdc": amount,
        "tx_signature": tx_signature,
        "memo": memo,
        "recorded_at_ms": 3,
    })
}

/// A `getTransaction` result in the `jsonParsed` shape
/// `verify_payout_onchain` reads: memo instructions plus per-wallet
/// pre/post token balances.
fn payout_tx(memos: &[&str], transfers: &[(&str, u64, u64)]) -> serde_json::Value {
    let instructions: Vec<serde_json::Value> = memos
        .iter()
        .map(|m| {
            serde_json::json!({
                "program": "spl-memo",
                "programId": "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr",
                "parsed": m,
            })
        })
        .collect();
    let balance_rows = |index: usize| -> Vec<serde_json::Value> {
        transfers
            .iter()
            .map(|(owner, pre, post)| {
                let amount = if index == 0 { pre } else { post };
                serde_json::json!({
                    "owner": owner,
                    "mint": MINT,
                    "uiTokenAmount": { "amount": amount.to_string() },
                })
            })
            .collect()
    };
    serde_json::json!({
        "meta": {
            "err": null,
            "preTokenBalances": balance_rows(0),
            "postTokenBalances": balance_rows(1),
        },
        "transaction": { "message": { "instructions": instructions } },
    })
}

async fn serve_coordinator(rows: serde_json::Value) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = Router::new().route(
        "/federation/buyers/:buyer/jobs",
        get(move || {
            let rows = rows.clone();
            async move { Json(rows) }
        }),
    );
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

async fn serve_rpc(result: serde_json::Value) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = Router::new().route(
        "/",
        post(move || {
            let result = result.clone();
            async move { Json(serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": result })) }
        }),
    );
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

/// An RPC stub answering `getTransaction` and `getSignatureStatuses`
/// from separate canned results, so a test can model "the coordinator
/// recorded a signature the buyer's RPC hasn't served the transaction
/// for yet".
async fn serve_rpc_split(
    get_transaction: serde_json::Value,
    sig_status: serde_json::Value,
) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = Router::new().route(
        "/",
        post(move |Json(req): Json<serde_json::Value>| {
            let get_transaction = get_transaction.clone();
            let sig_status = sig_status.clone();
            async move {
                let result = if req.get("method").and_then(serde_json::Value::as_str)
                    == Some("getSignatureStatuses")
                {
                    sig_status
                } else {
                    get_transaction
                };
                Json(serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": result }))
            }
        }),
    );
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

fn config(coordinator_url: String, rpc_url: Option<String>) -> BuyerConfig {
    BuyerConfig {
        coordinator_url,
        poll_interval: Duration::from_millis(50),
        referral_code: None,
        rpc_url,
    }
}

#[tokio::test]
async fn verified_onchain_when_the_chain_carries_the_memo_and_amount() {
    let (buyer, envelope, receipt) = envelope_and_receipt(1_000_000, A2ATaskStatus::Ok);
    let memo = receipt.payout_memo();
    let tx = payout_tx(
        &[&memo],
        &[
            (COORDINATOR_WALLET, 50_000_000, 49_020_000),
            (OPERATOR_WALLET, 0, 980_000),
        ],
    );
    let rpc = serve_rpc(tx).await;
    let coordinator = serve_coordinator(serde_json::json!([row(
        &envelope,
        "completed",
        Some(&receipt),
        Some(payout_view(980_000, Some("live-sig"), &memo)),
    )]))
    .await;

    let verification = verify_payout(
        &reqwest::Client::new(),
        &config(coordinator, Some(rpc)),
        &buyer,
        envelope.payload.job_id,
    )
    .await
    .expect("the chain honors the receipt");
    assert_eq!(verification.verdict, "verified_onchain");
    assert_eq!(verification.memo.as_deref(), Some(memo.as_str()));
    assert_eq!(verification.tx_signature.as_deref(), Some("live-sig"));
    let proof = verification.proof.expect("chain-read proof");
    assert_eq!(proof.amount_micro_usdc, 980_000);
    assert_eq!(proof.mint_b58, MINT);
    assert_eq!(proof.recipient_owner_b58, OPERATOR_WALLET);
}

#[tokio::test]
async fn a_schemeless_rpc_url_is_refused_before_the_chain_read() {
    // A completed job with a real on-chain pointer, but the buyer's
    // rpc_url is missing its scheme. The refusal must name the fix, not
    // fail opaquely inside reqwest — and it must not depend on any RPC
    // server being reachable.
    let (buyer, envelope, receipt) = envelope_and_receipt(1_000_000, A2ATaskStatus::Ok);
    let memo = receipt.payout_memo();
    let coordinator = serve_coordinator(serde_json::json!([row(
        &envelope,
        "completed",
        Some(&receipt),
        Some(payout_view(980_000, Some("live-sig"), &memo)),
    )]))
    .await;

    let err = verify_payout(
        &reqwest::Client::new(),
        &config(coordinator, Some("localhost:8899".into())),
        &buyer,
        envelope.payload.job_id,
    )
    .await
    .expect_err("a schemeless rpc_url is a config error, not a chain read");
    assert!(
        err.to_string().contains("http://"),
        "the refusal must name the scheme fix: {err}"
    );
}

#[tokio::test]
async fn refuses_books_that_claim_a_different_amount_than_the_chain_moved() {
    let (buyer, envelope, receipt) = envelope_and_receipt(1_000_000, A2ATaskStatus::Ok);
    let memo = receipt.payout_memo();
    let tx = payout_tx(&[&memo], &[(OPERATOR_WALLET, 0, 970_000)]);
    let rpc = serve_rpc(tx).await;
    let coordinator = serve_coordinator(serde_json::json!([row(
        &envelope,
        "completed",
        Some(&receipt),
        Some(payout_view(980_000, Some("live-sig"), &memo)),
    )]))
    .await;

    let err = verify_payout(
        &reqwest::Client::new(),
        &config(coordinator, Some(rpc)),
        &buyer,
        envelope.payload.job_id,
    )
    .await
    .expect_err("books contradict the chain");
    let msg = err.to_string();
    assert!(
        msg.contains("970000") && msg.contains("980000"),
        "got: {msg}"
    );
}

#[tokio::test]
async fn offchain_record_only_when_no_transaction_was_submitted() {
    let (buyer, envelope, receipt) = envelope_and_receipt(1_000, A2ATaskStatus::Ok);
    let memo = receipt.payout_memo();
    let coordinator = serve_coordinator(serde_json::json!([row(
        &envelope,
        "completed",
        Some(&receipt),
        Some(payout_view(980, None, &memo)),
    )]))
    .await;

    let verification = verify_payout(
        &reqwest::Client::new(),
        &config(coordinator, None),
        &buyer,
        envelope.payload.job_id,
    )
    .await
    .unwrap();
    assert_eq!(verification.verdict, "offchain_record_only");
    assert_eq!(verification.memo.as_deref(), Some(memo.as_str()));
    assert!(verification.tx_signature.is_none());
    assert!(verification.proof.is_none());
    assert!(
        verification.detail.contains("980"),
        "got: {}",
        verification.detail
    );
}

#[tokio::test]
async fn payout_pending_on_a_completed_job_with_no_recorded_push() {
    let (buyer, envelope, receipt) = envelope_and_receipt(1_000, A2ATaskStatus::Ok);
    let coordinator = serve_coordinator(serde_json::json!([row(
        &envelope,
        "completed",
        Some(&receipt),
        None,
    )]))
    .await;

    let verification = verify_payout(
        &reqwest::Client::new(),
        &config(coordinator, None),
        &buyer,
        envelope.payload.job_id,
    )
    .await
    .unwrap();
    assert_eq!(verification.verdict, "payout_pending");
    assert_eq!(
        verification.memo.as_deref(),
        Some(receipt.payout_memo().as_str()),
        "the memo the eventual transfer must carry is already derivable"
    );
}

#[tokio::test]
async fn no_payout_due_on_a_failed_job() {
    let (buyer, envelope, receipt) = envelope_and_receipt(1_000, A2ATaskStatus::Error);
    let coordinator = serve_coordinator(serde_json::json!([row(
        &envelope,
        "failed",
        Some(&receipt),
        None
    )]))
    .await;

    let verification = verify_payout(
        &reqwest::Client::new(),
        &config(coordinator, None),
        &buyer,
        envelope.payload.job_id,
    )
    .await
    .unwrap();
    assert_eq!(verification.verdict, "no_payout_due");
    assert!(
        verification.detail.contains("refunded"),
        "got: {}",
        verification.detail
    );
}

#[tokio::test]
async fn no_receipt_before_the_job_concludes() {
    let (buyer, envelope, _) = envelope_and_receipt(1_000, A2ATaskStatus::Ok);
    let coordinator =
        serve_coordinator(serde_json::json!([row(&envelope, "accepted", None, None)])).await;

    let verification = verify_payout(
        &reqwest::Client::new(),
        &config(coordinator, None),
        &buyer,
        envelope.payload.job_id,
    )
    .await
    .unwrap();
    assert_eq!(verification.verdict, "no_receipt");
    assert!(verification.memo.is_none());
    assert!(
        verification.detail.contains("accepted"),
        "got: {}",
        verification.detail
    );
}

#[tokio::test]
async fn unknown_job_is_a_loud_error() {
    let (buyer, _, _) = envelope_and_receipt(1_000, A2ATaskStatus::Ok);
    let coordinator = serve_coordinator(serde_json::json!([])).await;

    let err = verify_payout(
        &reqwest::Client::new(),
        &config(coordinator, None),
        &buyer,
        Uuid::new_v4(),
    )
    .await
    .expect_err("nothing to verify");
    assert!(err.to_string().contains("history"), "got: {err}");
}

#[tokio::test]
async fn an_onchain_pointer_without_an_rpc_endpoint_is_an_explicit_config_error() {
    let (buyer, envelope, receipt) = envelope_and_receipt(1_000, A2ATaskStatus::Ok);
    let memo = receipt.payout_memo();
    let coordinator = serve_coordinator(serde_json::json!([row(
        &envelope,
        "completed",
        Some(&receipt),
        Some(payout_view(980, Some("live-sig"), &memo)),
    )]))
    .await;

    let err = verify_payout(
        &reqwest::Client::new(),
        &config(coordinator, None),
        &buyer,
        envelope.payload.job_id,
    )
    .await
    .expect_err("no rpc endpoint configured");
    assert!(matches!(err, BuyerError::Verification(_)), "got: {err}");
    assert!(err.to_string().contains("rpc_url"), "got: {err}");
}

#[tokio::test]
async fn a_receipt_for_someone_elses_work_never_reaches_the_chain() {
    let (buyer, envelope, _) = envelope_and_receipt(1_000, A2ATaskStatus::Ok);
    let (_, _, foreign_receipt) = envelope_and_receipt(1_000, A2ATaskStatus::Ok);
    let coordinator = serve_coordinator(serde_json::json!([row(
        &envelope,
        "completed",
        Some(&foreign_receipt),
        None,
    )]))
    .await;

    let err = verify_payout(
        &reqwest::Client::new(),
        &config(coordinator, None),
        &buyer,
        envelope.payload.job_id,
    )
    .await
    .expect_err("a relayed foreign receipt must not verify");
    assert!(matches!(err, BuyerError::Verification(_)), "got: {err}");
}

#[tokio::test]
async fn payout_unconfirmed_when_the_signature_is_seen_but_the_tx_is_not_served_yet() {
    // The coordinator recorded a signature its own RPC confirmed, but
    // the buyer's RPC hasn't served the transaction back yet: a soft
    // "re-check shortly", never a verification failure that reads as
    // fraud.
    let (buyer, envelope, receipt) = envelope_and_receipt(1_000_000, A2ATaskStatus::Ok);
    let memo = receipt.payout_memo();
    let rpc = serve_rpc_split(
        serde_json::Value::Null,
        serde_json::json!({ "value": [{ "confirmationStatus": "confirmed" }] }),
    )
    .await;
    let coordinator = serve_coordinator(serde_json::json!([row(
        &envelope,
        "completed",
        Some(&receipt),
        Some(payout_view(980_000, Some("propagating-sig"), &memo)),
    )]))
    .await;

    let verification = verify_payout(
        &reqwest::Client::new(),
        &config(coordinator, Some(rpc)),
        &buyer,
        envelope.payload.job_id,
    )
    .await
    .expect("a not-yet-served payout is a soft verdict, not an error");
    assert_eq!(verification.verdict, "payout_unconfirmed");
    assert_eq!(
        verification.tx_signature.as_deref(),
        Some("propagating-sig")
    );
    assert!(verification.proof.is_none());
    assert!(
        verification.detail.contains("re-check"),
        "got: {}",
        verification.detail
    );
}

#[tokio::test]
async fn a_recorded_signature_the_rpc_never_saw_is_a_loud_error() {
    // A signature the buyer's RPC has no record of at all is a real
    // contradiction: the payout the coordinator claims must not read as
    // merely pending.
    let (buyer, envelope, receipt) = envelope_and_receipt(1_000_000, A2ATaskStatus::Ok);
    let memo = receipt.payout_memo();
    let rpc = serve_rpc_split(
        serde_json::Value::Null,
        serde_json::json!({ "value": [serde_json::Value::Null] }),
    )
    .await;
    let coordinator = serve_coordinator(serde_json::json!([row(
        &envelope,
        "completed",
        Some(&receipt),
        Some(payout_view(980_000, Some("phantom-sig"), &memo)),
    )]))
    .await;

    let err = verify_payout(
        &reqwest::Client::new(),
        &config(coordinator, Some(rpc)),
        &buyer,
        envelope.payload.job_id,
    )
    .await
    .expect_err("a signature the chain never saw must fail loudly");
    assert!(matches!(err, BuyerError::Verification(_)), "got: {err}");
    assert!(err.to_string().contains("no record of it"), "got: {err}");
}
