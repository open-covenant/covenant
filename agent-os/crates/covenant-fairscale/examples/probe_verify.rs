//! Debug probe: build an x402 payment for FairScale's live 402 challenge and
//! POST it straight to the facilitator's `/verify` endpoint to get a
//! structured rejection reason instead of a blind re-402.
//!
//! Signs a real mainnet transaction but never calls `/settle`, so no funds
//! move. Still gated behind `FAIRSCALE_LIVE=1` because it touches production
//! endpoints and a real key:
//!
//! ```sh
//! FAIRSCALE_LIVE=1 \
//! COVENANT_X402_FUNDING_KEYPAIR=$HOME/.config/solana/covenant-agent.json \
//! cargo run -p covenant-fairscale --features solana-example --example probe_verify
//! ```

use base64::Engine;
use covenant_x402::{PayaiSolanaSigner, PaymentRequirements, Signer};

const FACILITATOR_VERIFY_URL: &str = "https://x402.dexter.cash/verify";

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if std::env::var("FAIRSCALE_LIVE").ok().as_deref() != Some("1") {
        eprintln!("refusing: set FAIRSCALE_LIVE=1 to run this live probe");
        std::process::exit(2);
    }
    let wallet = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "AdChcSmDKX57rU9qChMJ3MKnqNZbmiQAjuns9VCjzqRb".to_string());
    let keypair = std::env::var("COVENANT_X402_FUNDING_KEYPAIR")
        .expect("COVENANT_X402_FUNDING_KEYPAIR: path to a Solana CLI keypair JSON");
    let rpc = std::env::var("COVENANT_X402_RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    let http = reqwest::Client::new();

    // 1. Grab the live 402 challenge.
    let resp = http
        .get(format!(
            "https://agent-api.fairscale.xyz/v1/score?wallet={wallet}"
        ))
        .send()
        .await
        .expect("GET score");
    assert_eq!(resp.status().as_u16(), 402, "expected a 402 challenge");
    let challenge: serde_json::Value = resp.json().await.expect("402 body as JSON");
    let accepts0 = challenge["accepts"][0].clone();
    println!(
        "=== challenge accepts[0] ===\n{}",
        serde_json::to_string_pretty(&accepts0).unwrap()
    );

    // 2. Parse requirements. The live challenge omits `amountUsdc`, which our
    //    struct requires; inject the display value before deserializing.
    let mut for_parse = accepts0.clone();
    if for_parse.get("amountUsdc").is_none() {
        for_parse["amountUsdc"] = serde_json::json!(0.005);
    }
    let req: PaymentRequirements =
        serde_json::from_value(for_parse).expect("parse accepts[0] as PaymentRequirements");

    // 3. Build the exact payment envelope the failing client run sends.
    let signer = PayaiSolanaSigner::from_keypair_file(&keypair, rpc)
        .expect("load funding keypair")
        .network_verbatim(true)
        .x402_version(2);
    let header = signer.build_payment(&req).await.expect("build payment");
    let payload: serde_json::Value = serde_json::from_slice(
        &base64::engine::general_purpose::STANDARD
            .decode(header.as_bytes())
            .expect("header should be base64"),
    )
    .expect("payload should be JSON");
    println!(
        "=== decoded X-PAYMENT envelope ===\n{}",
        serde_json::to_string_pretty(&payload).unwrap()
    );

    // 4. Ask the facilitator directly what it thinks of it.
    let body = serde_json::json!({
        "x402Version": 2,
        "paymentPayload": payload,
        "paymentRequirements": accepts0,
    });
    let v = http
        .post(FACILITATOR_VERIFY_URL)
        .json(&body)
        .send()
        .await
        .expect("POST /verify");
    let status = v.status();
    let text = v.text().await.unwrap_or_default();
    println!("=== facilitator /verify ===\nstatus: {status}\nbody: {text}");

    // 5. Replay the same header against the resource server while the
    //    blockhash is still fresh, and dump everything it sends back.
    let paid = http
        .get(format!(
            "https://agent-api.fairscale.xyz/v1/score?wallet={wallet}"
        ))
        .header("X-PAYMENT", header.trim())
        .send()
        .await
        .expect("paid GET");
    println!("=== paid retry ===\nstatus: {}", paid.status());
    for (k, v) in paid.headers() {
        println!("hdr {k}: {}", v.to_str().unwrap_or("<bin>"));
    }
    println!("body: {}", paid.text().await.unwrap_or_default());
}
