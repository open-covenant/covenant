//! Build the endpoint index from the orbit-x402 registry.
//!
//! [`XonaCatalog`] is the bridge between the registry and the tool
//! layer. It filters the registry down to Xona's entries that settle on
//! the configured `(network, asset)` rail — Xona also lists Base
//! endpoints the Solana funding key cannot pay — and keeps a flat
//! [`XonaEndpoint`] list the tool layer reads. One catalog is cheap to
//! rebuild on each refresh cycle.

use covenant_x402::{OrbitClient, RegistryEntry};

use crate::config::XonaConfig;
use crate::{Result, XonaError};

/// One resolved Xona endpoint on the configured settlement rail.
#[derive(Debug, Clone, PartialEq)]
pub struct XonaEndpoint {
    /// Registry slug, e.g. `image/creative-director`.
    pub slug: String,
    /// Fully resolved endpoint URL.
    pub url: String,
    /// HTTP verb the endpoint expects (Xona's are POST).
    pub method: String,
    /// Capability summary from the registry.
    pub description: String,
    /// Published price in atomic USDC for the configured rail — the
    /// discovery-time figure; the live 402 is authoritative.
    pub price_micro_usdc: u128,
    /// Registry-advertised payee on the configured rail. Pinned against
    /// the live 402 challenge before any signature is produced.
    pub pay_to: String,
}

impl XonaEndpoint {
    /// MCP tool name: `xona.<slug>` with path separators flattened to
    /// dots and any braces stripped.
    pub fn tool_name(&self) -> String {
        let body = self
            .slug
            .trim_start_matches('/')
            .replace(['{', '}'], "")
            .replace('/', ".");
        format!("xona.{body}")
    }

    /// USD-pegged budget credits to debit — one credit per atomic cent
    /// of published price, matching the daemon's other paid-call rails.
    pub fn credits(&self) -> u64 {
        (self.price_micro_usdc / 10_000) as u64
    }
}

pub struct XonaCatalog {
    endpoints: Vec<XonaEndpoint>,
}

impl XonaCatalog {
    /// Filter raw registry entries down to Xona's endpoints on the
    /// configured rail. An entry is kept when its `serverTitle` matches
    /// the Xona prefix, its slug is allowed, and it carries a pricing
    /// option on the configured `(network, asset)`.
    pub fn from_entries(entries: Vec<RegistryEntry>, config: &XonaConfig) -> Self {
        let endpoints = entries
            .into_iter()
            .filter(|e| config.matches_server(&e.server_title) && config.allows(&e.slug))
            .filter_map(|e| {
                let pricing = e
                    .pricing
                    .iter()
                    .find(|p| p.network == config.network && p.asset == config.asset)?;
                let price_micro_usdc = pricing.amount.parse::<u128>().ok()?;
                Some(XonaEndpoint {
                    slug: e.slug,
                    url: e.endpoint,
                    method: e.method,
                    description: e.description,
                    price_micro_usdc,
                    pay_to: pricing.pay_to.clone(),
                })
            })
            .collect();
        Self { endpoints }
    }

    /// Parse a vendored or fetched registry snapshot (a JSON array of
    /// [`RegistryEntry`]) and filter it under the config.
    pub fn from_snapshot(snapshot_json: &str, config: &XonaConfig) -> Result<Self> {
        let entries: Vec<RegistryEntry> = serde_json::from_str(snapshot_json)
            .map_err(|e| XonaError::Snapshot(format!("decode registry snapshot: {e}")))?;
        Ok(Self::from_entries(entries, config))
    }

    /// Build from the crate's vendored snapshot — the offline default.
    pub fn from_vendored(config: &XonaConfig) -> Result<Self> {
        Self::from_snapshot(crate::VENDORED_SNAPSHOT, config)
    }

    /// Fetch the live orbit-x402 registry and rebuild. The daemon polls
    /// this on a refresh interval so a Xona catalog change needs no
    /// rebuild.
    pub async fn refresh(orbit: &OrbitClient, config: &XonaConfig) -> Result<Self> {
        let entries = orbit
            .fetch_all()
            .await
            .map_err(|e| XonaError::Registry(e.to_string()))?;
        Ok(Self::from_entries(entries, config))
    }

