//! One live paid FairScale credit read, end to end, against production.
//!
//! Settles the x402 challenge with real USDC on Solana mainnet ($0.50 at the
//! 2026-07-20 quote — a hundred times a score read), and runs only when
//! explicitly asked:
//!
//! ```sh
//! FAIRSCALE_LIVE=1 \
//! COVENANT_X402_FUNDING_KEYPAIR=$HOME/.config/solana/id.json \
//! cargo run -p covenant-fairscale --features solana-example --example live_credit_probe -- <wallet> [amount_usd]
//! ```
//!
//! `COVENANT_X402_RPC_URL` overrides the RPC (default: the public mainnet
//! endpoint). FairScale's facilitator is the fee payer, so the funder signs
//! only the USDC transfer. `amount_usd` is the credit line being probed
//! (default 1000, matching the daemon's `CREDIT_PROBE_USD`), not the price.

use std::sync::Arc;

use covenant_fairscale::{FairScaleClient, AGENT_BASE_URL, REPUTATION_BASE_URL};
use covenant_x402::PayaiSolanaSigner;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if std::env::var("FAIRSCALE_LIVE").ok().as_deref() != Some("1") {
        eprintln!("refusing to spend: set FAIRSCALE_LIVE=1 to run the paid credit read (~$0.50)");
        std::process::exit(2);
    }
    let wallet = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "AdChcSmDKX57rU9qChMJ3MKnqNZbmiQAjuns9VCjzqRb".to_string());
    let amount_usd: u64 = std::env::args()
        .nth(2)
        .map(|s| s.parse().expect("amount_usd: whole USD"))
        .unwrap_or(1000);
    let keypair = std::env::var("COVENANT_X402_FUNDING_KEYPAIR")
        .expect("COVENANT_X402_FUNDING_KEYPAIR: path to a Solana CLI keypair JSON");
    let rpc = std::env::var("COVENANT_X402_RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    let signer = PayaiSolanaSigner::from_keypair_file(&keypair, rpc)
        .expect("load funding keypair")
        .network_verbatim(true)
        .x402_version(2);

    let client = FairScaleClient::new(REPUTATION_BASE_URL, AGENT_BASE_URL).with_payer(
        Arc::new(signer),
        10_000,
        600_000,
    );

    let credit = client
        .credit(&wallet, amount_usd)
        .await
        .expect("paid credit read");
    println!(
        "credit: wallet={wallet} amountUsd={amount_usd} creditScore={:?} riskBand={:?}",
        credit.credit_score(),
        credit.risk_band()
    );
    println!("lending terms: {:?}", credit.lending_terms());
    println!("raw: {}", serde_json::to_string_pretty(&credit.raw).unwrap());
}
