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
//! - `COVENANT_EVM_CHAIN` — target chain. `base-sepolia` (the default) is
//!   the only chain signed for autonomously. The Base mainnet aliases
//!   (`base`, `base-mainnet`, `eip155:8453`) all resolve to one gated
//!   domain and fail closed without the override below.
//! - `COVENANT_EVM_ALLOW_MAINNET` — operator override for the mainnet
//!   gate; must be exactly `1`. Signing here is off-chain and moves no
//!   funds, but a mainnet-domain attestation is a live artifact Base
//!   tooling accepts, so producing one stays an operator decision
//!   (BLOCKERS.md "HELD — multichain-21 attestation").
//! - `COVENANT_EVM_MODE` — what stdin holds. `audit-root` (the default)
//!   reads a dual-signed audit-root VC and must be signed by the key that
//!   signed the VC's EVM proof. `reputation` reads a reputation projection
//!   (`{ score, scoreDecimals, sourceChain, solanaAttestationPda, issuedAt,
//!   expiry }`) and signs it directly.

use std::io::{Read, Write};
use std::path::Path;
use std::process::ExitCode;

use covenant_attestation::VerifiableCredential;
use covenant_evm_signer::{
    parse_reputation_projection, EasAttestationSigner, EasDomain, EvmSignerError,
};
use covenant_identity::Secp256k1IssuerKey;

type BoxError = Box<dyn std::error::Error>;

/// The one chain the sidecar signs for without an operator override.
const DEFAULT_CHAIN: &str = "base-sepolia";
/// Operator override for the Base mainnet gate. Must be exactly `1`.
const ALLOW_MAINNET_ENV: &str = "COVENANT_EVM_ALLOW_MAINNET";

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
    let key_path = std::env::var("COVENANT_EVM_ISSUER_KEY")
        .map_err(|_| "COVENANT_EVM_ISSUER_KEY is not set")?;
    let domain = domain_for(
        std::env::var("COVENANT_EVM_CHAIN")
            .as_deref()
            .unwrap_or(DEFAULT_CHAIN),
        std::env::var(ALLOW_MAINNET_ENV).ok().as_deref(),
    )?;
    let mode = std::env::var("COVENANT_EVM_MODE").unwrap_or_else(|_| "audit-root".into());

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;

    let issuer = Secp256k1IssuerKey::load_or_create(Path::new(&key_path))?;
    let signer = EasAttestationSigner::new(issuer, domain);

    let attestation = match mode.as_str() {
        "audit-root" => signer.attest(&VerifiableCredential::from_json(input.trim())?)?,
        "reputation" => signer.attest_reputation(&parse_reputation_projection(
            &serde_json::from_str(input.trim())?,
        )?)?,
        other => {
            return Err(format!(
                "unsupported COVENANT_EVM_MODE '{other}': 'audit-root' or 'reputation'"
            )
            .into())
        }
    };
    Ok(attestation.to_json_string())
}

/// Resolve the target chain to its EAS domain. Each chain carries its own
/// EAS domain version, so an unknown chain is refused rather than silently
/// retargeted (which would sign under the wrong domain and recover the wrong
/// signer).
///
/// The mainnet gate is keyed on the *resolved* chain id, not the alias
/// spelling, so `base`, `base-mainnet`, and `eip155:8453` all pass through
/// the same fail-closed check: unless `allow_mainnet` is exactly `1`
/// (`COVENANT_EVM_ALLOW_MAINNET=1`), only Base Sepolia's domain resolves.
/// Off-chain signing moves no funds, but a mainnet-domain attestation is a
/// live artifact Base tooling accepts, so it stays an operator decision
/// (BLOCKERS.md "HELD — multichain-21 attestation").
fn domain_for(chain: &str, allow_mainnet: Option<&str>) -> Result<EasDomain, BoxError> {
    let domain = match chain {
        "base-sepolia" => EasDomain::base_sepolia(),
        "base" | "base-mainnet" | "eip155:8453" => EasDomain::base_mainnet(),
        other => {
            return Err(format!(
                "unsupported COVENANT_EVM_CHAIN '{other}': expected 'base-sepolia' or a \
                 gated mainnet alias ('base', 'base-mainnet', 'eip155:8453')"
            )
            .into())
        }
    };
    if domain.chain_id != EasDomain::base_sepolia().chain_id && allow_mainnet != Some("1") {
        return Err(Box::new(EvmSignerError::MainnetGated("Base mainnet")));
    }
    Ok(domain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Every spelling that resolves to the Base mainnet domain. A gate keyed
    /// on one alias but not another would be a bypass, so the tests iterate
    /// all of them; a newly added alias must join the gate to pass.
    const MAINNET_ALIASES: [&str; 3] = ["base", "base-mainnet", "eip155:8453"];

    #[test]
    fn the_default_chain_needs_no_override() {
        // Pins the header doc: `base-sepolia` is the default and the only
        // chain available without COVENANT_EVM_ALLOW_MAINNET.
        assert_eq!(domain_for(DEFAULT_CHAIN, None).unwrap().chain_id, 84_532);
    }

    #[test]
    fn every_mainnet_alias_fails_closed_without_the_override() {
        // The gate BLOCKERS.md's mainnet-attestation prerequisite points at.
        for alias in MAINNET_ALIASES {
            let err = domain_for(alias, None).unwrap_err();
            assert!(
                matches!(
                    err.downcast_ref::<EvmSignerError>(),
                    Some(EvmSignerError::MainnetGated(_))
                ),
                "{alias} must be MainnetGated, got: {err}"
            );
        }
    }

    #[test]
    fn the_override_must_be_exactly_one() {
        // Fail closed on anything but the documented literal `1`: a typo'd
        // or truthy-looking value must not open the gate.
        for weak in ["true", "yes", "0", "", " 1"] {
            for alias in MAINNET_ALIASES {
                assert!(
                    domain_for(alias, Some(weak)).is_err(),
                    "{alias} must stay gated under override '{weak}'"
                );
            }
        }
    }

    #[test]
    fn overridden_mainnet_aliases_share_one_pinned_domain() {
        for alias in MAINNET_ALIASES {
            let d = domain_for(alias, Some("1")).unwrap();
            assert_eq!(d.chain_id, 8_453, "{alias}");
            assert_eq!(d.version, "1.0.1", "{alias}");
        }
    }

    #[test]
    fn unknown_chains_are_refused_even_with_the_override() {
        // The override widens availability to exactly Base mainnet — it is
        // not a bypass of chain resolution itself. Unknown chains are
        // refused, not silently retargeted, so a caller never signs under
        // the wrong EAS domain.
        for chain in ["mainnet", "ethereum", "eip155:1", ""] {
            assert!(domain_for(chain, Some("1")).is_err(), "{chain}");
        }
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
        let projection = parse_reputation_projection(&input).unwrap();
        assert_eq!(projection.score.score, 9_500);
        assert_eq!(projection.score.decimals, 4);
        assert_eq!(projection.solana_attestation_pda, [0xAB; 32]);

        let signer = EasAttestationSigner::base_sepolia(
            Secp256k1IssuerKey::from_secret_bytes(&[9u8; 32]).unwrap(),
        );
        let att = signer.attest_reputation(&projection).unwrap();
        assert_eq!(att.recover_signer().unwrap(), att.signer);
    }
}
