//! Live HTTP round-trip against PayAI's facilitator with a deliberately
//! malformed transaction. Proves the request/response wire shape and
//! reachability without needing a real signed SPL transfer.
//!
//! Expected outcome: HTTP 200 with `isValid: false` and
//! `invalidReason: "invalid_exact_svm_payload_transaction_could_not_be_decoded"`.
//!
//!     cargo run -p covenant-hyre --example live_facilitator_smoke

use covenant_hyre::config::{PAY_TO, SOLANA_NETWORK, USDC_MINT};
use covenant_hyre::facilitator::{
    FacilitatorClient, FacilitatorExtra, FacilitatorPayload, FacilitatorRequest,
    FacilitatorRequirements,
};

const PAYAI_FEE_PAYER: &str = "2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4";
const NOT_A_TX_B64: &str = "bm90IGEgcmVhbCB0eA==";

#[tokio::main]
async fn main() {
    let http = reqwest::Client::new();
    let client = FacilitatorClient::new(http);

    let req = FacilitatorRequest::new(
        FacilitatorPayload::new("solana", NOT_A_TX_B64),
        FacilitatorRequirements {
            scheme: "exact".into(),
            network: "solana".into(),
            max_amount_required: "80000".into(),
            resource: "https://mpp.hyreagent.fun/defi/tvl".into(),
            description: "smoke probe".into(),
            mime_type: "application/json".into(),
            output_schema: serde_json::json!({}),
            pay_to: PAY_TO.into(),
            max_timeout_seconds: 30,
            asset: USDC_MINT.into(),
            extra: FacilitatorExtra {
                fee_payer: PAYAI_FEE_PAYER.into(),
            },
        },
    );

    println!("posting /verify with malformed tx...");
    println!("network ref (capability): {SOLANA_NETWORK}");
    let resp = client
        .verify(&req)
        .await
        .expect("verify call should round-trip");
    println!("isValid:        {}", resp.is_valid);
    println!("invalidReason:  {:?}", resp.invalid_reason);
    println!("invalidMessage: {:?}", resp.invalid_message);
    println!("payer:          {:?}", resp.payer);

    assert!(!resp.is_valid, "expected isValid=false for a garbage tx");
    let reason = resp.invalid_reason.as_deref().unwrap_or("");
    assert!(
        reason.starts_with("invalid_exact_svm_payload_transaction"),
        "unexpected invalidReason: {reason}"
    );
    println!("\nOK — round-trip confirmed, error shape matches.");
}
