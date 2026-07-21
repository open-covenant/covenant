//! Two live paid FairScale reads (score + trust-gate), end to end, against
//! production.
//!
//! Settles each x402 challenge with real USDC on Solana mainnet ($0.005 per
//! read at the 2026-07-20 quote, so ~$0.01 for a full run), and runs only
//! when explicitly asked:
//!
//! ```sh
//! FAIRSCALE_LIVE=1 \
//! COVENANT_X402_FUNDING_KEYPAIR=$HOME/.config/solana/id.json \
//! cargo run -p covenant-fairscale --example live_paid_read -- <wallet>
//! ```
//!
//! `COVENANT_X402_RPC_URL` overrides the RPC (default: the public mainnet
//! endpoint). FairScale's facilitator is the fee payer, so the funder signs
//! only the USDC transfer.

use std::sync::Arc;

use covenant_fairscale::{FairScaleClient, AGENT_BASE_URL, REPUTATION_BASE_URL};
use covenant_x402::PayaiSolanaSigner;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if std::env::var("FAIRSCALE_LIVE").ok().as_deref() != Some("1") {
        eprintln!("refusing to spend: set FAIRSCALE_LIVE=1 to run paid reads (~$0.005 each)");
        std::process::exit(2);
    }
    let wallet = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "AdChcSmDKX57rU9qChMJ3MKnqNZbmiQAjuns9VCjzqRb".to_string());
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

    let score = client.score(&wallet).await.expect("paid score read");
    println!(
        "score: wallet={wallet} fairscore={:?} tier={:?}",
        score.fairscore(),
        score.tier()
    );
    println!("raw: {}", serde_json::to_string_pretty(&score.raw).unwrap());

    let gate = client
        .trust_gate(&wallet, 60)
        .await
        .expect("paid trust-gate read");
    println!(
        "trust-gate: decision={} recommendation={} reasons={:?}",
        gate.decision, gate.recommendation, gate.reasons
    );
}
