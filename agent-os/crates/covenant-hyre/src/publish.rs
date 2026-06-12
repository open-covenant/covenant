//! Optional resale publishing through the SAP bridge.
//!
//! If an operator runs the SAP bridge and flags a Hyre tool as
//! published, remote agents can discover that this daemon resells paid
//! Hyre access at a marked-up tier — the markup flowing back through
//! Covenant settlement. This module only derives the publishable
//! descriptors; the bridge owns the actual on-chain registration and is
//! gated by its own capability. With the bridge off, [`publishable`]
//! returns nothing and the rest of the profile is unaffected.

use tracing::warn;

use crate::catalog::HyreCatalog;
use crate::config::HyreConfig;

/// A Hyre tool offered for resale, priced at the operator's markup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishableTool {
    pub tool_name: String,
    pub slug: String,
    pub description: String,
    /// Hyre's published price, atomic USDC.
    pub base_price_micro_usdc: u128,
    /// Resale price after the operator markup, atomic USDC.
    pub resale_price_micro_usdc: u128,
}

/// Derive the descriptors for the tools an operator has flagged for
/// resale. Returns empty when the bridge is off, so callers can wire
/// this unconditionally. `is_published` is the per-tool flag the
/// operator controls (by tool name).
pub fn publishable(
    catalog: &HyreCatalog,
    config: &HyreConfig,
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
        .filter_map(|ep| {
            let Some(resale_price_micro_usdc) = config.marked_up(ep.price_micro_usdc) else {
                warn!(
                    tool = %ep.tool_name(),
                    price = ep.price_micro_usdc,
                    "resale markup overflows u128; tool not published for resale"
                );
                return None;
            };
            Some(PublishableTool {
                tool_name: ep.tool_name(),
                slug: ep.slug(),
                description: if ep.summary.is_empty() {
                    ep.description.clone()
                } else {
                    ep.summary.clone()
                },
                base_price_micro_usdc: ep.price_micro_usdc,
                resale_price_micro_usdc,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> HyreCatalog {
        HyreCatalog::from_vendored(&HyreConfig::default()).unwrap()
    }

    #[test]
    fn bridge_off_publishes_nothing() {
        let out = publishable(&catalog(), &HyreConfig::default(), false, |_| true);
        assert!(out.is_empty());
    }

    #[test]
    fn per_tool_flag_filters_and_markup_applies() {
        let config = HyreConfig {
            markup_bps: 5_000, // +50%
            ..HyreConfig::default()
        };
        let out = publishable(&catalog(), &config, true, |name| name == "hyre.defi.tvl");
        assert_eq!(out.len(), 1);
        let t = &out[0];
        assert_eq!(t.slug, "defi/tvl");
        assert_eq!(t.base_price_micro_usdc, 10_000);
        assert_eq!(t.resale_price_micro_usdc, 15_000);
    }

    #[test]
    fn overflowing_resale_price_drops_the_tool() {
        // A manifest-derived price can be as large as u128::MAX (usd_to_micro
        // only bounds the *scaling*), so applying a nonzero markup can overflow.
        // The tool must drop out of the resale set, not panic publishable().
        let manifest = serde_json::json!({
            "openapi": "3.1.0",
            "paths": {
                "/defi/tvl": {
                    "get": { "x-payment-info": { "price": { "amount": "100000000000000000000000000000000" } } }
                }
            }
        })
        .to_string();
        let catalog = HyreCatalog::from_manifest(&manifest, &HyreConfig::default()).unwrap();

        // At cost the huge-but-valid price still fits, so the tool publishes.
        let at_cost = publishable(&catalog, &HyreConfig::default(), true, |n| {
            n == "hyre.defi.tvl"
        });
        assert_eq!(
            at_cost.len(),
            1,
            "at-cost resale of a huge but valid price still publishes"
        );

        // With a markup the resale price overflows u128 and the tool drops.
        let marked = HyreConfig {
            markup_bps: 5_000,
            ..HyreConfig::default()
        };
        let out = publishable(&catalog, &marked, true, |n| n == "hyre.defi.tvl");
        assert!(
            out.is_empty(),
            "an overflowing resale markup drops the tool instead of panicking",
        );
    }
}
