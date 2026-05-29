//! Manual smoke test against the live Hyre API.
//!
//! Fetches the real 402 challenge from production (free — the challenge
//! is returned before any payment) and runs the crate's parser and
//! selector against it, printing what the daemon would hand the signer.
//! Stops before signing: no funding key, no USDC moved.
//!
//!     cargo run -p covenant-hyre --example live_challenge

use covenant_hyre::config::{HyreConfig, PAY_TO, SOLANA_NETWORK, USDC_MINT};
use covenant_hyre::{parse_challenge, x402::select};

#[tokio::main]
async fn main() {
    let cfg = HyreConfig::default();
    let http = reqwest::Client::new();

    let probes: [(&str, reqwest::Method, &str, Option<serde_json::Value>); 2] = [
        ("defi/tvl", reqwest::Method::GET, "/defi/tvl", None),
        (
            "ask",
            reqwest::Method::POST,
            "/ask",
            Some(serde_json::json!({ "query": "top whales on bonk?" })),
        ),
    ];

    for (slug, method, path, body) in probes {
        let url = format!("{}{}", cfg.base_url, path);
        println!("\n=== {method} {url} ===");
        let mut req = http.request(method, &url);
        if let Some(b) = &body {
            req = req.json(b);
        }
        let resp = req.send().await.expect("request");
        println!("status: {}", resp.status());
        if resp.status().as_u16() != 402 {
            println!(
                "  (expected 402; got {}). body: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            );
            continue;
        }
        let challenge = resp.text().await.expect("body");
        let accepts = parse_challenge(&challenge).expect("parse live challenge");
        println!("  parsed {} payment option(s)", accepts.len());

        // The cap defaults to the published price for this slug; use a
        // cap that admits it so selection succeeds.
        let cap = accepts
            .iter()
            .filter_map(|a| a.atomic_amount().and_then(|s| s.parse::<u128>().ok()))
            .max()
            .unwrap_or(0);
        match select(&accepts, &cfg.network, &cfg.asset, cap) {
            Some(a) => {
                let amount = a.atomic_amount().unwrap_or("?");
                let fee_payer = a
                    .extra
                    .as_ref()
                    .and_then(|e| e.fee_payer.as_deref())
                    .unwrap_or("(none)");
                println!(
                    "  selected: amount={amount} atomic USDC  payTo={}  network={}",
                    a.pay_to, a.network
                );
                println!("            feePayer(sponsored gas)={fee_payer}");
                println!(
                    "  pins:   payTo {} | asset {} | feePayer {} | network compatible with {}",
                    pass(a.pay_to == PAY_TO),
                    pass(a.asset == USDC_MINT),
                    pass(fee_payer == covenant_hyre::config::PAYAI_FEE_PAYER),
                    SOLANA_NETWORK
                );
            }
            None => println!("  NO MATCH for {slug} on {} / {}", cfg.network, cfg.asset),
        }
    }
    println!("\ndone — parser + selector validated against live Hyre.");
}

fn pass(ok: bool) -> &'static str {
    if ok {
        "OK"
    } else {
        "MISMATCH"
    }
}
