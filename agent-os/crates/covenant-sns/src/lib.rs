//! SNS (Solana Name Service) profile for Covenant.
//!
//! Read-only `.sol` name resolution: the human-readable front door to an
//! agent's on-chain accountability record. Phase 1 is keyless resolution over
//! the hosted SNS REST resolver (forward name -> owner, reverse owner ->
//! names, typed record reads). No keys, no spend, no `solana-sdk` in the
//! daemon build. Subdomain issuance and record binding are a later phase
//! behind a signer sidecar.

#![deny(unsafe_code)]

mod config;
mod resolve;
mod tools;

pub use config::SnsConfig;
pub use resolve::{HttpSnsResolver, ResolvedDomain, SnsError, SnsResolver};
pub use tools::{sns_specs, sns_tool, TOOL_PREFIX};

/// Default hosted SNS resolver (the public Bonfida sns-sdk proxy). Keyless
/// HTTP; override with `COVENANT_SNS_RESOLVER_URL`.
pub const DEFAULT_RESOLVER_URL: &str = "https://sns-sdk-proxy.bonfida.workers.dev";

/// The `.sol` top-level suffix.
pub const SOL_TLD: &str = ".sol";

/// Bare resolver name for a `.sol` input: trim whitespace and drop a single
/// trailing `.sol`. The resolver expects `bonfida`, not `bonfida.sol`;
/// subdomains keep their parent (`agent.covenant`).
pub fn normalize_domain(input: &str) -> &str {
    let t = input.trim();
    t.strip_suffix(SOL_TLD).unwrap_or(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_one_sol_suffix_and_keeps_subdomains() {
        assert_eq!(normalize_domain("bonfida.sol"), "bonfida");
        assert_eq!(normalize_domain("  bonfida.sol  "), "bonfida");
        assert_eq!(normalize_domain("bonfida"), "bonfida");
        assert_eq!(normalize_domain("agent.covenant.sol"), "agent.covenant");
    }
}
