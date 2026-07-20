//! Flagship demo: a verifiable BlockRun call on Base mainnet.
//!
//! Makes one real paid call to BlockRun over x402 (USDC on Base, EIP-3009
//! gasless), captures the Covenant receipt, verifies it locally, and prints the
//! settlement tx with a Basescan link. This spends real USDC (BlockRun's default
//! chat call is ~0.003 USDC) — it is a `live_` example, never run by `cargo
//! test`.
//!
//! Run:
//!   COVENANT_X402_EVM_KEY_HEX=<64-hex Base secp256k1 secret, funded with USDC> \
//!   cargo run -p covenant-blockrun --example live_verified_call
//!
//! Optional:
//!   BLOCKRUN_MODEL     model to request (default gpt-4o-mini)
//!   BLOCKRUN_PROMPT    prompt text (default a one-word ping)
//!   BLOCKRUN_BASE_URL  API base (default https://blockrun.ai/api)

use std::sync::Arc;

use covenant_blockrun::{BlockRunClient, CallReceipt};
use covenant_x402::EvmSigner;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Prefer a file path so the secret never touches the shell or its history;
    // fall back to an inline hex for one-off runs. Matches the x402 signer sidecar.
    let key_hex = match std::env::var("COVENANT_X402_EVM_KEY") {
        Ok(path) if !path.trim().is_empty() => std::fs::read_to_string(path.trim())
            .map_err(|e| format!("reading COVENANT_X402_EVM_KEY file: {e}"))?,
        _ => std::env::var("COVENANT_X402_EVM_KEY_HEX").map_err(|_| {
            "set COVENANT_X402_EVM_KEY (path to a 64-hex key file) or COVENANT_X402_EVM_KEY_HEX"
        })?,
    };
    let secret = decode_key(key_hex.trim())?;
    let signer = EvmSigner::from_secret_bytes(&secret)?;
    let payer = signer.address_hex();

    let base_url = std::env::var("BLOCKRUN_BASE_URL")
        .unwrap_or_else(|_| "https://blockrun.ai/api".to_string());
    let model = std::env::var("BLOCKRUN_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
    let prompt = std::env::var("BLOCKRUN_PROMPT")
        .unwrap_or_else(|_| "Reply with the single word: ok".into());

    eprintln!("payer (Base): {payer}");
    eprintln!("calling {base_url}/v1/chat/completions with model {model} — this spends real USDC");

    let client = BlockRunClient::new(base_url, Arc::new(signer));
    let body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": prompt }],
        "max_tokens": 16,
    });

    let paid = client.call("/v1/chat/completions", body).await?;
    let receipt = &paid.receipt;

    println!("\n=== response ===");
    println!("{}", serde_json::to_string_pretty(&paid.response)?);

    println!("\n=== covenant receipt ===");
    println!("{}", serde_json::to_string_pretty(&receipt.to_json())?);

    report(receipt);
    Ok(())
}

fn report(receipt: &CallReceipt) {
    println!("\n=== verdict ===");
    println!("requested model : {}", receipt.model_requested);
    println!("served model    : {}", receipt.model_served);
    println!("verdict         : {}", receipt.verdict);
    if let Some(pct) = receipt.routing.savings_pct {
        println!("router savings  : {pct}%");
    }
    println!("receipt digest  : {}", receipt.digest());

    println!("\n=== settlement ===");
    println!("network         : {}", receipt.payment.network);
    println!(
        "amount          : {} ({} USDC)",
        receipt.payment.amount, receipt.payment.amount_usdc
    );
    match &receipt.payment.tx {
        Some(tx) => {
            println!("settlement tx   : {tx}");
            println!("basescan        : https://basescan.org/tx/{tx}");
        }
        None => println!("settlement tx   : (none reported by the provider)"),
    }

    // Independent local check: the stored verdict must match the model pair.
    let ok = receipt.verdict == receipt.expected_verdict();
    println!("\nlocal verify    : {}", if ok { "PASS" } else { "FAIL" });
}

fn decode_key(hex: &str) -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let hex = hex.strip_prefix("0x").unwrap_or(hex);
    if hex.len() != 64 {
        return Err("key must be 64 hex chars (32 bytes)".into());
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}
