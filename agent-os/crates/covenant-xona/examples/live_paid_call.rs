//! End-to-end live smoke against Xona Agent's production API, settling
//! self-paid on Solana (no sponsor feePayer).
//!
//! Dry-run by default: builds the catalog, resolves the target endpoint,
//! fetches its 402 challenge, picks the matching option, and prints what
//! would be signed and posted. Pass `--confirm` (or set
//! `COVENANT_XONA_CONFIRM=1`) to actually re-POST the signed request and
//! move USDC.
//!
//!     COVENANT_X402_FUNDING_KEYPAIR=~/.config/solana/some.json \
//!     COVENANT_X402_RPC_URL=https://api.mainnet-beta.solana.com \
//!     COVENANT_X402_SIGNER_BIN=/path/to/covenant-x402-signer \
//!     cargo run -p covenant-xona --example live_paid_call -- --confirm
//!
//! Targets `image/creative-director` ($0.03) — the endpoint the original
//! covenant-x402 `xona-demo` proved end-to-end.

use std::process::Stdio;

use async_trait::async_trait;
use covenant_x402::{
    orbit::{DEFAULT_BASE_URL, DEFAULT_PAGE_SIZE},
    OrbitClient, PaymentRequirements, Result as X402Result, Signer, X402Error,
};
use covenant_xona::config::{PAY_TO, SOLANA_NETWORK, USDC_MINT};
use covenant_xona::{execute_paid, PaidRequest, XonaCatalog, XonaConfig};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const TARGET_SLUG: &str = "image/creative-director";
const PROMPT: &str = "a single neon koi over a black background, minimal";

/// Signs by shelling out to the built `covenant-x402-signer` sidecar —
/// the same binary the daemon spawns. With no `feePayer` in the
/// requirement the sidecar self-pays via `SolanaSigner`.
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
        || std::env::var("COVENANT_XONA_CONFIRM").ok().as_deref() == Some("1");

    let cfg = XonaConfig {
        enabled: true,
        ..Default::default()
    };

    // Resolve the endpoint through the live registry (vendored fallback),
    // exactly as the daemon does at boot.
    println!("--- step 0: build the Xona catalog ---");
    let orbit = OrbitClient::with(
        reqwest::Client::new(),
        DEFAULT_BASE_URL.to_string(),
        DEFAULT_PAGE_SIZE,
    );
    let catalog = match XonaCatalog::refresh(&orbit, &cfg).await {
        Ok(c) if !c.is_empty() => {
            println!(
                "catalog: {} Solana endpoints (live registry)",
                c.endpoints().len()
            );
            c
        }
        _ => {
            let c = XonaCatalog::from_vendored(&cfg).expect("vendored snapshot");
            println!(
                "catalog: {} Solana endpoints (vendored fallback)",
                c.endpoints().len()
            );
            c
        }
    };
    let ep = catalog
        .endpoints()
        .iter()
        .find(|e| e.slug == TARGET_SLUG)
        .unwrap_or_else(|| panic!("catalog has no Solana endpoint for {TARGET_SLUG}"))
        .clone();

    let per_call_cap = ep.price_micro_usdc.max(1);
    println!("\ntarget:  POST {}", ep.url);
    println!("net:     {SOLANA_NETWORK}");
    println!("asset:   {USDC_MINT}");
    println!("pay_to:  {} (config-pinned {PAY_TO})", ep.pay_to);
    println!(
        "cap:     {per_call_cap} atomic USDC ({:.4} USDC)",
        per_call_cap as f64 / 1e6
    );

    let http = reqwest::Client::new();
    let body = serde_json::json!({ "prompt": PROMPT });

    println!("\n--- step 1: fetch the unpaid 402 challenge (free) ---");
    let probe = http.post(&ep.url).json(&body).send().await.expect("POST");
    println!("status: {}", probe.status());
    let challenge = probe.text().await.unwrap();
    println!("challenge body: {}", &challenge[..challenge.len().min(400)]);
    let options = covenant_xona::parse_challenge(&challenge).expect("parse");
    println!("parsed {} option(s)", options.len());
    let chosen = covenant_xona::x402::select(&options, &cfg.network, &cfg.asset, per_call_cap)
        .expect("at least one option within cap");
    println!("picked: amount={}  payTo={}", chosen.amount, chosen.pay_to);

    if chosen.pay_to != ep.pay_to {
        panic!(
            "REFUSING: 402 advertised payTo={} but registry-advertised payee={}",
            chosen.pay_to, ep.pay_to
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
        provider: "xona".into(),
        slug: ep.slug.clone(),
        url: ep.url.clone(),
        method: ep.method.clone(),
        body: Some(body),
        network: cfg.network.clone(),
        asset: cfg.asset.clone(),
        per_call_cap,
        credits: ep.credits(),
        price_micro_usdc: ep.price_micro_usdc,
        pay_to: ep.pay_to.clone(),
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
            println!("\nOK — paid {amount} atomic USDC against {}.", ep.pay_to);
        } else {
            println!(
                "\nFAIL — Xona returned {} after we paid. Investigate.",
                out.status
            );
            std::process::exit(2);
        }
    } else {
        println!(
            "\nNo payment recorded — Xona returned {} on the first POST (free).",
            out.status
        );
    }
}
