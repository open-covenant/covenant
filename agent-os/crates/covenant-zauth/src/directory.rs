//! Read-only client for `https://api.zauth.inc/api/directory`.

use serde::{Deserialize, Serialize};

use crate::{Result, ZauthError};

const DEFAULT_BASE_URL: &str = "https://api.zauth.inc";

#[derive(Debug, Clone)]
pub struct DirectoryClient {
    http: reqwest::Client,
    base_url: String,
}

impl DirectoryClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            base_url: DEFAULT_BASE_URL.into(),
        }
    }

    pub fn with_base_url(http: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self {
            http,
            base_url: base_url.into(),
        }
    }

    /// Page through the directory. `verified=true` restricts to the
    /// crawler-verified subset (176 of 2,672 at the time of writing).
    pub async fn list(&self, query: ListQuery) -> Result<DirectoryPage> {
        let mut req = self.http.get(format!("{}/api/directory", self.base_url));
        if let Some(v) = query.verified {
            req = req.query(&[("verified", v.to_string())]);
        }
        if let Some(n) = query.network.as_deref() {
            req = req.query(&[("network", n)]);
        }
        if let Some(l) = query.limit {
            req = req.query(&[("limit", l.to_string())]);
        }
        if let Some(o) = query.offset {
            req = req.query(&[("offset", o.to_string())]);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(ZauthError::UnexpectedStatus(status.as_u16()));
        }
        let text = resp.text().await?;
        serde_json::from_str(&text)
            .map_err(|e| ZauthError::DecodeDirectory(format!("{e}: body={text}")))
    }
}

