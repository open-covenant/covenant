//! End-to-end live smoke against Hyre's production API, settling
//! through the PayAI facilitator.
//!
//! Dry-run by default: fetches the 402 challenge, picks an option, and
//! prints what would be signed and posted. Pass `--confirm` (or set
//! `COVENANT_HYRE_CONFIRM=1`) to actually re-POST the signed request
//! and move USDC. Captures the on-chain signature from the response.
//!
//!     COVENANT_X402_FUNDING_KEYPAIR=~/.config/solana/some.json \
//!     COVENANT_X402_RPC_URL=https://api.mainnet-beta.solana.com \
//!     COVENANT_X402_SIGNER_BIN=/path/to/covenant-x402-signer \
//!     cargo run -p covenant-hyre --example live_paid_call -- --confirm
//!
//! Picks the cheapest endpoint (`/defi/tvl` is $0.01) so the smoke
//! debits the smallest amount Hyre's catalog offers.

use std::process::Stdio;

use async_trait::async_trait;
use covenant_hyre::config::{HyreConfig, PAY_TO, SOLANA_NETWORK, USDC_MINT};
use covenant_hyre::{execute_paid, PaidRequest};
use covenant_x402::{PaymentRequirements, Result as X402Result, Signer, X402Error};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const TARGET_PATH: &str = "/defi/tvl";
const TARGET_CREDITS: u64 = 1;
const PER_CALL_CAP: u128 = 50_000;

struct SubprocessSigner {
    program: String,
    env: Vec<(String, String)>,
}

#[async_trait]
impl Signer for SubprocessSigner {
    async fn build_payment(&self, req: &PaymentRequirements) -> X402Result<String> {
        let payload = serde_json::to_vec(req)
            .map_err(|e| X402Error::Sign(format!("encode requirement: {e}")))?;
        let mut child = Command::new(&self.program)
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| X402Error::Sign(format!("spawn {}: {e}", self.program)))?;
        child
            .stdin
            .take()
            .ok_or_else(|| X402Error::Sign("stdin unavailable".into()))?
            .write_all(&payload)
            .await
            .map_err(|e| X402Error::Sign(format!("write stdin: {e}")))?;
        let out = child
            .wait_with_output()
            .await
            .map_err(|e| X402Error::Sign(format!("await child: {e}")))?;
        if !out.status.success() {
            return Err(X402Error::Sign(format!(
                "signer exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let header = String::from_utf8(out.stdout)
            .map_err(|e| X402Error::Sign(format!("non-utf8 stdout: {e}")))?
            .trim()
            .to_string();
        if header.is_empty() {
            return Err(X402Error::Sign("signer returned empty header".into()));
        }
        Ok(header)
    }
}

#[tokio::main]
async fn main() {
    let confirm = std::env::args().any(|a| a == "--confirm")
        || std::env::var("COVENANT_HYRE_CONFIRM").ok().as_deref() == Some("1");

    let cfg = HyreConfig::default();
    let url = format!("{}{}", cfg.base_url, TARGET_PATH);
    println!("target:  GET {url}");
    println!("net:     {} ({})", cfg.network, SOLANA_NETWORK);
    println!("asset:   {} ({})", cfg.asset, USDC_MINT);
    println!("pay_to:  {PAY_TO}");
    println!(
        "cap:     {PER_CALL_CAP} atomic USDC ({:.4} USDC)",
        PER_CALL_CAP as f64 / 1e6
    );

    let http = reqwest::Client::new();

    println!("\n--- step 1: fetch the unpaid 402 challenge (free) ---");
    let probe = http.get(&url).send().await.expect("GET");
    println!("status: {}", probe.status());
    let challenge = probe.text().await.unwrap();
    println!("challenge body: {}", &challenge[..challenge.len().min(400)]);
    let accepts = covenant_hyre::parse_challenge(&challenge).expect("parse");
    println!("parsed {} accept option(s)", accepts.len());
    let chosen = covenant_hyre::x402::select(&accepts, &cfg.network, &cfg.asset, PER_CALL_CAP)
        .expect("at least one option within cap");
    println!(
        "picked: amount={}  payTo={}  feePayer={:?}",
        chosen.atomic_amount().unwrap_or("?"),
        chosen.pay_to,
        chosen.extra.as_ref().and_then(|e| e.fee_payer.as_deref()),
    );

    if chosen.pay_to != PAY_TO {
        panic!(
            "REFUSING: 402 advertised pay_to={} but config-pinned PAY_TO={}",
            chosen.pay_to, PAY_TO
        );
    }
    if chosen.asset != USDC_MINT {
        panic!(
            "REFUSING: 402 advertised asset={} but config-pinned USDC_MINT={}",
            chosen.asset, USDC_MINT
        );
    }

    if !confirm {
        println!("\nDRY RUN — pass --confirm to actually pay. Exiting cleanly.");
        return;
    }

    println!("\n--- step 2: sign + POST (real USDC) ---");
    let kp = std::env::var("COVENANT_X402_FUNDING_KEYPAIR")
        .expect("COVENANT_X402_FUNDING_KEYPAIR must be set");
    let rpc = std::env::var("COVENANT_X402_RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".into());
    let signer_bin = std::env::var("COVENANT_X402_SIGNER_BIN")
        .expect("COVENANT_X402_SIGNER_BIN must point to the built sidecar binary");

    let signer = SubprocessSigner {
        program: signer_bin,
        env: vec![
            ("COVENANT_X402_FUNDING_KEYPAIR".into(), kp),
            ("COVENANT_X402_RPC_URL".into(), rpc),
        ],
    };

    let plan = PaidRequest {
        provider: "hyre".into(),
        slug: "defi/tvl".into(),
        url,
        method: "GET".into(),
        body: None,
        network: cfg.network.clone(),
        asset: cfg.asset.clone(),
        per_call_cap: PER_CALL_CAP,
        credits: TARGET_CREDITS,
        price_micro_usdc: chosen
            .atomic_amount()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
    };

    let out = match execute_paid(&http, &signer, &plan).await {
        Ok(o) => o,
        Err(e) => {
            eprintln!("\nFAIL: {e}");
            std::process::exit(1);
        }
    };
    println!("status:        {}", out.status);
    println!("paid_amount:   {:?} atomic USDC", out.paid_amount);
    println!("response body: {}", &out.body[..out.body.len().min(1000)]);

    if let Some(amount) = out.paid_amount {
        if (200..300).contains(&out.status) {
            println!(
                "\nOK — paid {amount} atomic USDC against {}.",
                chosen.pay_to
            );
            println!("Look at Hyre's response headers for an x-payment-response hop if you need the on-chain sig.");
        } else {
            println!(
                "\nFAIL — Hyre returned {} after we paid. Investigate.",
                out.status
            );
            std::process::exit(2);
        }
    } else {
        println!(
            "\nNo payment recorded — Hyre returned {} on the first GET (free).",
            out.status
        );
    }
}
