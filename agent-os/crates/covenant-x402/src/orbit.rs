//! orbit-x402 registry client and provider catalog.
//!
//! orbit-x402 (api.orbitx402.com) aggregates x402 servers across
//! the ecosystem. Each registry entry pairs an endpoint URL with
//! zero or more pricing options. Polling this registry on a
//! refresh interval lets the daemon discover new providers without
//! a rebuild.
//!
//! This module exposes:
//!
//! - typed wire structs ([`RegistryResponse`], [`RegistryEntry`],
//!   [`Pagination`]) matching the registry's JSON shape,
//! - [`OrbitClient`] for fetching pages with built-in pagination
//!   ([`OrbitClient::fetch_all`]),
//! - [`Catalog`] for querying the materialised entry list.
//!
//! The catalog is intentionally a flat `Vec` with linear-scan
//! lookups — at the registry's current scale (low thousands of
//! entries) the indexes would be wasted memory. Switch to hashed
//! indexes if and when scan latency becomes measurable.

use serde::Deserialize;
use tracing::{debug, warn};

use crate::http::{read_capped, MAX_RESPONSE_BYTES};
use crate::{PaymentRequirements, Result, X402Error};

/// Default base URL for the public registry.
pub const DEFAULT_BASE_URL: &str = "https://api.orbitx402.com";

/// Default page size used by [`OrbitClient::fetch_all`].
pub const DEFAULT_PAGE_SIZE: usize = 100;

/// Safety cap: maximum pages [`OrbitClient::fetch_all`] requests
/// regardless of what `pagination.total` claims. Stops a runaway
/// loop against a registry returning nonsense pagination metadata.
pub const MAX_PAGES: usize = 200;

/// Top-level shape returned by `GET /api/services-list`.
#[derive(Debug, Clone, Deserialize)]
pub struct RegistryResponse {
    pub items: Vec<RegistryEntry>,
    pub pagination: Pagination,
}

/// Pagination metadata. `limit` + `offset` echo the request;
/// `total` is the registry's count of every entry available.
#[derive(Debug, Clone, Deserialize)]
pub struct Pagination {
    pub limit: usize,
    pub offset: usize,
    pub total: usize,
}

/// One entry in the registry — a paid (or free) endpoint plus its
/// provider context.
#[derive(Debug, Clone, Deserialize)]
pub struct RegistryEntry {
    /// API host the endpoint lives under.
    #[serde(rename = "serverUrl")]
    pub server_url: String,
    /// Human-readable provider name (e.g. `"Xona"`, `"Hyre"`).
    #[serde(rename = "serverTitle")]
    pub server_title: String,
    /// Full endpoint URL including path.
    pub endpoint: String,
    /// Path component without the host — convenient short id.
    pub slug: String,
    /// HTTP verb the endpoint expects.
    pub method: String,
    /// Capability summary from the provider.
    pub description: String,
    /// Accepted payment options. An empty array means the endpoint
    /// is free (or pre-authorised) and a direct call should
    /// succeed without a 402.
    #[serde(default)]
    pub pricing: Vec<PaymentRequirements>,
}

/// Client for the orbit-x402 registry.
pub struct OrbitClient {
    http: reqwest::Client,
    base_url: String,
    page_size: usize,
    max_bytes: usize,
}

impl OrbitClient {
    /// Public registry, default page size, default reqwest client.
    pub fn new() -> Self {
        Self::with(
            reqwest::Client::new(),
            DEFAULT_BASE_URL.into(),
            DEFAULT_PAGE_SIZE,
        )
    }

    /// Custom configuration — used by tests and self-hosted
    /// registries. `page_size` is clamped to at least 1.
    pub fn with(http: reqwest::Client, base_url: String, page_size: usize) -> Self {
        Self::with_limits(http, base_url, page_size, MAX_RESPONSE_BYTES)
    }

    fn with_limits(
        http: reqwest::Client,
        base_url: String,
        page_size: usize,
        max_bytes: usize,
    ) -> Self {
        Self {
            http,
            base_url,
            page_size: page_size.max(1),
            max_bytes,
        }
    }

    /// Fetches a single page. Prefer [`Self::fetch_all`] for the
    /// common "materialise everything" case.
    pub async fn fetch_page(&self, limit: usize, offset: usize) -> Result<RegistryResponse> {
        let url = format!(
            "{}/api/services-list?limit={}&offset={}",
            self.base_url.trim_end_matches('/'),
            limit.max(1),
            offset,
        );
        debug!(%url, "orbit-x402 fetch_page");
        let resp = self.http.get(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(X402Error::UnexpectedStatus(status.as_u16()));
        }
        let body = read_capped(resp, self.max_bytes, X402Error::Registry).await?;
        serde_json::from_str(&body).map_err(|e| X402Error::Registry(e.to_string()))
    }

