//! End-to-end demo of the covenant-x402 crate against Xona on Solana.
//!
//! ⚠️  Defaults to MAINNET. Each successful run charges real USDC
//! from the configured keypair to the endpoint's payTo address.
//!
//! Prereqs (mainnet defaults):
//! - Keypair file at `$XONA_KEYPAIR` (default `~/.config/solana/id.json`)
//! - Keypair funded with mainnet SOL for tx fees (a few thousand
//!   lamports is enough)
//! - Keypair funded with mainnet USDC for the call cost (the demo
//!   prints the exact pricing before signing)
//!
//! The recipient's USDC ATA is created idempotently if missing (the
//! payer covers the small rent), so no out-of-band setup is needed.
//!
//! Run:
//! ```bash
//! cargo run -p covenant-x402 --bin xona-demo --features solana
//! ```
//!
//! Configurable env vars (all optional):
//!
//! - `XONA_KEYPAIR`                  path to Solana CLI keypair JSON
//! - `XONA_RPC_URL`                  Solana JSON-RPC URL
//! - `XONA_NETWORK`                  CAIP-2 chain id to settle on
//! - `XONA_ASSET`                    SPL mint address to pay in
//! - `XONA_SLUG`                     endpoint slug to call
//! - `XONA_PROMPT`                   request body prompt
//! - `XONA_PER_CALL_CAP_USDC_ATOMIC` max atomic units to spend on one call
//!
//! To run against devnet instead, override all four of:
//! `XONA_RPC_URL`, `XONA_NETWORK`, `XONA_ASSET`, plus a keypair
//! funded with devnet USDC. Xona's catalog may not list devnet
//! endpoints — if discovery returns no match, file a request with
//! the Xona team.

use std::path::PathBuf;
use std::process::ExitCode;

use covenant_x402::solana::USDC_MAINNET_MINT;
use covenant_x402::{Catalog, Client, OrbitClient, PaidRequest, SolanaSigner};
use solana_sdk::signer::keypair::read_keypair_file;
use solana_sdk::signer::Signer as _;

/// Solana mainnet-beta CAIP-2 chain id.
const SOLANA_MAINNET_NETWORK: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";
const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";
const DEFAULT_SLUG: &str = "image/creative-director";
const DEFAULT_PROMPT: &str = "a cyberpunk cat sitting on a rooftop at dusk";
/// $1.00 USDC. Generous default for a one-shot demo; override for
/// tighter budgets in repeated runs.
const DEFAULT_PER_CALL_CAP: u128 = 1_000_000;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xona-demo failed: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::from_env();
    cfg.print_banner();

    let keypair = read_keypair_file(&cfg.keypair_path)
        .map_err(|e| format!("read keypair {}: {e}", cfg.keypair_path.display()))?;
    eprintln!("signer pubkey : {}", keypair.pubkey());
    eprintln!();

    eprintln!("Fetching orbit-x402 catalog (this may take ~10s on first run)…");
    let entries = OrbitClient::new().fetch_all().await?;
    let catalog = Catalog::new(entries);
    eprintln!("Catalog loaded: {} entries\n", catalog.len());

    let entry = catalog
        .by_slug(&cfg.slug)
        .ok_or_else(|| format!("orbit catalog has no entry for slug {:?}", cfg.slug))?;
    eprintln!("Endpoint   : {}", entry.endpoint);
    eprintln!("Server     : {}", entry.server_title);
    eprintln!("Description: {}", entry.description);
    eprintln!("Pricing options:");
    for p in &entry.pricing {
        eprintln!(
            "  - {} atomic ({} ≈ {} USDC) on {} → {}",
            p.amount, p.asset, p.amount_usdc, p.network, p.pay_to
        );
    }
    eprintln!();

    // Preview the matching price before paying.
    if let Some(pricing) = entry
        .pricing
        .iter()
        .find(|p| p.network == cfg.network && p.asset == cfg.asset)
    {
        eprintln!(
            "About to pay {} atomic units (≈ {} USDC) and call {}\n",
            pricing.amount, pricing.amount_usdc, entry.endpoint
        );
    }

    let signer = SolanaSigner::new(keypair, cfg.rpc_url);
    let client = Client::new(reqwest::Client::new());
    let body = serde_json::json!({ "prompt": cfg.prompt });

    // One call resolves the endpoint, enforces the cap, and runs the
    // 402-then-pay loop — the same path the daemon uses.
    let resp = catalog
        .discover_and_pay(
            &client,
            &signer,
            &PaidRequest {
                slug: &cfg.slug,
                server_title: None,
                network: &cfg.network,
                asset: &cfg.asset,
                per_call_cap: cfg.per_call_cap,
                provider: "xona",
                body: Some(&body),
            },
        )
        .await?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    println!("--- response ---");
    println!("status: {status}");
    println!("body:");
    println!("{text}");

    Ok(())
}

struct Config {
    keypair_path: PathBuf,
    rpc_url: String,
    network: String,
    asset: String,
    slug: String,
    prompt: String,
    per_call_cap: u128,
}

impl Config {
    fn from_env() -> Self {
        let keypair_path = std::env::var("XONA_KEYPAIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                PathBuf::from(home).join(".config/solana/id.json")
            });
        Self {
            keypair_path,
            rpc_url: env_or("XONA_RPC_URL", DEFAULT_RPC_URL),
            network: env_or("XONA_NETWORK", SOLANA_MAINNET_NETWORK),
            asset: env_or("XONA_ASSET", USDC_MAINNET_MINT),
            slug: env_or("XONA_SLUG", DEFAULT_SLUG),
            prompt: env_or("XONA_PROMPT", DEFAULT_PROMPT),
            per_call_cap: std::env::var("XONA_PER_CALL_CAP_USDC_ATOMIC")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_PER_CALL_CAP),
        }
    }

    fn print_banner(&self) {
        eprintln!("xona-demo configuration:");
        eprintln!("  keypair       : {}", self.keypair_path.display());
        eprintln!("  rpc_url       : {}", self.rpc_url);
        eprintln!("  network       : {}", self.network);
        eprintln!("  asset         : {}", self.asset);
        eprintln!("  slug          : {}", self.slug);
        eprintln!("  per_call_cap  : {} atomic units", self.per_call_cap);
        eprintln!();
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.into())
}
