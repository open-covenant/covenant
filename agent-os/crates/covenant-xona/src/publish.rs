//! Optional resale publishing through the SAP bridge.
//!
//! If an operator runs the SAP bridge and flags a Xona tool as
//! published, remote agents can discover that this daemon resells paid
//! Xona access at a marked-up tier — the markup flowing back through
//! Covenant settlement. This module only derives the publishable
//! descriptors; the bridge owns the actual on-chain registration and is
//! gated by its own capability. With the bridge off, [`publishable`]
//! returns nothing and the rest of the profile is unaffected.

use crate::catalog::XonaCatalog;
use crate::config::XonaConfig;

/// A Xona tool offered for resale, priced at the operator's markup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishableTool {
    pub tool_name: String,
    pub slug: String,
    pub description: String,
    /// Xona's published price, atomic USDC.
    pub base_price_micro_usdc: u128,
    /// Resale price after the operator markup, atomic USDC.
    pub resale_price_micro_usdc: u128,
}

/// Derive the descriptors for the tools an operator has flagged for
/// resale. Returns empty when the bridge is off, so callers can wire
/// this unconditionally. `is_published` is the per-tool flag the
/// operator controls (by tool name).
pub fn publishable(
    catalog: &XonaCatalog,
    config: &XonaConfig,
    bridge_enabled: bool,
    is_published: impl Fn(&str) -> bool,
) -> Vec<PublishableTool> {
    if !bridge_enabled {
        return Vec::new();
    }
    catalog
        .endpoints()
        .iter()
        .filter(|ep| is_published(&ep.tool_name()))
        .map(|ep| PublishableTool {
            tool_name: ep.tool_name(),
            slug: ep.slug.clone(),
            description: if ep.description.is_empty() {
                ep.slug.clone()
            } else {
                ep.description.clone()
            },
            base_price_micro_usdc: ep.price_micro_usdc,
            resale_price_micro_usdc: config.marked_up(ep.price_micro_usdc),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> XonaCatalog {
        XonaCatalog::from_vendored(&XonaConfig::default()).unwrap()
    }

    #[test]
    fn bridge_off_publishes_nothing() {
        let out = publishable(&catalog(), &XonaConfig::default(), false, |_| true);
        assert!(out.is_empty());
    }

    #[test]
    fn per_tool_flag_filters_and_markup_applies() {
        let config = XonaConfig {
            markup_bps: 5_000, // +50%
            ..XonaConfig::default()
        };
        let out = publishable(&catalog(), &config, true, |name| {
            name == "xona.image.creative-director"
        });
        assert_eq!(out.len(), 1);
        let t = &out[0];
        assert_eq!(t.slug, "image/creative-director");
        // image/creative-director is $0.03 on Solana.
        assert_eq!(t.base_price_micro_usdc, 30_000);
        assert_eq!(t.resale_price_micro_usdc, 45_000);
    }
}
