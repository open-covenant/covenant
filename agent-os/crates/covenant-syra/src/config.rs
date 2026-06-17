//! Syra provider configuration.
//!
//! Constants pinned from Syra's live 402 challenge so a manipulated
//! challenge can't steer the funding key to another payee or sponsor
//! (enforced in [`crate::x402::to_requirements`]).

use serde::{Deserialize, Serialize};

/// CAIP-2 id for Syra's Solana settlement (mainnet), from the live 402.
pub const SOLANA_NETWORK: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";
/// USDC mint payments are denominated in.
pub const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
/// Syra's Solana payee, from the live 402.
pub const PAY_TO: &str = "53JhuF8bgxvUQ59nDG6kWs4awUQYCS3wswQmUsV5uC7t";
/// Syra's sponsor pubkey, co-signs as `feePayer` so the agent needs only
/// USDC (no SOL for gas).
pub const FEE_PAYER: &str = "AepWpq3GQwL8CeKMtZyKtKPa7W91Coygh3ropAJapVdU";
/// API host.
pub const BASE_URL: &str = "https://api.syraa.fun";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyraConfig {
    /// Master switch. `false` means no Syra tools are registered.
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
    /// rejected before the signer runs. `0` disables the local cap and
    /// defers to the daemon capability's per-call cap.
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

impl Default for SyraConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_base_url(),
            network: default_network(),
            asset: default_asset(),
            // Default ceiling: $0.50 atomic. The priciest curated
            // endpoint is $0.10, so this admits all of them while
            // refusing a 402 that asks for far more.
            per_call_cap: 500_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled_with_solana_rail() {
        let c = SyraConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.network, SOLANA_NETWORK);
        assert_eq!(c.asset, USDC_MINT);
        assert_eq!(c.per_call_cap, 500_000);
    }
}
