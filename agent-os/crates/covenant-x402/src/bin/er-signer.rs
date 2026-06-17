//! ER x402 signer sidecar.
//!
//! Matches the daemon's `SubprocessSigner` contract: reads a `PaymentRequirements`
//! JSON on stdin, runs `consume_credits` in the ER via [`EphemeralSigner`], and
//! writes the resulting `x-payment` header to stdout. The funding key, settlement
//! program, and ER RPC come from this process's own env, so they never enter the
//! daemon's address space.
//!
//! Env: COVENANT_X402_ER_KEYPAIR (Solana CLI keypair json), COVENANT_X402_ER_PROGRAM
//! (settlement program id), COVENANT_X402_ER_RPC (pinned ER validator RPC).

use std::io::Read;
use std::process::exit;
use std::str::FromStr;

use covenant_x402::{ephemeral::EphemeralSigner, PaymentRequirements, Signer};
use solana_sdk::pubkey::Pubkey;

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{e}");
        exit(1);
    }
}

async fn run() -> Result<(), String> {
    let keypair = env_var("COVENANT_X402_ER_KEYPAIR")?;
    let program = env_var("COVENANT_X402_ER_PROGRAM")?;
    let rpc = env_var("COVENANT_X402_ER_RPC")?;
    let program = Pubkey::from_str(&program).map_err(|e| format!("bad program id: {e}"))?;

    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| format!("read stdin: {e}"))?;
    let req: PaymentRequirements =
        serde_json::from_str(&input).map_err(|e| format!("decode requirement: {e}"))?;

    let signer = EphemeralSigner::from_keypair_file(&keypair, program, rpc)
        .map_err(|e| format!("build signer: {e}"))?;
    let header = signer
        .build_payment(&req)
        .await
        .map_err(|e| format!("sign: {e}"))?;
    println!("{header}");
    Ok(())
}

fn env_var(key: &str) -> Result<String, String> {
    std::env::var(key).map_err(|_| format!("{key} not set"))
}
