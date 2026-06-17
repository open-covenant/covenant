//! WURK provider configuration. Agent-to-human family only.
//!
//! Constants are pinned from WURK's live 402 challenge and agent-card so
//! a manipulated challenge can't steer the funding key to another payee
//! or sponsor (enforced in [`crate::x402::to_requirements`]).

use serde::{Deserialize, Serialize};

/// CAIP-2 id for WURK's Solana settlement (mainnet), from the live 402.
pub const SOLANA_NETWORK: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";
/// USDC mint payments are denominated in.
pub const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
/// WURK's Solana payee (holds job funds until payout), from the live 402.
pub const PAY_TO: &str = "SAT8g2xU7AFy7eUmNJ9SNrM6yYo7LDCi13GXJ8Ez9kC";
/// PayAI sponsor pubkey co-signing as `feePayer` (same facilitator Hyre uses).
pub const FEE_PAYER: &str = "2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4";
/// API host.
pub const BASE_URL: &str = "https://wurkapi.fun";

/// The ONLY endpoint families this crate may touch. The engagement,
/// raid, and vote endpoints are deliberately excluded — covenant-wurk
/// cannot construct a request against them.
pub const ALLOWED_PATHS: &[&str] = &["agenttohuman", "agenttohumanadvanced"];

/// Whether `path_segment` is in the agent-to-human allowlist.
pub fn is_allowed(path_segment: &str) -> bool {
    ALLOWED_PATHS.contains(&path_segment)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WurkConfig {
    /// Master switch. `false` means no WURK tools are registered.
    #[serde(default)]
    pub enabled: bool,
    /// API host. Endpoint URLs are built against this.
    #[serde(default = "default_base_url")]
    pub base_url: String,
    /// CAIP-2 settlement network.
    #[serde(default = "default_network")]
    pub network: String,
    /// Payment asset (USDC mint).
    #[serde(default = "default_asset")]
    pub asset: String,
    /// Per-call ceiling in atomic USDC. A 402 amount above this is
    /// rejected before the signer runs. `0` defers to the caller-supplied
    /// cap (winners * perUser).
    #[serde(default)]
    pub per_call_cap: u128,
}

fn default_base_url() -> String {
    BASE_URL.to_string()
}
fn default_network() -> String {
    SOLANA_NETWORK.to_string()
}
fn default_asset() -> String {
    USDC_MINT.to_string()
}

impl Default for WurkConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_base_url(),
            network: default_network(),
            asset: default_asset(),
            per_call_cap: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled_with_solana_rail() {
        let c = WurkConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.network, SOLANA_NETWORK);
        assert_eq!(c.asset, USDC_MINT);
    }

    #[test]
    fn only_agent_to_human_paths_are_allowed() {
        assert!(is_allowed("agenttohuman"));
        assert!(is_allowed("agenttohumanadvanced"));
        for blocked in [
            "xraid",
            "xlikes",
            "xfollowers",
            "dex",
            "cmcvote",
            "cgvote",
            "tgmembers",
        ] {
            assert!(!is_allowed(blocked), "{blocked} must be blocked");
        }
    }
}
