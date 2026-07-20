//! Probe: which request header name does FairScale's agent-api read the
//! x402 envelope from, if any? The 402 response carries an unprefixed
//! `payment-required` response header, so this replays one valid envelope
//! under `payment` and one under `x-payment`, dumping status, headers,
//! and body for each.
//!
//! Result (2026-07-20): byte-identical 402s for both, same ETag as the
//! unpaid challenge; the wall reads neither header. probe_verify holds
//! the facilitator-side proof that the envelope itself is valid.
//!
//! Each attempt costs ~$0.005 only if the server settles it, and a
//! settle means a 200, which is the read we are paying for.
//!
//! ```sh
//! FAIRSCALE_LIVE=1 \
//! COVENANT_X402_FUNDING_KEYPAIR=$HOME/.config/solana/covenant-agent.json \
//! cargo run -p covenant-fairscale --features solana-example --example probe_header_variant
//! ```

use covenant_fairscale::AGENT_BASE_URL;
use covenant_x402::{PayaiSolanaSigner, PaymentRequirements, Signer};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if std::env::var("FAIRSCALE_LIVE").ok().as_deref() != Some("1") {
        eprintln!("refusing to spend: set FAIRSCALE_LIVE=1 to run paid probes (~$0.005 each)");
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

    let url = format!("{AGENT_BASE_URL}/v1/score/{wallet}");
    let http = reqwest::Client::new();

    // 1. Unpaid hit → 402 challenge.
    let resp = http.get(&url).send().await.expect("challenge GET");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("challenge body JSON");
    println!("challenge: status={status}");
    assert_eq!(
        status.as_u16(),
        402,
        "expected 402 challenge, got {status}: {body}"
    );
    let opt = &body["accepts"][0];
    println!("accepts[0]: {opt}");

    let amount = opt["amount"].as_str().unwrap_or_default().to_string();
    let req = PaymentRequirements {
        network: opt["network"].as_str().unwrap_or_default().to_string(),
        asset: opt["asset"].as_str().unwrap_or_default().to_string(),
        amount_usdc: amount.parse::<f64>().unwrap_or_default() / 1e6,
        amount,
        pay_to: opt["payTo"].as_str().unwrap_or_default().to_string(),
        scheme: opt["scheme"].as_str().unwrap_or_default().to_string(),
        extra: serde_json::from_value(opt["extra"].clone()).ok(),
    };

    // 2. Replay under each candidate header name, fresh envelope each time.
    for header_name in ["payment", "x-payment"] {
        let envelope = signer.build_payment(&req).await.expect("build envelope");
        println!(
            "\n=== attempt: header `{header_name}` (envelope {} bytes) ===",
            envelope.len()
        );
        let resp = http
            .get(&url)
            .header(header_name, &envelope)
            .send()
            .await
            .expect("paid GET");
        let status = resp.status();
        println!("status: {status}");
        for (k, v) in resp.headers() {
            println!("  hdr {k}: {}", v.to_str().unwrap_or("<binary>"));
        }
        let text = resp.text().await.unwrap_or_default();
        let snippet: String = text.chars().take(800).collect();
        println!("body: {snippet}");
        if status.as_u16() == 200 {
            println!("\nSUCCESS: server accepted envelope under header `{header_name}`");
            return;
        }
    }
    println!("\nno variant accepted; header name is not the discriminator");
}
