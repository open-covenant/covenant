//! Live check: drive `EphemeralSigner` against a real MagicBlock ER validator.
//!
//! The credit account must already be delegated (see the settlement-ephemeral
//! spike's `er-session.mjs delegate`). This calls `build_payment`, which submits a
//! `consume_credits` to the ER and returns the x402 envelope, then prints it.
//!
//!   cargo run -p covenant-x402 --features solana --example ephemeral_live
//!
//! Env: PAYER (keypair, default ~/.config/solana/id.json), PROGRAM (default
//! cov9UDyp…), ER (default devnet-eu), AMOUNT (default 3), NONCE (default a fixed hex).

use std::env;
use std::str::FromStr;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use covenant_x402::{ephemeral::EphemeralSigner, PaymentExtra, PaymentRequirements, Signer};
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
    let amount = env::var("AMOUNT").unwrap_or("3".into());
    let nonce = env::var("NONCE").unwrap_or("deadbeefcafe".into());

    let signer = EphemeralSigner::from_keypair_file(&payer, program, er).expect("build signer");
    println!(
        "payer {} | credit account {}",
        signer.pubkey(),
        signer.credit_account()
    );

    let req = PaymentRequirements {
        network: "solana-er:devnet".into(),
        asset: "credits".into(),
        amount: amount.clone(),
        amount_usdc: 0.0,
        pay_to: "credits".into(),
        scheme: "exact-er".into(),
        extra: Some(PaymentExtra {
            fee_payer: None,
            nonce: Some(nonce.clone()),
        }),
    };

    println!("metering {amount} credits in the ER (nonce {nonce})...");
    let header = signer.build_payment(&req).await.expect("build_payment");
    let json: serde_json::Value =
        serde_json::from_slice(&BASE64.decode(&header).expect("b64")).expect("json");
    println!("x-payment envelope:\n{}", serde_json::to_string_pretty(&json).unwrap());
    println!("\nER consume signature: {}", json["payload"]["signature"]);
}
