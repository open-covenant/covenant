//! Hyre provider configuration.
//!
//! Everything operator-tunable about the Hyre profile lives here: the
//! settlement rail, the spend allowlist, and the resale markup. The
//! defaults encode Hyre's published Solana/PayAI rail so a daemon that
//! only flips `enabled = true` gets a working profile.

use serde::{Deserialize, Serialize};

/// CAIP-2 id for Hyre's Solana settlement, from the manifest's
/// `x-discovery`.
pub const SOLANA_NETWORK: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";
/// USDC mint payments are denominated in.
pub const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
/// Hyre's Solana payee.
pub const PAY_TO: &str = "7G73PLhKvAPBGTzG5ESAE4coE7QrVeTTKfhTxQZbyGgC";
/// PayAI facilitator — used as a diagnostic endpoint (see
/// [`crate::facilitator`]). On the production path Hyre's own
/// middleware is what calls `/verify` + `/settle`.
pub const FACILITATOR: &str = "https://facilitator.payai.network";
/// PayAI's sponsor pubkey — the wallet that co-signs as `feePayer` for
/// every Solana x402 settlement. Pinned in [`crate::x402::to_requirements`]
/// so a manipulated 402 challenge can't substitute an attacker address
/// (a substituted pubkey would just fail at PayAI's `/verify`, but the
/// hard equality check makes that intent explicit and refuses to sign).
pub const PAYAI_FEE_PAYER: &str = "2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4";
/// Production host serving both the API and the OpenAPI manifest.
pub const BASE_URL: &str = "https://mpp.hyreagent.fun";
/// Live manifest path, polled to refresh the catalog without a rebuild.
pub const MANIFEST_PATH: &str = "/openapi.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HyreConfig {
    /// Master switch. `false` means no Hyre tools are registered and
    /// no Hyre call ever leaves the host.
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
    /// defers entirely to the daemon capability's per-call cap.
    #[serde(default)]
    pub per_call_cap: u128,
    /// Endpoint allowlist by slug (`trenches/new-tokens`). `None`
    /// allows every catalog endpoint; `Some([])` allows none.
    #[serde(default)]
    pub allow: Option<Vec<String>>,
    /// Resale markup in basis points applied to the published price
    /// when republishing tools through the SAP bridge. `0` = at cost.
    #[serde(default)]
    pub markup_bps: u16,
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

impl Default for HyreConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_base_url(),
            network: default_network(),
            asset: default_asset(),
            per_call_cap: 0,
            allow: None,
            markup_bps: 0,
        }
    }
}

impl HyreConfig {
    /// URL the catalog refresh fetches the manifest from.
    pub fn manifest_url(&self) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), MANIFEST_PATH)
    }

    /// Whether `slug` is permitted by the allowlist.
    pub fn allows(&self, slug: &str) -> bool {
        match &self.allow {
            None => true,
            Some(list) => list.iter().any(|s| s == slug),
        }
    }

    /// Apply the resale markup to an atomic-USDC price.
    pub fn marked_up(&self, price_micro_usdc: u128) -> u128 {
        price_micro_usdc + price_micro_usdc * self.markup_bps as u128 / 10_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled_with_solana_rail() {
        let c = HyreConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.network, SOLANA_NETWORK);
        assert_eq!(c.asset, USDC_MINT);
        assert_eq!(c.manifest_url(), "https://mpp.hyreagent.fun/openapi.json");
    }

    #[test]
    fn allowlist_none_allows_all_some_is_exact() {
        let mut c = HyreConfig::default();
        assert!(c.allows("trenches/new-tokens"));
        c.allow = Some(vec!["defi/tvl".into()]);
        assert!(c.allows("defi/tvl"));
        assert!(!c.allows("trenches/new-tokens"));
        c.allow = Some(vec![]);
        assert!(!c.allows("defi/tvl"));
    }

    #[test]
    fn markup_applies_basis_points() {
        let mut c = HyreConfig::default();
        assert_eq!(c.marked_up(80_000), 80_000);
        c.markup_bps = 2_000; // +20%
        assert_eq!(c.marked_up(80_000), 96_000);
    }

    #[test]
    fn config_round_trips_through_serde_with_defaults() {
        let json = serde_json::json!({ "enabled": true });
        let c: HyreConfig = serde_json::from_value(json).unwrap();
        assert!(c.enabled);
        assert_eq!(c.base_url, BASE_URL);
        assert_eq!(c.markup_bps, 0);
    }
}
