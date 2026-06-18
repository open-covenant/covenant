//! Live Syra paid call, end to end through covenant-syra.
//!
//! Drives the real `execute_paid` 402-then-pay loop against Syra's
//! production endpoint, signing via the covenant-x402-signer sidecar.
//!
//! Usage:
//!   SYRA_SIGNER_BIN=.../target/release/covenant-x402-signer \
//!   SYRA_KEYPAIR=~/.config/solana/solana-id.json \
//!   SYRA_RPC=https://api.mainnet-beta.solana.com \
//!   cargo run -p covenant-syra --example live_call -- /health

use std::io::Write;
use std::process::{Command, Stdio};

use async_trait::async_trait;
use covenant_syra::{config, execute_paid, PaidRequest};
use covenant_x402::{PaymentRequirements, Signer};

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

#[tokio::main]
async fn main() {
    let bin = std::env::var("SYRA_SIGNER_BIN").expect("set SYRA_SIGNER_BIN");
    let keypair = std::env::var("SYRA_KEYPAIR").expect("set SYRA_KEYPAIR");
    let rpc =
        std::env::var("SYRA_RPC").unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".into());

    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).cloned().unwrap_or_else(|| "/health".into());
    let url = format!("{}{}", config::BASE_URL, path);
    eprintln!("call: {url}");

    let plan = PaidRequest {
        provider: "syra".into(),
        slug: path.trim_start_matches('/').into(),
        url,
        method: "GET".into(),
        body: None,
        network: config::SOLANA_NETWORK.into(),
        asset: config::USDC_MINT.into(),
        per_call_cap: config::SyraConfig::default().per_call_cap,
    };

    let signer = SubprocessSigner { bin, keypair, rpc };
    // Fail fast if Syra's upstream stalls (its data routes flap 503),
    // instead of hanging on a request with no deadline.
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("build http client");
    match execute_paid(&http, &signer, &plan).await {
        Ok(r) => {
            println!("status: {}", r.status);
            println!("paid_amount: {:?}", r.paid_amount);
            println!("body: {}", r.body);
        }
        Err(e) => {
            eprintln!("ERROR: {e}");
            let mut src = std::error::Error::source(&e);
            while let Some(s) = src {
                eprintln!("  caused by: {s}");
                src = s.source();
            }
            std::process::exit(1);
        }
    }
}
