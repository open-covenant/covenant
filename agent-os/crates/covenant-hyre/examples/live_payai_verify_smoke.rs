//! Byte-correctness smoke: drive the production sidecar end-to-end on a
//! real PaymentRequirements, then POST the resulting X-PAYMENT payload
//! to PayAI's `/verify` (free — no co-sign, no on-chain spend) and
//! assert `isValid: true`. If this passes, the wire format we produce
//! is acceptable to PayAI and the only remaining risk is Hyre's
//! middleware itself.
//!
//!     COVENANT_X402_FUNDING_KEYPAIR=~/.config/solana/some.json \
//!     COVENANT_X402_RPC_URL=https://api.mainnet-beta.solana.com \
//!     COVENANT_X402_SIGNER_BIN=/path/to/covenant-x402-signer \
//!     cargo run -p covenant-hyre --example live_payai_verify_smoke
//!
//! Uses Hyre's `/defi/tvl` price-point and the pinned PayAI feePayer.
//! Funder must have an existing USDC ATA on whichever cluster the RPC
//! points at (mainnet by default).

use std::process::Stdio;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use covenant_hyre::config::{PAY_TO, PAYAI_FEE_PAYER, USDC_MINT};
use covenant_hyre::facilitator::{
    FacilitatorClient, FacilitatorExtra, FacilitatorPayload, FacilitatorRequest,
    FacilitatorRequirements,
};
use covenant_x402::{PaymentExtra, PaymentRequirements};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const TARGET_URL: &str = "https://mpp.hyreagent.fun/defi/tvl";
const TARGET_AMOUNT_ATOMIC: &str = "10000";

#[tokio::main]
async fn main() {
    let signer_bin = std::env::var("COVENANT_X402_SIGNER_BIN")
        .expect("set COVENANT_X402_SIGNER_BIN to the built covenant-x402-signer binary");
    let kp = std::env::var("COVENANT_X402_FUNDING_KEYPAIR")
        .expect("set COVENANT_X402_FUNDING_KEYPAIR to the funder's Solana keypair JSON");
    let rpc = std::env::var("COVENANT_X402_RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".into());

    let req = PaymentRequirements {
        network: "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp".into(),
        asset: USDC_MINT.into(),
        amount: TARGET_AMOUNT_ATOMIC.into(),
        amount_usdc: 0.01,
        pay_to: PAY_TO.into(),
        scheme: "exact".into(),
        extra: Some(PaymentExtra {
            fee_payer: Some(PAYAI_FEE_PAYER.into()),
        }),
    };
    println!("=== input PaymentRequirements ===");
    println!("{}", serde_json::to_string_pretty(&req).unwrap());

    println!("\n=== invoking sidecar to produce X-PAYMENT ===");
    let payload = serde_json::to_vec(&req).unwrap();
    let mut child = Command::new(&signer_bin)
        .env("COVENANT_X402_FUNDING_KEYPAIR", &kp)
        .env("COVENANT_X402_RPC_URL", &rpc)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sidecar");
    child.stdin.take().unwrap().write_all(&payload).await.unwrap();
    let out = child.wait_with_output().await.unwrap();
    if !out.status.success() {
        eprintln!(
            "sidecar exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
        std::process::exit(1);
    }
    let header = String::from_utf8(out.stdout).unwrap().trim().to_string();
    println!("x-payment (first 60 chars): {}…", &header[..header.len().min(60)]);

    let envelope_bytes = BASE64.decode(header.as_bytes()).expect("base64 decode header");
    let envelope: serde_json::Value =
        serde_json::from_slice(&envelope_bytes).expect("envelope is JSON");
    let tx_b64 = envelope["payload"]["transaction"]
        .as_str()
        .expect("envelope.payload.transaction is a string")
        .to_string();
    let envelope_network = envelope["network"].as_str().unwrap_or("?").to_string();
    println!("envelope.network: {envelope_network}");
    println!("envelope.scheme:  {}", envelope["scheme"].as_str().unwrap_or("?"));

    let facilitator_req = FacilitatorRequest::new(
        FacilitatorPayload::new(envelope_network, tx_b64),
        FacilitatorRequirements {
            scheme: "exact".into(),
            network: "solana".into(),
            max_amount_required: TARGET_AMOUNT_ATOMIC.into(),
            resource: TARGET_URL.into(),
            description: "TVL snapshot (verify-only probe)".into(),
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

    println!("\n=== POST /verify (free, no co-sign) ===");
    let client = FacilitatorClient::new(reqwest::Client::new());
    let resp = client.verify(&facilitator_req).await.expect("verify");
    println!("isValid:        {}", resp.is_valid);
    println!("invalidReason:  {:?}", resp.invalid_reason);
    println!("invalidMessage: {:?}", resp.invalid_message);
    println!("payer:          {:?}", resp.payer);

    if !resp.is_valid {
        eprintln!(
            "\nFAIL — PayAI rejected. invalidReason={:?}",
            resp.invalid_reason
        );
        std::process::exit(2);
    }
    println!("\nOK — PayAI accepts our envelope. live_paid_call is safe to run next.");
}
