//! Full x402-over-ER loop, live, in Rust: the real `covenant_x402::Client` drives
//! `EphemeralSigner` against a running facilitator and the live ER.
//!
//! Start the facilitator first (`cargo run -p covenant-x402-facilitator`) and make
//! sure the credit account is delegated (spike `er-session.mjs delegate`), then:
//!
//!   cargo run -p covenant-x402-facilitator --example live_loop
//!
//! Env: PAYER, PROGRAM, ER (passed to the signer), URL (facilitator endpoint).

use std::env;
use std::str::FromStr;

use covenant_x402::{ephemeral::EphemeralSigner, http_client, Capability, Client};
use reqwest::Method;
use solana_sdk::pubkey::Pubkey;

#[tokio::main]
async fn main() {
    let home = env::var("HOME").unwrap();
    let payer = env::var("PAYER").unwrap_or(format!("{home}/.config/solana/id.json"));
    let program = Pubkey::from_str(
        &env::var("PROGRAM").unwrap_or("cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y".into()),
    )
    .expect("program id");
    let er = env::var("ER").unwrap_or("https://devnet-eu.magicblock.app".into());
    let url = env::var("URL").unwrap_or("http://127.0.0.1:8402/paid".into());

    let signer = EphemeralSigner::from_keypair_file(&payer, program, er).expect("build signer");
    let client = Client::new(http_client());
    let cap = Capability {
        provider: "covenant-er".into(),
        network: "solana-er:devnet".into(),
        asset: "credits".into(),
        per_call_cap: 1_000_000,
    };

    println!("agent calling {url} (pays per call by metering in the ER)...");
    let resp = client
        .request_paid(Method::GET, &url, None, &cap, &signer)
        .await
        .expect("paid call");
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    println!("-> {status}\n{body}");
    assert!(status.is_success(), "expected 200 after ER-settled payment");
    println!("\nOK — full x402-over-ER loop in Rust: 402 -> ER consume -> 200.");
}