    pub fn endpoints(&self) -> &[XonaEndpoint] {
        &self.endpoints
    }

    pub fn is_empty(&self) -> bool {
        self.endpoints.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendored_catalog_keeps_only_solana_endpoints() {
        let cat = XonaCatalog::from_vendored(&XonaConfig::default()).unwrap();
        // The vendored snapshot carries Solana + Base entries; only the
        // Solana-settled ones survive the default (network, asset) filter.
        assert!(!cat.is_empty());
        assert!(cat
            .endpoints()
            .iter()
            .all(|e| !e.slug.starts_with("base/") && !e.slug.starts_with("base-main/")));
        let cd = cat
            .endpoints()
            .iter()
            .find(|e| e.slug == "image/creative-director")
            .expect("creative-director on solana");
        assert_eq!(cd.pay_to, crate::config::PAY_TO);
        assert_eq!(cd.url, "https://api.xona-agent.com/image/creative-director");
        assert_eq!(cd.tool_name(), "xona.image.creative-director");
        assert!(cd.price_micro_usdc > 0);
    }

    #[test]
    fn allowlist_filters_catalog() {
        let config = XonaConfig {
            allow: Some(vec![
                "image/creative-director".into(),
                "audio/speech-to-text".into(),
            ]),
            ..XonaConfig::default()
        };
        let cat = XonaCatalog::from_vendored(&config).unwrap();
        assert_eq!(cat.endpoints().len(), 2);
        assert!(cat
            .endpoints()
            .iter()
            .all(|e| e.slug == "image/creative-director" || e.slug == "audio/speech-to-text"));
    }

    #[test]
    fn from_entries_drops_off_rail_and_unpriced() {
        let entries: Vec<RegistryEntry> = serde_json::from_value(serde_json::json!([
            // Xona, Solana-priced → kept.
            {
                "serverUrl": "https://api.xona-agent.com",
                "serverTitle": "Xona Agent | Infrastructure for Agentic Commerce",
                "endpoint": "https://api.xona-agent.com/image/designer",
                "slug": "image/designer", "method": "POST", "description": "d",
                "pricing": [{ "network": crate::config::SOLANA_NETWORK, "asset": crate::config::USDC_MINT, "amount": "60000", "amountUsdc": 0.06, "payTo": crate::config::PAY_TO, "scheme": "exact" }]
            },
            // Xona, but only Base-priced → dropped (off-rail).
            {
                "serverUrl": "https://api.xona-agent.com",
                "serverTitle": "Xona Agent | Infrastructure for Agentic Commerce",
                "endpoint": "https://api.xona-agent.com/base/image/designer",
                "slug": "base/image/designer", "method": "POST", "description": "d",
                "pricing": [{ "network": "eip155:8453", "asset": "0xUSDCbase", "amount": "60000", "amountUsdc": 0.06, "payTo": "0xpayee", "scheme": "exact" }]
            },
            // Xona, no pricing → dropped.
            {
                "serverUrl": "https://api.xona-agent.com",
                "serverTitle": "Xona Agent | Infrastructure for Agentic Commerce",
                "endpoint": "https://api.xona-agent.com/token/pumpfun-movers",
                "slug": "token/pumpfun-movers", "method": "POST", "description": "d",
                "pricing": []
            },
            // A different provider, Solana-priced → dropped (wrong server).
            {
                "serverUrl": "https://other.example",
                "serverTitle": "Orbis — API Marketplace",
                "endpoint": "https://other.example/x", "slug": "x", "method": "POST", "description": "d",
                "pricing": [{ "network": crate::config::SOLANA_NETWORK, "asset": crate::config::USDC_MINT, "amount": "10000", "amountUsdc": 0.01, "payTo": "Pother", "scheme": "exact" }]
            }
        ]))
        .unwrap();
        let cat = XonaCatalog::from_entries(entries, &XonaConfig::default());
        assert_eq!(cat.endpoints().len(), 1);
        assert_eq!(cat.endpoints()[0].slug, "image/designer");
        assert_eq!(cat.endpoints()[0].credits(), 6);
    }
}
