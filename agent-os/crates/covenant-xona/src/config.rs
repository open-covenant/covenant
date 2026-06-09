//! Xona provider configuration.
//!
//! Everything operator-tunable about the Xona profile lives here: the
//! settlement rail, the spend allowlist, and the registry source. The
//! defaults encode Xona's published Solana/USDC rail so a daemon that
//! only flips `enabled = true` gets a working profile.

use serde::{Deserialize, Serialize};

/// CAIP-2 id for Xona's Solana settlement (mainnet-beta genesis hash).
pub const SOLANA_NETWORK: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";
/// USDC mint payments are denominated in (Solana mainnet).
pub const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
/// Xona's Solana payee, pinned against the live 402 challenge so a
/// manipulated challenge can't steer the funding key to another address.
pub const PAY_TO: &str = "9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRSTEA";
/// Stable prefix of Xona's `serverTitle` in the orbit registry
/// (`"Xona Agent | Infrastructure for Agentic Commerce"`). Matched by
/// prefix so a tagline change doesn't drop the catalog.
pub const SERVER_TITLE_PREFIX: &str = "Xona Agent";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct XonaConfig {
    /// Master switch. `false` means no Xona tools are registered and
    /// no Xona call ever leaves the host.
    #[serde(default)]
    pub enabled: bool,
    /// Registry `serverTitle` prefix that identifies Xona's entries.
    #[serde(default = "default_server_title_prefix")]
    pub server_title_prefix: String,
    /// CAIP-2 settlement network. Only endpoints priced on this network
    /// (and `asset`) enter the catalog — Xona also lists Base endpoints
    /// the Solana funding key cannot pay.
    #[serde(default = "default_network")]
    pub network: String,
    /// Payment asset (USDC mint).
    #[serde(default = "default_asset")]
    pub asset: String,
    /// Per-call ceiling in atomic USDC. A 402 amount above this is
    /// rejected before the signer runs. `0` disables the local cap and
    /// defers entirely to the daemon capability's per-call cap.
    #[serde(default)]
    pub per_call_cap: u128,
    /// Endpoint allowlist by slug (`image/creative-director`). `None`
    /// allows every catalog endpoint; `Some([])` allows none.
    #[serde(default)]
    pub allow: Option<Vec<String>>,
}

fn default_server_title_prefix() -> String {
    SERVER_TITLE_PREFIX.to_string()
}
fn default_network() -> String {
    SOLANA_NETWORK.to_string()
}
fn default_asset() -> String {
    USDC_MINT.to_string()
}

impl Default for XonaConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            server_title_prefix: default_server_title_prefix(),
            network: default_network(),
            asset: default_asset(),
            per_call_cap: 0,
            allow: None,
        }
    }
}

impl XonaConfig {
    /// Whether a registry `serverTitle` belongs to Xona.
    pub fn matches_server(&self, server_title: &str) -> bool {
        server_title.starts_with(&self.server_title_prefix)
    }

    /// Whether `slug` is permitted by the allowlist.
    pub fn allows(&self, slug: &str) -> bool {
        match &self.allow {
            None => true,
            Some(list) => list.iter().any(|s| s == slug),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled_with_solana_rail() {
        let c = XonaConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.network, SOLANA_NETWORK);
        assert_eq!(c.asset, USDC_MINT);
        assert_eq!(c.server_title_prefix, SERVER_TITLE_PREFIX);
    }

    #[test]
    fn matches_server_is_prefix_not_exact() {
        let c = XonaConfig::default();
        assert!(c.matches_server("Xona Agent | Infrastructure for Agentic Commerce"));
        assert!(c.matches_server("Xona Agent"));
        assert!(!c.matches_server("Orbis — API Marketplace"));
        assert!(!c.matches_server("Hyre"));
    }

    #[test]
    fn allowlist_none_allows_all_some_is_exact() {
        let mut c = XonaConfig::default();
        assert!(c.allows("image/creative-director"));
        c.allow = Some(vec!["audio/speech-to-text".into()]);
        assert!(c.allows("audio/speech-to-text"));
        assert!(!c.allows("image/creative-director"));
        c.allow = Some(vec![]);
        assert!(!c.allows("audio/speech-to-text"));
    }

    #[test]
    fn config_round_trips_through_serde_with_defaults() {
        let json = serde_json::json!({ "enabled": true });
        let c: XonaConfig = serde_json::from_value(json).unwrap();
        assert!(c.enabled);
        assert_eq!(c.network, SOLANA_NETWORK);
        assert_eq!(c.server_title_prefix, SERVER_TITLE_PREFIX);
    }
}
