//! Build the gateway catalog and tool index from a parsed manifest.
//!
//! [`HyreCatalog`] is the bridge between the manifest and the rest of
//! the system. It produces a [`covenant_x402::Catalog`] — the exact
//! registry shape the gateway's discover-and-pay flow consumes — and
//! keeps the richer [`Endpoint`] list the tool layer reads for argument
//! schemas. One catalog is cheap to rebuild on each refresh cycle.

use covenant_x402::{Catalog, PaymentRequirements, RegistryEntry};

use crate::config::HyreConfig;
use crate::manifest::{self, Endpoint};
use crate::{HyreError, Result};

/// Server title every Hyre entry carries in the gateway catalog, used
/// to disambiguate a slug shared with another provider.
pub const SERVER_TITLE: &str = "Hyre";

pub struct HyreCatalog {
    endpoints: Vec<Endpoint>,
    network: String,
    asset: String,
    base_url: String,
}

impl HyreCatalog {
    /// Parse and filter a manifest under the supplied config.
    pub fn from_manifest(manifest_json: &str, config: &HyreConfig) -> Result<Self> {
        let endpoints = manifest::parse(manifest_json)?
            .into_iter()
            .filter(|e| config.allows(&e.slug()))
            .collect();
        Ok(Self {
            endpoints,
            network: config.network.clone(),
            asset: config.asset.clone(),
            base_url: config.base_url.trim_end_matches('/').to_string(),
        })
    }

    /// Build from the crate's vendored manifest — the offline default.
    pub fn from_vendored(config: &HyreConfig) -> Result<Self> {
        Self::from_manifest(crate::VENDORED_MANIFEST, config)
    }

    /// Fetch the live manifest and rebuild. The daemon polls this on a
    /// refresh interval so a Hyre catalog change needs no rebuild.
    pub async fn refresh(http: &reqwest::Client, config: &HyreConfig) -> Result<Self> {
        Self::refresh_with_limits(http, config, crate::http::MAX_RESPONSE_BYTES).await
    }

    async fn refresh_with_limits(
        http: &reqwest::Client,
        config: &HyreConfig,
        max_bytes: usize,
    ) -> Result<Self> {
        let resp = http
            .get(config.manifest_url())
            .send()
            .await?
            .error_for_status()?;
        let body = crate::http::read_capped(resp, max_bytes, HyreError::Manifest).await?;
        Self::from_manifest(&body, config)
    }

    pub fn endpoints(&self) -> &[Endpoint] {
        &self.endpoints
    }

    pub fn is_empty(&self) -> bool {
        self.endpoints.is_empty()
    }

    /// Full endpoint URL for a path template, braces intact. The tool
    /// layer substitutes path parameters into this at call time.
    pub fn endpoint_url(&self, ep: &Endpoint) -> String {
        format!("{}{}", self.base_url, ep.path)
    }

