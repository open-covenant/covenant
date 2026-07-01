//! `covenant-evm-signer` sidecar — one-shot, stdin → stdout.
//!
//! Reads a Covenant dual-signed audit-root VC as JSON on stdin, signs the
//! equivalent EAS off-chain attestation with the secp256k1 issuer key, and
//! writes the attestation JSON to stdout. Like the x402 signer, the key
//! lives in this process, not the daemon's address space, so its blast
//! radius is bounded.
//!
//! Configuration (env):
//! - `COVENANT_EVM_ISSUER_KEY` — path to the 32-byte secp256k1 issuer
//!   secret. Loaded through the hardened identity store (`0600`/`0700`),
//!   created on first use. Required. Must be the key that signed the VC's
//!   EVM proof, or the attestation is refused.
//! - `COVENANT_EVM_CHAIN` — target chain. Only `base-sepolia` (the
//!   default) is available; mainnet broadcast is gated separately.

use std::io::{Read, Write};
use std::path::Path;
use std::process::ExitCode;

use covenant_attestation::VerifiableCredential;
use covenant_evm_signer::{EasAttestationSigner, EasDomain};
use covenant_identity::Secp256k1IssuerKey;

fn main() -> ExitCode {
    match run() {
        Ok(json) => {
            if writeln!(std::io::stdout(), "{json}").is_err() {
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("covenant-evm-signer: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<String, Box<dyn std::error::Error>> {
    let key_path =
        std::env::var("COVENANT_EVM_ISSUER_KEY").map_err(|_| "COVENANT_EVM_ISSUER_KEY is not set")?;
    let domain = domain_for(
        std::env::var("COVENANT_EVM_CHAIN")
            .as_deref()
            .unwrap_or("base-sepolia"),
    )?;

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let vc = VerifiableCredential::from_json(input.trim())?;

    let issuer = Secp256k1IssuerKey::load_or_create(Path::new(&key_path))?;
    let attestation = EasAttestationSigner::new(issuer, domain).attest(&vc)?;
    Ok(attestation.to_json_string())
}

/// Resolve the target chain to its EAS domain. Base Sepolia only —
/// anything else is refused rather than silently retargeted, so a caller
/// that expects mainnet gets an error instead of a Sepolia attestation.
fn domain_for(chain: &str) -> Result<EasDomain, Box<dyn std::error::Error>> {
    match chain {
        "base-sepolia" => Ok(EasDomain::base_sepolia()),
        other => Err(format!(
            "unsupported COVENANT_EVM_CHAIN '{other}': only 'base-sepolia' is available (mainnet is gated)"
        )
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_sepolia_is_the_default_and_only_chain() {
        assert_eq!(domain_for("base-sepolia").unwrap().chain_id, 84_532);
        assert!(domain_for("base").is_err());
        assert!(domain_for("base-mainnet").is_err());
        assert!(domain_for("mainnet").is_err());
    }
}