    /// Iterates pages until every entry is loaded. Stops when the
    /// accumulated offset meets or exceeds `pagination.total`, or
    /// when a short page indicates the end of the list, or when
    /// [`MAX_PAGES`] requests have been made (safety cap).
    pub async fn fetch_all(&self) -> Result<Vec<RegistryEntry>> {
        let mut all = Vec::new();
        let mut offset = 0usize;
        for page_num in 0..MAX_PAGES {
            let page = self.fetch_page(self.page_size, offset).await?;
            let returned = page.items.len();
            all.extend(page.items);
            offset += returned;
            if returned == 0 || offset >= page.pagination.total || returned < self.page_size {
                debug!(
                    pages = page_num + 1,
                    fetched = all.len(),
                    "orbit-x402 fetch_all complete"
                );
                return Ok(all);
            }
        }
        warn!(
            max = MAX_PAGES,
            fetched = all.len(),
            "orbit-x402 fetch_all hit MAX_PAGES safety cap"
        );
        Ok(all)
    }
}

impl Default for OrbitClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Materialised view of an orbit fetch — a flat entry list with
/// lookup helpers.
///
/// Cheap to construct; the daemon typically rebuilds one on each
/// refresh cycle.
pub struct Catalog {
    entries: Vec<RegistryEntry>,
}

