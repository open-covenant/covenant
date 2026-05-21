//! Discover-and-pay orchestration.
//!
//! [`Client::request_paid`] handles one already-resolved endpoint.
//! The registry side ([`crate::OrbitClient`] / [`Catalog`]) handles
//! discovery. This module joins them: given a materialised catalog,
//! a slug, and the caller's payment constraints, it resolves the
//! endpoint and runs the paid call in one step — the sequence the
//! demo binary would otherwise hand-roll.
//!
//! The catalog's pricing is only a discovery hint. The authoritative
//! amount comes from the live 402 challenge, and the per-call cap is
//! enforced against that amount inside [`Client::request_paid`]. This
//! helper still does a cheap pre-flight cap check against the catalog
//! price so an obviously-too-expensive call fails before any network
//! round-trip.

use reqwest::{Method, Response};
use serde_json::Value;

use crate::{
    orbit::{Catalog, RegistryEntry},
    Capability, Client, Result, Signer, X402Error,
};

/// One discover-and-pay call.
pub struct PaidRequest<'a> {
    /// Endpoint slug to resolve in the catalog.
    pub slug: &'a str,
    /// Optional server title to disambiguate a slug shared by more
    /// than one provider. When `None`, the first slug match wins.
    pub server_title: Option<&'a str>,
    /// CAIP-2 network the caller can settle on.
    pub network: &'a str,
    /// Asset (mint or contract address) the caller can pay in.
    pub asset: &'a str,
    /// Maximum atomic amount allowed for this call.
    pub per_call_cap: u128,
    /// Free-form provider tag carried into the [`Capability`] for
    /// audit. Not enforced.
    pub provider: &'a str,
    /// Optional JSON request body.
    pub body: Option<&'a Value>,
}

impl Catalog {
    /// Resolves `req.slug` in the catalog and runs the paid call.
    ///
    /// Resolution order:
    /// 1. Find the entry by `(server_title, slug)` if a title is
    ///    given, else by slug alone.
    /// 2. Pre-flight: if the entry advertises a price on the
    ///    requested `(network, asset)` that already exceeds
    ///    `per_call_cap`, fail with [`X402Error::NoMatch`] before
    ///    touching the network.
    /// 3. Issue the call through [`Client::request_paid`], which
    ///    re-checks the live 402 amount against the cap.
    pub async fn discover_and_pay(
        &self,
        client: &Client,
        signer: &dyn Signer,
        req: &PaidRequest<'_>,
    ) -> Result<Response> {
        let entry = self
            .resolve_entry(req.server_title, req.slug)
            .ok_or(X402Error::NoMatch)?;

        // Cheap pre-flight against the catalog's advertised price.
        if let Some(price) = entry
            .pricing
            .iter()
            .find(|p| p.network == req.network && p.asset == req.asset)
        {
            if let Ok(advertised) = price.amount.parse::<u128>() {
                if advertised > req.per_call_cap {
                    return Err(X402Error::NoMatch);
                }
            }
        }

        let method = entry
            .method
            .parse::<Method>()
            .unwrap_or(Method::POST);

        let capability = Capability {
            provider: req.provider.to_string(),
            network: req.network.to_string(),
            asset: req.asset.to_string(),
            per_call_cap: req.per_call_cap,
        };

        client
            .request_paid(method, &entry.endpoint, req.body, &capability, signer)
            .await
    }

    /// Resolve an entry by optional server title + slug.
    fn resolve_entry(
        &self,
        server_title: Option<&str>,
        slug: &str,
    ) -> Option<&RegistryEntry> {
        self.iter().find(|e| {
            e.slug == slug && server_title.is_none_or(|t| e.server_title == t)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MockSigner;
    use wiremock::{
        matchers::{header_exists, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn entry_json(server: &str, slug: &str, endpoint: &str, amount: &str) -> serde_json::Value {
        serde_json::json!({
            "serverUrl": "https://api.xona-agent.com",
            "serverTitle": server,
            "endpoint": endpoint,
            "slug": slug,
            "method": "POST",
            "description": "test endpoint",
            "pricing": [{
                "network": "solana:mainnet",
                "asset": "usdc-sol",
                "amount": amount,
                "amountUsdc": 0.08,
                "payTo": "9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRSTEA",
                "scheme": "exact"
            }]
        })
    }

    async fn challenge_server(slug_path: &str) -> MockServer {
        let server = MockServer::start().await;
        let challenge = serde_json::json!([{
            "network": "solana:mainnet",
            "asset": "usdc-sol",
            "amount": "80000",
            "amountUsdc": 0.08,
            "payTo": "9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRSTEA",
            "scheme": "exact"
        }]);
        Mock::given(method("POST"))
            .and(path(slug_path))
            .respond_with(ResponseTemplate::new(402).set_body_json(challenge))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(slug_path))
            .and(header_exists("x-payment"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })),
            )
            .mount(&server)
            .await;
        server
    }

    fn req<'a>(slug: &'a str, cap: u128) -> PaidRequest<'a> {
        PaidRequest {
            slug,
            server_title: None,
            network: "solana:mainnet",
            asset: "usdc-sol",
            per_call_cap: cap,
            provider: "xona",
            body: None,
        }
    }

    #[tokio::test]
    async fn resolves_by_slug_and_pays() {
        let server = challenge_server("/img").await;
        let entries = vec![serde_json::from_value(entry_json(
            "Xona",
            "img",
            &format!("{}/img", server.uri()),
            "80000",
        ))
        .unwrap()];
        let catalog = Catalog::new(entries);

        let resp = catalog
            .discover_and_pay(
                &Client::new(reqwest::Client::new()),
                &MockSigner,
                &req("img", 100_000),
            )
            .await
            .expect("paid");
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn unknown_slug_is_no_match() {
        let catalog = Catalog::new(vec![]);
        let err = catalog
            .discover_and_pay(
                &Client::new(reqwest::Client::new()),
                &MockSigner,
                &req("missing", 100_000),
            )
            .await
            .expect_err("no entry");
        assert!(matches!(err, X402Error::NoMatch));
    }

    #[tokio::test]
    async fn preflight_rejects_over_cap_before_network() {
        // Endpoint points at an unroutable URL; if the pre-flight cap
        // check did not fire we'd get an Http error instead of NoMatch.
        let entries = vec![serde_json::from_value(entry_json(
            "Xona",
            "pricey",
            "http://127.0.0.1:1/pricey",
            "200000",
        ))
        .unwrap()];
        let catalog = Catalog::new(entries);
        let err = catalog
            .discover_and_pay(
                &Client::new(reqwest::Client::new()),
                &MockSigner,
                &req("pricey", 100_000),
            )
            .await
            .expect_err("over cap");
        assert!(matches!(err, X402Error::NoMatch));
    }

    #[tokio::test]
    async fn server_title_disambiguates_shared_slug() {
        let server = challenge_server("/shared").await;
        let entries = vec![
            serde_json::from_value(entry_json(
                "Other",
                "shared",
                "http://127.0.0.1:1/shared",
                "80000",
            ))
            .unwrap(),
            serde_json::from_value(entry_json(
                "Xona",
                "shared",
                &format!("{}/shared", server.uri()),
                "80000",
            ))
            .unwrap(),
        ];
        let catalog = Catalog::new(entries);
        let mut request = req("shared", 100_000);
        request.server_title = Some("Xona");

        let resp = catalog
            .discover_and_pay(&Client::new(reqwest::Client::new()), &MockSigner, &request)
            .await
            .expect("paid via Xona entry");
        assert_eq!(resp.status(), 200);
    }
}
