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
//! Dispatch is by inspecting `requirements.extra.feePayer`:
//! - present → sponsored flow (`PayaiSolanaSigner`): builds a v0
//!   `VersionedTransaction` whose payer slot is the sponsor's pubkey
//!   and partial-signs as funder; the facilitator co-signs at settle.
//! - absent → self-paid flow (`SolanaSigner`): builds a legacy
//!   `Transaction` and full-signs, with the funder paying SOL gas.
//!
//! Configuration (env):
//! - `COVENANT_X402_FUNDING_KEYPAIR` — path to the Solana keypair JSON
//!   that funds payments. Required.
//! - `COVENANT_X402_RPC_URL` — Solana RPC for the blockhash + mint
//!   decimals lookup. Defaults to mainnet-beta.
//! - `COVENANT_X402_VERSION`: envelope `x402Version` to emit, 1 (default,
//!   PayAI v1) or 2 (Dexter-style facilitators).
//! - `COVENANT_X402_NETWORK_VERBATIM`: truthy echoes the challenge's CAIP-2
//!   `network` verbatim instead of PayAI's short `solana`. Both knobs apply
//!   to the sponsored flow; the self-paid flow ignores them.

use std::process::ExitCode;

use covenant_x402::{PayaiSolanaSigner, PaymentRequirements, Signer, SolanaSigner};
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
    let requirement = decode_requirement(&input)?;

    let sponsored = requirement
        .extra
        .as_ref()
        .and_then(|e| e.fee_payer.as_ref())
        .is_some();

    if sponsored {
        let (version, verbatim) = envelope_overrides(
            std::env::var("COVENANT_X402_VERSION").ok().as_deref(),
            std::env::var("COVENANT_X402_NETWORK_VERBATIM")
                .ok()
                .as_deref(),
        )?;
        let signer = PayaiSolanaSigner::from_keypair_file(&keypair_path, rpc_url)?
            .x402_version(version)
            .network_verbatim(verbatim);
        Ok(signer.build_payment(&requirement).await?)
    } else {
        let signer = SolanaSigner::from_keypair_file(&keypair_path, rpc_url)?;
        Ok(signer.build_payment(&requirement).await?)
    }
}

/// Envelope shape overrides for the sponsored flow. Facilitators disagree:
/// PayAI v1 wants `x402Version: 1` with the short `solana` network, Dexter
/// validates version 2 with the CAIP-2 network verbatim. The daemon pins these
/// per provider instance; junk fails the dispatch instead of silently signing
/// an envelope the facilitator will reject.
fn envelope_overrides(version: Option<&str>, verbatim: Option<&str>) -> Result<(u8, bool), String> {
    let version = match version.map(str::trim) {
        None | Some("") | Some("1") => 1,
        Some("2") => 2,
        Some(other) => {
            return Err(format!(
                "COVENANT_X402_VERSION must be 1 or 2, got {other:?}"
            ))
        }
    };
    let verbatim = matches!(
        verbatim.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("1") | Some("true") | Some("yes")
    );
    Ok((version, verbatim))
}

/// Decode the stdin [`PaymentRequirements`], trimming surrounding whitespace
/// and tagging a parse failure with where it came from so a malformed paid-
/// call payload surfaces a clear error instead of a raw serde message or a
/// panic. Extracted from `run` so the sidecar's only input-validation
/// surface is reachable from a unit test (run owns the stdin + env reads).
fn decode_requirement(input: &str) -> Result<PaymentRequirements, Box<dyn std::error::Error>> {
    let req = serde_json::from_str(input.trim())
        .map_err(|e| format!("decode PaymentRequirements from stdin: {e}"))?;
    Ok(req)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_requirement_surfaces_malformed_input_with_marker() {
        // The sidecar's only input-validation surface: malformed stdin JSON
        // must surface a context-tagged error (not a panic or a raw serde
        // error) so a bad paid-call payload fails the sidecar cleanly.
        let err = decode_requirement("{ not json").unwrap_err().to_string();
        assert!(
            err.contains("decode PaymentRequirements from stdin"),
            "expected context-tagged decode marker, got: {err}"
        );
    }

    #[test]
    fn envelope_overrides_default_to_payai_v1() {
        assert_eq!(envelope_overrides(None, None).unwrap(), (1, false));
        assert_eq!(envelope_overrides(Some(""), Some("0")).unwrap(), (1, false));
    }

    #[test]
    fn envelope_overrides_accept_dexter_v2_verbatim() {
        assert_eq!(
            envelope_overrides(Some("2"), Some("true")).unwrap(),
            (2, true)
        );
        assert_eq!(
            envelope_overrides(Some(" 2 "), Some("YES")).unwrap(),
            (2, true)
        );
    }

    #[test]
    fn envelope_overrides_reject_junk_version() {
        // Junk must fail the dispatch, not silently sign a v1 envelope a
        // v2-validating facilitator will bounce after the quote is consumed.
        assert!(envelope_overrides(Some("3"), None).is_err());
        assert!(envelope_overrides(Some("two"), None).is_err());
    }
}
