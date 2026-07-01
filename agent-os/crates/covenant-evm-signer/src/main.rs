//! `covenant-evm-signer` sidecar — one-shot, stdin → stdout.
//!
//! Reads a Covenant statement as JSON on stdin, signs the equivalent EAS
//! off-chain attestation with the secp256k1 issuer key, and writes the
//! attestation JSON to stdout. Like the x402 signer, the key lives in this
//! process, not the daemon's address space, so its blast radius is bounded.
//!
//! Configuration (env):
//! - `COVENANT_EVM_ISSUER_KEY` — path to the 32-byte secp256k1 issuer
//!   secret. Loaded through the hardened identity store (`0600`/`0700`),
//!   created on first use. Required.
//! - `COVENANT_EVM_CHAIN` — target chain. Only `base-sepolia` (the
//!   default) is available; mainnet broadcast is gated separately.
//! - `COVENANT_EVM_MODE` — what stdin holds. `audit-root` (the default)
//!   reads a dual-signed audit-root VC and must be signed by the key that
//!   signed the VC's EVM proof. `reputation` reads a reputation projection
//!   (`{ score, scoreDecimals, sourceChain, solanaAttestationPda, issuedAt,
//!   expiry }`) and signs it directly.

use std::io::{Read, Write};
use std::path::Path;
use std::process::ExitCode;

use covenant_attestation::VerifiableCredential;
use covenant_evm_signer::{EasAttestationSigner, EasDomain, ReputationProjection, ReputationScore};
use covenant_identity::Secp256k1IssuerKey;
use serde_json::Value;

type BoxError = Box<dyn std::error::Error>;

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

fn run() -> Result<String, BoxError> {
    let key_path =
        std::env::var("COVENANT_EVM_ISSUER_KEY").map_err(|_| "COVENANT_EVM_ISSUER_KEY is not set")?;
    let domain = domain_for(
        std::env::var("COVENANT_EVM_CHAIN")
            .as_deref()
            .unwrap_or("base-sepolia"),
    )?;
    let mode = std::env::var("COVENANT_EVM_MODE").unwrap_or_else(|_| "audit-root".into());

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;

    let issuer = Secp256k1IssuerKey::load_or_create(Path::new(&key_path))?;
    let signer = EasAttestationSigner::new(issuer, domain);

    let attestation = match mode.as_str() {
        "audit-root" => signer.attest(&VerifiableCredential::from_json(input.trim())?)?,
        "reputation" => signer.attest_reputation(&reputation_from_json(&serde_json::from_str(input.trim())?)?)?,
        other => {
            return Err(format!(
                "unsupported COVENANT_EVM_MODE '{other}': 'audit-root' or 'reputation'"
            )
            .into())
        }
    };
    Ok(attestation.to_json_string())
}

/// Resolve the target chain to its EAS domain. Base Sepolia only —
/// anything else is refused rather than silently retargeted, so a caller
/// that expects mainnet gets an error instead of a Sepolia attestation.
fn domain_for(chain: &str) -> Result<EasDomain, BoxError> {
    match chain {
        "base-sepolia" => Ok(EasDomain::base_sepolia()),
        other => Err(format!(
            "unsupported COVENANT_EVM_CHAIN '{other}': only 'base-sepolia' is available (mainnet is gated)"
        )
        .into()),
    }
}

/// Parse a reputation projection from the sidecar's stdin JSON. Accepts
/// camelCase (the wire default) with snake_case fallbacks. The score and its
/// decimal scale are read together so the two can never drift apart.
fn reputation_from_json(v: &Value) -> Result<ReputationProjection, BoxError> {
    let u64_field = |names: &[&str]| -> Result<u64, BoxError> {
        names
            .iter()
            .find_map(|n| v.get(*n).and_then(Value::as_u64))
            .ok_or_else(|| format!("missing unsigned integer field '{}'", names[0]).into())
    };
    let str_field = |names: &[&str]| -> Result<&str, BoxError> {
        names
            .iter()
            .find_map(|n| v.get(*n).and_then(Value::as_str))
            .ok_or_else(|| format!("missing string field '{}'", names[0]).into())
    };

    let score = u32::try_from(u64_field(&["score"])?).map_err(|_| "'score' exceeds uint32")?;
    let decimals =
        u8::try_from(u64_field(&["scoreDecimals", "score_decimals"])?).map_err(|_| "'scoreDecimals' exceeds uint8")?;

    Ok(ReputationProjection::from_pda_hex(
        ReputationScore::new(score, decimals),
        str_field(&["sourceChain", "source_chain"])?.to_string(),
        str_field(&["solanaAttestationPda", "solana_attestation_pda"])?,
        u64_field(&["issuedAt", "issued_at"])?,
        u64_field(&["expiry"])?,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn base_sepolia_is_the_default_and_only_chain() {
        assert_eq!(domain_for("base-sepolia").unwrap().chain_id, 84_532);
        assert!(domain_for("base").is_err());
        assert!(domain_for("base-mainnet").is_err());
        assert!(domain_for("mainnet").is_err());
    }

    #[test]
    fn reputation_json_parses_and_signs() {
        let input = json!({
            "score": 9_500,
            "scoreDecimals": 4,
            "sourceChain": "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
            "solanaAttestationPda": "0xabababababababababababababababababababababababababababababababab",
            "issuedAt": 1_700_000_000,
            "expiry": 1_800_000_000
        });
        let projection = reputation_from_json(&input).unwrap();
        assert_eq!(projection.score.score, 9_500);
        assert_eq!(projection.score.decimals, 4);
        assert_eq!(projection.solana_attestation_pda, [0xAB; 32]);

        let signer = EasAttestationSigner::base_sepolia(Secp256k1IssuerKey::from_secret_bytes(&[9u8; 32]).unwrap());
        let att = signer.attest_reputation(&projection).unwrap();
        assert_eq!(att.recover_signer().unwrap(), att.signer);
    }

    #[test]
    fn reputation_json_reports_missing_fields() {
        let missing_pda = json!({
            "score": 1, "scoreDecimals": 0,
            "sourceChain": "solana:x", "issuedAt": 1, "expiry": 2
        });
        assert!(reputation_from_json(&missing_pda).is_err());
    }
}