impl Catalog {
    pub fn new(entries: Vec<RegistryEntry>) -> Self {
        Self { entries }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate every entry. Lets sibling modules (e.g. the
    /// discover-and-pay flow) resolve entries without exposing the
    /// backing storage.
    pub fn iter(&self) -> impl Iterator<Item = &RegistryEntry> {
        self.entries.iter()
    }

    /// Every entry whose server title matches exactly. Useful for
    /// "list every endpoint Xona exposes".
    pub fn by_server<'a>(&'a self, title: &'a str) -> impl Iterator<Item = &'a RegistryEntry> + 'a {
        self.entries.iter().filter(move |e| e.server_title == title)
    }

    /// First entry whose slug matches. Slugs are not guaranteed
    /// unique across servers — combine with [`Self::by_server`]
    /// when ambiguity matters.
    pub fn by_slug(&self, slug: &str) -> Option<&RegistryEntry> {
        self.entries.iter().find(|e| e.slug == slug)
    }

    /// Find the pricing option for a `(server, slug)` endpoint that
    /// matches the supplied `(network, asset)`. Returns `None` when
    /// the endpoint is not in the catalog, has no pricing, or has
    /// no pricing option on the requested chain + asset.
    pub fn find_pricing(
        &self,
        server_title: &str,
        slug: &str,
        network: &str,
        asset: &str,
    ) -> Option<&PaymentRequirements> {
        self.entries
            .iter()
            .find(|e| e.server_title == server_title && e.slug == slug)?
            .pricing
            .iter()
            .find(|p| p.network == network && p.asset == asset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        matchers::{method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };

    /// Captured shape of one entry from the live registry, used to
    /// pin our serde mapping against the wire format.
    fn xona_entry_json(slug: &str, amount: &str) -> serde_json::Value {
        serde_json::json!({
            "serverUrl": "https://api.xona-agent.com",
            "serverTitle": "Xona",
            "endpoint": format!("https://api.xona-agent.com/{slug}"),
            "slug": slug,
            "method": "POST",
            "description": "AI-powered creative endpoint.",
            "pricing": [{
                "network": "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
                "asset": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                "amount": amount,
                "amountUsdc": 0.08,
                "payTo": "9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRSTEA",
                "scheme": "exact"
            }]
        })
    }

    fn page(items: Vec<serde_json::Value>, offset: usize, total: usize) -> serde_json::Value {
        serde_json::json!({
            "items": items,
            "pagination": {
                "limit": 2,
                "offset": offset,
                "total": total,
            }
        })
    }

    #[test]
    fn decodes_registry_entry_shape() {
        let entry: RegistryEntry =
            serde_json::from_value(xona_entry_json("image/creative-director", "80000"))
                .expect("decode");
        assert_eq!(entry.server_title, "Xona");
        assert_eq!(entry.slug, "image/creative-director");
        assert_eq!(entry.method, "POST");
        assert_eq!(entry.pricing.len(), 1);
        assert_eq!(entry.pricing[0].amount, "80000");
    }

    #[test]
    fn decodes_entry_with_empty_pricing() {
        let mut raw = xona_entry_json("free/endpoint", "0");
        raw["pricing"] = serde_json::json!([]);
        let entry: RegistryEntry = serde_json::from_value(raw).expect("decode");
        assert!(entry.pricing.is_empty());
    }

    #[tokio::test]
    async fn fetch_page_hits_expected_url() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/services-list"))
            .and(query_param("limit", "100"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(
                vec![xona_entry_json("a", "100")],
                0,
                1,
            )))
            .mount(&server)
            .await;

        let client = OrbitClient::with(reqwest::Client::new(), server.uri(), 100);
        let resp = client.fetch_page(100, 0).await.expect("page");
        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.pagination.total, 1);
    }

    #[tokio::test]
    async fn fetch_page_rejects_non_success_status() {
        // A registry that answers non-2xx (down, rate-limited, gateway error)
        // must surface UnexpectedStatus, never decode the error body as a
        // services list: a 503 page parsed as zero entries would silently empty
        // the catalog and make every discover-and-pay call miss.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/services-list"))
            .respond_with(ResponseTemplate::new(503).set_body_string("registry unavailable"))
            .mount(&server)
            .await;
        let client = OrbitClient::with(reqwest::Client::new(), server.uri(), 100);
        let err = client.fetch_page(100, 0).await.expect_err("non-success");
        assert!(
            matches!(err, X402Error::UnexpectedStatus(503)),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn fetch_page_response_body_is_bounded() {
        // The registry is third-party and untrusted: a page past the cap is
        // refused as a Registry decode failure rather than buffered into the
        // worker's memory. The under-cap parse path stays covered by
        // fetch_page_hits_expected_url, which runs against the default cap.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/services-list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(
                vec![xona_entry_json("a", "100")],
                0,
                1,
            )))
            .mount(&server)
            .await;
        // A cap below the page body trips the bounded read before it parses.
        let client = OrbitClient::with_limits(reqwest::Client::new(), server.uri(), 100, 64);
        let err = client.fetch_page(100, 0).await.expect_err("over cap");
        assert!(matches!(err, X402Error::Registry(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn fetch_page_rejects_malformed_response_body() {
        // A 2xx registry body that is not valid JSON — a CDN error page, a
        // truncated response — must surface X402Error::Registry at the
        // serde_json::from_str boundary rather than panic or be mis-parsed
        // into an empty catalog that makes every discover-and-pay call miss.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/services-list"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not a services-list"))
            .mount(&server)
            .await;
        let client = OrbitClient::with(reqwest::Client::new(), server.uri(), 100);
        let err = client.fetch_page(100, 0).await.expect_err("malformed body");
        assert!(matches!(err, X402Error::Registry(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn fetch_all_paginates_until_total_reached() {
        // total=5, page_size=2 → expect 3 pages (2, 2, 1).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/services-list"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(
                vec![xona_entry_json("a", "1"), xona_entry_json("b", "2")],
                0,
                5,
            )))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/services-list"))
            .and(query_param("offset", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(
                vec![xona_entry_json("c", "3"), xona_entry_json("d", "4")],
                2,
                5,
            )))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/services-list"))
            .and(query_param("offset", "4"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(
                vec![xona_entry_json("e", "5")],
                4,
                5,
            )))
            .mount(&server)
            .await;

        let client = OrbitClient::with(reqwest::Client::new(), server.uri(), 2);
        let entries = client.fetch_all().await.expect("all");
        assert_eq!(entries.len(), 5);
        assert_eq!(
            entries.iter().map(|e| e.slug.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c", "d", "e"]
        );
    }

    #[tokio::test]
    async fn fetch_all_stops_on_short_page() {
        // total=10 lies; we only return 1 item on the first page.
        // fetch_all must terminate on the short-page heuristic
        // rather than looping forever asking for offsets past the
        // actual data.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/services-list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(
                vec![xona_entry_json("a", "1")],
                0,
                10,
            )))
            .mount(&server)
            .await;

        let client = OrbitClient::with(reqwest::Client::new(), server.uri(), 2);
        let entries = client.fetch_all().await.expect("all");
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn catalog_filters_by_server_title() {
        let entries = vec![
            serde_json::from_value(xona_entry_json("a", "1")).unwrap(),
            serde_json::from_value(xona_entry_json("b", "2")).unwrap(),
            serde_json::from_value({
                let mut e = xona_entry_json("c", "3");
                e["serverTitle"] = "Other".into();
                e
            })
            .unwrap(),
        ];
        let cat = Catalog::new(entries);
        let xona: Vec<&RegistryEntry> = cat.by_server("Xona").collect();
        assert_eq!(xona.len(), 2);
        let other: Vec<&RegistryEntry> = cat.by_server("Other").collect();
        assert_eq!(other.len(), 1);
    }

    #[test]
    fn catalog_find_pricing_matches_chain_and_asset() {
        let entries =
            vec![
                serde_json::from_value(xona_entry_json("image/creative-director", "80000"))
                    .unwrap(),
            ];
        let cat = Catalog::new(entries);
        let p = cat
            .find_pricing(
                "Xona",
                "image/creative-director",
                "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
                "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            )
            .expect("pricing");
        assert_eq!(p.amount, "80000");
    }

    #[test]
    fn catalog_find_pricing_returns_none_on_chain_mismatch() {
        let entries =
            vec![
                serde_json::from_value(xona_entry_json("image/creative-director", "80000"))
                    .unwrap(),
            ];
        let cat = Catalog::new(entries);
        let p = cat.find_pricing("Xona", "image/creative-director", "base:8453", "usdc-base");
        assert!(p.is_none());
    }
}