    /// Materialise the gateway catalog. Each endpoint becomes one
    /// [`RegistryEntry`] with a single Solana/USDC pricing option; the
    /// authoritative amount still comes from the live 402, the catalog
    /// price is the discovery hint the pre-flight cap check reads.
    pub fn to_x402_catalog(&self) -> Catalog {
        let entries = self
            .endpoints
            .iter()
            .map(|ep| RegistryEntry {
                server_url: self.base_url.clone(),
                server_title: SERVER_TITLE.to_string(),
                endpoint: self.endpoint_url(ep),
                slug: ep.slug(),
                method: ep.method.clone(),
                description: if ep.summary.is_empty() {
                    ep.description.clone()
                } else {
                    ep.summary.clone()
                },
                pricing: vec![PaymentRequirements {
                    network: self.network.clone(),
                    asset: self.asset.clone(),
                    amount: ep.price_micro_usdc.to_string(),
                    amount_usdc: ep.price_micro_usdc as f64 / 1_000_000.0,
                    pay_to: crate::config::PAY_TO.to_string(),
                    scheme: "exact".to_string(),
                    extra: None,
                }],
            })
            .collect();
        Catalog::new(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendored_catalog_has_full_surface() {
        let cat = HyreCatalog::from_vendored(&HyreConfig::default()).unwrap();
        assert_eq!(cat.endpoints().len(), 24);
        let x402 = cat.to_x402_catalog();
        assert_eq!(x402.len(), 24);
        let pricing = x402
            .find_pricing(
                SERVER_TITLE,
                "ask",
                crate::config::SOLANA_NETWORK,
                crate::config::USDC_MINT,
            )
            .expect("ask priced on solana/usdc");
        assert_eq!(pricing.amount, "250000");
        assert_eq!(pricing.pay_to, crate::config::PAY_TO);
    }

    #[test]
    fn allowlist_filters_catalog() {
        let config = HyreConfig {
            allow: Some(vec!["defi/tvl".into(), "defi/yields".into()]),
            ..HyreConfig::default()
        };
        let cat = HyreCatalog::from_vendored(&config).unwrap();
        assert_eq!(cat.endpoints().len(), 2);
        assert!(cat
            .endpoints()
            .iter()
            .all(|e| e.slug().starts_with("defi/")));
    }

    #[test]
    fn endpoint_url_joins_base_and_template() {
        let cat = HyreCatalog::from_vendored(&HyreConfig::default()).unwrap();
        let curve = cat
            .endpoints()
            .iter()
            .find(|e| e.slug() == "trenches/curve/{mint}")
            .expect("curve endpoint present");
        assert_eq!(
            cat.endpoint_url(curve),
            "https://mpp.hyreagent.fun/trenches/curve/{mint}"
        );
    }

    #[tokio::test]
    async fn refresh_fetches_manifest_from_configured_host() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let manifest = serde_json::json!({
            "openapi": "3.1.0",
            "paths": { "/defi/tvl": { "get": {
                "operationId": "tvl", "summary": "TVL", "description": "",
                "x-payment-info": { "price": { "amount": "0.010000" } }
            }}}
        });
        Mock::given(method("GET"))
            .and(path("/openapi.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(manifest))
            .mount(&server)
            .await;

        let config = HyreConfig {
            base_url: server.uri(),
            ..HyreConfig::default()
        };
        let cat = HyreCatalog::refresh(&reqwest::Client::new(), &config)
            .await
            .expect("refresh");
        assert_eq!(cat.endpoints().len(), 1);
        assert_eq!(cat.endpoints()[0].slug(), "defi/tvl");
    }

    #[tokio::test]
    async fn refresh_rejects_non_2xx_manifest_host() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // A manifest host answering non-2xx must fail the refresh via
        // error_for_status, never hand the error body to the parser. Surfacing
        // HyreError::Http (not Manifest) proves the transport guard fired, so the
        // daemon keeps its prior catalog instead of decoding a 503 page as
        // endpoints or collapsing every tool on a momentary outage.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/openapi.json"))
            .respond_with(ResponseTemplate::new(503).set_body_string("manifest unavailable"))
            .mount(&server)
            .await;
        let config = HyreConfig {
            base_url: server.uri(),
            ..HyreConfig::default()
        };
        // HyreCatalog is not Debug, so match the Result directly rather than
        // unwrapping the error.
        let result = HyreCatalog::refresh(&reqwest::Client::new(), &config).await;
        assert!(
            matches!(result, Err(crate::HyreError::Http(_))),
            "expected HyreError::Http on a non-2xx manifest host",
        );
    }

    #[tokio::test]
    async fn refresh_rejects_oversized_manifest_body() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/openapi.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string("a".repeat(4096)))
            .mount(&server)
            .await;
        let config = HyreConfig {
            base_url: server.uri(),
            ..HyreConfig::default()
        };
        let result = HyreCatalog::refresh_with_limits(&reqwest::Client::new(), &config, 64).await;
        assert!(
            matches!(result, Err(crate::HyreError::Manifest(_))),
            "expected HyreError::Manifest on an oversized manifest body",
        );
    }

    #[test]
    fn to_x402_catalog_uses_summary_then_falls_back_to_description() {
        // The discovery `description` operators read for the pre-flight cap
        // check is the endpoint summary when present, else the full
        // description. The vendored fixture only exercises the summary-
        // present arm; construct both so an inverted branch or a dropped
        // is_empty() guard (which would publish a blank description) fails
        // here.
        let endpoint = |path: &str, summary: &str| Endpoint {
            path: path.into(),
            method: "POST".into(),
            operation_id: path.trim_start_matches('/').into(),
            summary: summary.into(),
            description: "the full description".into(),
            price_micro_usdc: 80_000,
            params: vec![],
            body: vec![],
        };
        let cat = HyreCatalog {
            endpoints: vec![
                endpoint("/with-summary", "a concise summary"),
                endpoint("/no-summary", ""),
            ],
            network: "solana:mainnet".into(),
            asset: "usdc-sol".into(),
            base_url: "https://api.hyre.example".into(),
        };

        let x402 = cat.to_x402_catalog();
        assert_eq!(
            x402.by_slug("with-summary").expect("priced").description,
            "a concise summary",
            "a present summary is the discovery description"
        );
        assert_eq!(
            x402.by_slug("no-summary").expect("priced").description,
            "the full description",
            "an empty summary falls back to the full description"
        );
    }
}
