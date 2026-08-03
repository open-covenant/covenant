//! Standalone EVM (Base) x402 signer, sidecar to covenantd.
//!
//! One-shot, stdin to stdout, mirroring `covenant-x402-signer` but for EIP-3009
//! USDC payments on Base. [`EvmSigner`] produces a gasless signed authorization,
//! so this needs no RPC and no Solana dep tree; the facilitator submits and pays
//! gas. The secp256k1 key never enters the daemon's address space.
//!
//! Protocol:
//! - stdin:  a single JSON [`PaymentRequirements`] object.
//! - stdout: the `x-payment` header value (one line) on success.
//! - exit 0 on success; non-zero with a message on stderr otherwise.
//!
//! The signer rejects any non-EVM network (a `solana:` requirement fails
//! closed) and any asset that is not the chain-local USDC unless the operator
//! authorized the exact `(chain, asset)` pair. Point
//! `COVENANT_X402_SIGNER_BINARY` at this binary to pay only over Base, or set
//! `COVENANT_X402_EVM_SIGNER_BINARY` beside a Solana sidecar and the daemon
//! routes each chain's challenges to the signer that owns them — only in USDC
//! by default either way.
//!
//! Configuration (env):
//! - `COVENANT_X402_EVM_KEY_HEX` — the 32-byte secp256k1 secret as hex, or
//! - `COVENANT_X402_EVM_KEY` — path to a file holding that hex. One is required.
//! - `COVENANT_X402_EVM_VALID_SECS` — optional authorization lifetime override.
//! - `COVENANT_X402_EVM_EXTRA_ASSETS` — optional comma-separated
//!   `<chainId>:<0xaddress>` pairs authorizing assets beyond the pinned
//!   chain-local USDC; anything else fails closed. A malformed entry aborts
//!   rather than narrowing the allowlist silently.

use std::process::ExitCode;

use covenant_x402::{extra_assets_from_env, EvmSigner, PaymentRequirements, Signer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
                eprintln!("covenant-x402-signer-evm: write stdout: {e}");
                return ExitCode::FAILURE;
            }
            let _ = stdout.write_all(b"\n").await;
            let _ = stdout.flush().await;
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("covenant-x402-signer-evm: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<String, Box<dyn std::error::Error>> {
    let secret = load_secret()?;
    let mut signer =
        EvmSigner::from_secret_bytes(&secret)?.with_extra_assets(extra_assets_from_env()?);
    if let Ok(raw) = std::env::var("COVENANT_X402_EVM_VALID_SECS") {
        let secs: u64 = raw
            .parse()
            .map_err(|_| "COVENANT_X402_EVM_VALID_SECS must be a positive integer")?;
        signer = signer.with_valid_for_secs(secs);
    }

    let mut input = String::new();
    tokio::io::stdin().read_to_string(&mut input).await?;
    let requirement = decode_requirement(&input)?;

    Ok(signer.build_payment(&requirement).await?)
}

/// Load the secp256k1 secret from the inline hex env or the key file, whichever
/// is set. Kept separate from `run` so its parsing is unit-testable.
fn load_secret() -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let key_hex = if let Ok(inline) = std::env::var("COVENANT_X402_EVM_KEY_HEX") {
        inline
    } else {
        let path = std::env::var("COVENANT_X402_EVM_KEY")
            .map_err(|_| "COVENANT_X402_EVM_KEY_HEX or COVENANT_X402_EVM_KEY is required")?;
        std::fs::read_to_string(&path).map_err(|e| format!("read {path}: {e}"))?
    };
    decode_secret(&key_hex)
}

fn decode_secret(value: &str) -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let trimmed = value.trim();
    // The hex error is not echoed: InvalidHexCharacter names a byte of
    // the operator's key input, and key material never belongs on stderr.
    let bytes = hex::decode(trimmed.strip_prefix("0x").unwrap_or(trimmed))
        .map_err(|_| "EVM key must be an even-length hex string")?;
    let secret: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("EVM key must be 32 bytes, got {}", bytes.len()))?;
    Ok(secret)
}

/// Decode the stdin [`PaymentRequirements`], tagging a parse failure with where
/// it came from so a malformed paid-call payload surfaces a clear error.
fn decode_requirement(input: &str) -> Result<PaymentRequirements, Box<dyn std::error::Error>> {
    let req = serde_json::from_str(input.trim())
        .map_err(|e| format!("decode PaymentRequirements from stdin: {e}"))?;
    Ok(req)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_secret_accepts_prefixed_and_bare_hex() {
        let bare = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
        assert_eq!(decode_secret(bare).unwrap()[0], 1);
        assert_eq!(decode_secret(&format!("0x{bare}")).unwrap()[31], 0x20);
        assert_eq!(decode_secret(&format!("  0x{bare}\n")).unwrap()[0], 1);
    }

    #[test]
    fn decode_secret_rejects_wrong_length_and_non_hex() {
        assert!(decode_secret("00").is_err());
        assert!(decode_secret("zz".repeat(32).as_str()).is_err());
    }

    #[test]
    fn decode_requirement_surfaces_malformed_input_with_marker() {
        let err = decode_requirement("{ not json").unwrap_err().to_string();
        assert!(
            err.contains("decode PaymentRequirements from stdin"),
            "expected context-tagged decode marker, got: {err}"
        );
    }
}
