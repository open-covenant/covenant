//! Live keyless x402 pay-per-call through the AceData connector.
//!
//!   COVENANT_X402_SIGNER=<path to covenant-x402-signer> \
//!   COVENANT_X402_FUNDING_KEYPAIR=~/.config/solana/covenant-agent.json \
//!   COVENANT_X402_RPC_URL=<mainnet rpc> \
//!   cargo run -p covenant-acedata --example live_x402
//!
//! Spends real USDC (a fraction of a cent). Drives the exact connector
//! path: AceDataClient::with_x402 -> post -> X402Payer -> SubprocessSigner.

use std::process::Command;
use std::sync::Arc;

use async_trait::async_trait;
use covenant_acedata::{AceDataClient, X402Payer};
use covenant_x402::{PaymentRequirements, Signer, X402Error};

struct SidecarSigner {
    bin: String,
    keypair: String,
    rpc: String,
}

#[async_trait]
impl Signer for SidecarSigner {
    async fn build_payment(&self, r: &PaymentRequirements) -> covenant_x402::Result<String> {
        let input = serde_json::to_string(r).map_err(|e| X402Error::Sign(e.to_string()))?;
        let out = Command::new(&self.bin)
            .env("COVENANT_X402_FUNDING_KEYPAIR", &self.keypair)
            .env("COVENANT_X402_RPC_URL", &self.rpc)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                use std::io::Write;
                c.stdin.take().unwrap().write_all(input.as_bytes())?;
                c.wait_with_output()
            })
            .map_err(|e| X402Error::Sign(format!("spawn: {e}")))?;
        if !out.status.success() {
            return Err(X402Error::Sign(String::from_utf8_lossy(&out.stderr).into()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

#[tokio::main]
async fn main() {
    let signer = SidecarSigner {
        bin: std::env::var("COVENANT_X402_SIGNER").expect("COVENANT_X402_SIGNER"),
        keypair: std::env::var("COVENANT_X402_FUNDING_KEYPAIR")
            .expect("COVENANT_X402_FUNDING_KEYPAIR"),
        rpc: std::env::var("COVENANT_X402_RPC_URL")
            .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".into()),
    };
    let payer = X402Payer::new(Arc::new(signer));
    let client = AceDataClient::with_x402("https://api.acedata.cloud", payer);
    println!("keyless x402 client (is_x402={})", client.is_x402());

    match client
        .post(
            "/serp/google",
            serde_json::json!({ "query": "covenant solana provenance" }),
        )
        .await
    {
        Ok(v) => {
            let n = v
                .get("organic")
                .and_then(|o| o.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            println!(
                "OK — {n} results; cost: {}",
                v.get("cost").cloned().unwrap_or(serde_json::Value::Null)
            );
        }
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    }
}