#[derive(Debug, Clone, Default)]
pub struct ListQuery {
    pub verified: Option<bool>,
    pub network: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DirectoryPage {
    pub endpoints: Vec<DirectoryEndpoint>,
    pub pagination: Pagination,
    pub stats: DirectoryStats,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DirectoryEndpoint {
    pub url: String,
    pub method: String,
    /// CAIP-2 or short network id. Optional: a small number of entries
    /// in the live directory have `network: null` (under-crawled or
    /// pending classification).
    #[serde(default)]
    pub network: Option<String>,
    /// Display price in USDC. Optional: some entries have
    /// `priceUsdc: null` (e.g. attestation endpoints with no tariff).
    #[serde(rename = "priceUsdc", default)]
    pub price_usdc: Option<String>,
    pub status: EndpointStatus,
    /// Percentage [0..100]. Wire is int OR float OR null; decode as
    /// f64 to absorb the int/float drift and Option for the null.
    #[serde(rename = "successRate", default)]
    pub success_rate: Option<f64>,
    #[serde(rename = "totalCalls", default)]
    pub total_calls: u64,
    #[serde(rename = "lastWorking", default)]
    pub last_working: Option<String>,
    #[serde(rename = "lastTested", default)]
    pub last_tested: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub verified: bool,
    /// Percentage [0..100]. Same int/float/null drift as success_rate.
    #[serde(default)]
    pub uptime: Option<f64>,
    #[serde(rename = "avgResponseTime", default)]
    pub avg_response_time: Option<f64>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EndpointStatus {
    Working,
    Failing,
    Flaky,
    Untested,
    OverBudget,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pagination {
    pub total: u64,
    pub limit: u32,
    pub offset: u32,
    #[serde(rename = "hasMore")]
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DirectoryStats {
    #[serde(rename = "totalEndpoints")]
    pub total_endpoints: u64,
    pub verified: u64,
    pub working: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Recorded body from the live directory, 2026-06-03.
    const LIVE_SAMPLE: &str = r#"{
        "endpoints": [{
            "url": "https://mesh.heurist.xyz/x402/solana/agents/BaseUSDCForensicsAgent/usdc_top_funders",
            "method": "POST",
            "network": "solana",
            "priceUsdc": "0.03",
            "status": "WORKING",
            "successRate": 100,
            "totalCalls": 109,
            "lastWorking": "2026-01-16T15:05:42.394Z",
            "lastTested": "2026-01-16T15:05:42.394Z",
            "title": null,
            "description": null,
            "verified": true,
            "params": null,
            "responseShape": null,
            "meaningfulness": null,
            "uptime": 100,
            "avgResponseTime": null,
            "category": null,
            "tags": []
        }],
        "pagination": { "total": 2672, "limit": 1, "offset": 0, "hasMore": true },
        "stats": { "totalEndpoints": 2672, "verified": 176, "working": 2080 }
    }"#;

    #[test]
    fn parses_live_response() {
        let page: DirectoryPage = serde_json::from_str(LIVE_SAMPLE).expect("decode");
        assert_eq!(page.endpoints.len(), 1);
        let ep = &page.endpoints[0];
        assert!(ep.verified);
        assert_eq!(ep.status, EndpointStatus::Working);
        assert_eq!(ep.network.as_deref(), Some("solana"));
        assert_eq!(ep.price_usdc.as_deref(), Some("0.03"));
        assert_eq!(page.pagination.total, 2672);
        assert_eq!(page.stats.verified, 176);
    }

    #[test]
    fn parses_entries_with_null_network_and_int_or_float_percentages() {
        // Production data was caught serving:
        //   - `network: null` on a small number of entries
        //   - `successRate` and `uptime` as `int` on some entries,
        //     `float` on others, and `null` on freshly-crawled ones.
        // The schema must absorb all three without erroring.
        let raw = r#"{
            "endpoints": [
                {"url":"a","method":"GET","network":null,"priceUsdc":null,"status":"UNTESTED",
                 "successRate":null,"totalCalls":0,"uptime":null,"verified":false,"tags":[]},
                {"url":"b","method":"POST","network":"solana","priceUsdc":"0.01","status":"WORKING",
                 "successRate":100,"totalCalls":7,"uptime":100,"verified":true,"tags":[]},
                {"url":"c","method":"GET","network":"base","priceUsdc":"0.002","status":"WORKING",
                 "successRate":33.33,"totalCalls":12,"uptime":33.33,"verified":true,"tags":[]}
            ],
            "pagination": {"total":3,"limit":3,"offset":0,"hasMore":false},
            "stats": {"totalEndpoints":3,"verified":2,"working":2}
        }"#;
        let page: DirectoryPage = serde_json::from_str(raw).expect("decode permissive");
        assert!(page.endpoints[0].network.is_none());
        assert!(page.endpoints[0].success_rate.is_none());
        assert_eq!(page.endpoints[1].success_rate, Some(100.0));
        assert!((page.endpoints[2].uptime.unwrap() - 33.33).abs() < 1e-9);
    }

    #[test]
    fn parses_entry_with_null_price_and_success_rate() {
        // Real production sample: some attestation endpoints have
        // priceUsdc=null and successRate=null. Must decode, not error.
        let raw = r#"{
            "endpoints": [{
                "url": "https://thesealer.xyz/api/attest",
                "method": "POST",
                "network": "base",
                "priceUsdc": null,
                "status": "WORKING",
                "successRate": null,
                "totalCalls": 1,
                "lastWorking": "2026-04-08T14:24:19.023Z",
                "lastTested": "2026-04-08T14:24:19.023Z",
                "title": null,
                "description": null,
                "verified": true,
                "uptime": null,
                "avgResponseTime": null,
                "category": null,
                "tags": []
            }],
            "pagination": { "total": 1, "limit": 1, "offset": 0, "hasMore": false },
            "stats": { "totalEndpoints": 1, "verified": 1, "working": 1 }
        }"#;
        let page: DirectoryPage = serde_json::from_str(raw).expect("decode null fields");
        assert!(page.endpoints[0].price_usdc.is_none());
        assert!(page.endpoints[0].success_rate.is_none());
        assert!(page.endpoints[0].uptime.is_none());
    }

    #[test]
    fn parses_each_endpoint_status_variant() {
        for variant in ["WORKING", "FAILING", "FLAKY", "UNTESTED", "OVER_BUDGET"] {
            let json = format!("\"{variant}\"");
            let parsed: EndpointStatus = serde_json::from_str(&json).expect("decode");
            let re = serde_json::to_string(&parsed).expect("encode");
            assert_eq!(re, json, "round-trip {variant}");
        }
    }

    #[tokio::test]
    async fn list_passes_query_params() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/directory"))
            .and(query_param("verified", "true"))
            .and(query_param("network", "solana"))
            .and(query_param("limit", "5"))
            .respond_with(ResponseTemplate::new(200).set_body_string(LIVE_SAMPLE))
            .mount(&server)
            .await;

        let client = DirectoryClient::with_base_url(reqwest::Client::new(), server.uri());
        let page = client
            .list(ListQuery {
                verified: Some(true),
                network: Some("solana".into()),
                limit: Some(5),
                offset: None,
            })
            .await
            .expect("list");
        assert_eq!(page.endpoints.len(), 1);
    }

    #[tokio::test]
    async fn surfaces_unexpected_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/directory"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let client = DirectoryClient::with_base_url(reqwest::Client::new(), server.uri());
        let err = client.list(ListQuery::default()).await.expect_err("err");
        assert!(matches!(err, ZauthError::UnexpectedStatus(503)));
    }

    #[tokio::test]
    async fn surfaces_decode_error_with_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/directory"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{not-json"))
            .mount(&server)
            .await;

        let client = DirectoryClient::with_base_url(reqwest::Client::new(), server.uri());
        let err = client.list(ListQuery::default()).await.expect_err("err");
        assert!(
            matches!(err, ZauthError::DecodeDirectory(m) if m.contains("body=")),
            "expected DecodeDirectory carrying the raw body"
        );
    }
}
