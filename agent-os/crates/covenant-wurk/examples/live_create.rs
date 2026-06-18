//! Live WURK agent-to-human create, end to end through covenant-wurk.
//!
//! Drives the real `execute_paid` 402-then-pay loop against WURK's
//! production endpoint, signing with the funding key via the
//! covenant-x402-signer sidecar (so no Solana deps enter this crate).
//!
//! Usage:
//!   WURK_SIGNER_BIN=.../target/release/covenant-x402-signer \
//!   WURK_KEYPAIR=~/.config/solana/solana-id.json \
//!   WURK_RPC=https://api.mainnet-beta.solana.com \
//!   cargo run -p covenant-wurk --example live_create -- "<description>" <winners> <perUser>

use std::io::Write;
use std::process::{Command, Stdio};

use async_trait::async_trait;
use covenant_wurk::{config, execute_paid, PaidRequest};
use covenant_x402::{PaymentRequirements, Signer};

/// Signs by piping the requirements to the standalone signer sidecar.
struct SubprocessSigner {
    bin: String,
    keypair: String,
    rpc: String,
}

#[async_trait]
impl Signer for SubprocessSigner {
    async fn build_payment(
        &self,
        requirements: &PaymentRequirements,
    ) -> covenant_x402::Result<String> {
        let json = serde_json::to_string(requirements)
            .map_err(|e| covenant_x402::X402Error::Sign(e.to_string()))?;
        let mut child = Command::new(&self.bin)
            .env("COVENANT_X402_FUNDING_KEYPAIR", &self.keypair)
            .env("COVENANT_X402_RPC_URL", &self.rpc)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| covenant_x402::X402Error::Sign(format!("spawn signer: {e}")))?;
        {
            let mut stdin = child.stdin.take().expect("piped stdin");
            stdin
                .write_all(json.as_bytes())
                .map_err(|e| covenant_x402::X402Error::Sign(format!("write signer stdin: {e}")))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| covenant_x402::X402Error::Sign(format!("wait signer: {e}")))?;
        if !out.status.success() {
            return Err(covenant_x402::X402Error::Sign(format!(
                "signer exited {:?}",
                out.status.code()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[tokio::main]
async fn main() {
    let bin = std::env::var("WURK_SIGNER_BIN").expect("set WURK_SIGNER_BIN");
    let keypair = std::env::var("WURK_KEYPAIR").expect("set WURK_KEYPAIR");
    let rpc =
        std::env::var("WURK_RPC").unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".into());

    let args: Vec<String> = std::env::args().collect();
    let description = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "Covenant x402 connectivity test. Please reply: ok".into());
    let winners: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    let per_user: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.01);
    let cap = (winners as f64 * per_user * 1_000_000.0).round() as u128;

    let url = format!(
        "{}/solana/agenttohuman?description={}&winners={}&perUser={}",
        config::BASE_URL,
        encode(&description),
        winners,
        per_user
    );
    eprintln!("create: {url}\ncap atomic: {cap}");

    let plan = PaidRequest {
        provider: "wurk".into(),
        slug: "agenttohuman".into(),
        url,
        method: "GET".into(),
        body: None,
        network: config::SOLANA_NETWORK.into(),
        asset: config::USDC_MINT.into(),
        per_call_cap: cap,
    };

    let signer = SubprocessSigner { bin, keypair, rpc };
    let http = reqwest::Client::new();
    match execute_paid(&http, &signer, &plan).await {
        Ok(r) => {
            println!("status: {}", r.status);
            println!("paid_amount: {:?}", r.paid_amount);
            println!("body: {}", r.body);
        }
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    }
}
