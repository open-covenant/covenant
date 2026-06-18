//! Live zauth RepoScan, end to end through covenant-zauth.
//!
//! Pays the x402 challenge via the covenant-x402-signer sidecar and
//! prints the scan result.
//!
//! Usage:
//!   ZAUTH_SIGNER_BIN=.../target/release/covenant-x402-signer \
//!   ZAUTH_KEYPAIR=~/.config/solana/solana-id.json \
//!   ZAUTH_RPC=https://api.mainnet-beta.solana.com \
//!   cargo run -p covenant-zauth --example live_reposcan -- https://github.com/open-covenant/covenant

use std::io::Write;
use std::process::{Command, Stdio};

use async_trait::async_trait;
use covenant_x402::{PaymentRequirements, Signer};
use covenant_zauth::{assets, networks, treasury, RepoScanClient, RepoScanRequest};

struct SubprocessSigner {
    bin: String,
    keypair: String,
    rpc: String,
}

#[async_trait]
impl Signer for SubprocessSigner {
    async fn build_payment(&self, requirements: &PaymentRequirements) -> covenant_x402::Result<String> {
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
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(json.as_bytes())
            .map_err(|e| covenant_x402::X402Error::Sign(format!("write signer stdin: {e}")))?;
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
    let bin = std::env::var("ZAUTH_SIGNER_BIN").expect("set ZAUTH_SIGNER_BIN");
    let keypair = std::env::var("ZAUTH_KEYPAIR").expect("set ZAUTH_KEYPAIR");
    let rpc =
        std::env::var("ZAUTH_RPC").unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".into());
    let repo = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://github.com/open-covenant/covenant".into());

    eprintln!("reposcan: {repo}");
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("http client");
    let client = RepoScanClient::new(http);
    let req = RepoScanRequest {
        repo_url: &repo,
        network: networks::SOLANA_MAINNET,
        asset: assets::USDC_SOLANA,
        expected_pay_to: treasury::SOLANA,
        per_call_cap: 50_000,
    };
    let signer = SubprocessSigner { bin, keypair, rpc };
    match client.scan(&req, &signer).await {
        Ok(r) => {
            println!("status: {}", r.status);
            println!("paid_amount: {:?}", r.paid_amount);
            println!("error_detail: {:?}", r.error_detail);
            println!("body: {}", r.body);
        }
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    }
}
