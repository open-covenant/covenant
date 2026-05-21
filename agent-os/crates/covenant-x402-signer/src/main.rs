//! Standalone x402 funding-key signer (sidecar to covenantd).
//!
//! One-shot, stdin→stdout. The daemon spawns this process per paid
//! call, pipes the chosen [`PaymentRequirements`] as JSON to stdin,
//! and reads the resulting `x-payment` header from stdout. The funding
//! key never enters the daemon's address space, and the Solana dep
//! tree never enters the daemon's build.
//!
//! Protocol:
//! - stdin:  a single JSON [`PaymentRequirements`] object.
//! - stdout: the `x-payment` header value (one line) on success.
//! - exit 0 on success; non-zero with a message on stderr otherwise.
//!
//! Configuration (env):
//! - `COVENANT_X402_FUNDING_KEYPAIR` — path to the Solana keypair JSON
//!   that funds payments. Required.
//! - `COVENANT_X402_RPC_URL` — Solana RPC for the blockhash + mint
//!   decimals lookup. Defaults to mainnet-beta.

use std::process::ExitCode;

use covenant_x402::{PaymentRequirements, Signer, SolanaSigner};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    match run().await {
        Ok(header) => {
            let mut stdout = tokio::io::stdout();
            if let Err(e) = stdout.write_all(header.as_bytes()).await {
                eprintln!("covenant-x402-signer: write stdout: {e}");
                return ExitCode::FAILURE;
            }
            let _ = stdout.write_all(b"\n").await;
            let _ = stdout.flush().await;
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("covenant-x402-signer: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<String, Box<dyn std::error::Error>> {
    let keypair_path = std::env::var("COVENANT_X402_FUNDING_KEYPAIR")
        .map_err(|_| "COVENANT_X402_FUNDING_KEYPAIR is not set")?;
    let rpc_url =
        std::env::var("COVENANT_X402_RPC_URL").unwrap_or_else(|_| DEFAULT_RPC_URL.to_string());

    let mut input = String::new();
    tokio::io::stdin().read_to_string(&mut input).await?;
    let requirement: PaymentRequirements = serde_json::from_str(input.trim())
        .map_err(|e| format!("decode PaymentRequirements from stdin: {e}"))?;

    let signer = SolanaSigner::from_keypair_file(&keypair_path, rpc_url)?;
    let header = signer.build_payment(&requirement).await?;
    Ok(header)
}
