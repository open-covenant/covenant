//! The 402-then-pay loop.

use reqwest::{Method, Response, StatusCode};
use serde_json::Value;
use tracing::{debug, warn};

use crate::{
    signer::Signer,
    types::{Capability, PaymentRequirements},
    Result, X402Error,
};

/// Outbound x402 client.
///
/// One client wraps one [`reqwest::Client`] — re-use a single
/// instance across many capability-scoped calls.
pub struct Client {
    http: reqwest::Client,
}

impl Client {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    /// Issue a paid request.
    ///
    /// 1. Hits `url` with `method` (and optional JSON `body`).
    /// 2. On a 402 response, parses the body as an array of
    ///    [`PaymentRequirements`].
    /// 3. Picks the first requirement whose `network` + `asset`
    ///    match the capability and whose `amount` is within
    ///    `per_call_cap`. Returns [`X402Error::NoMatch`] when none
    ///    qualifies.
    /// 4. Hands the matched requirement to the signer and retries
    ///    the same request with the resulting `x-payment` header.
    /// 5. Returns the paid response.
    ///
    /// A gratis 2xx on the first hit is returned as-is — some
    /// endpoints in a paid catalog may be free.
    pub async fn request_paid(
        &self,
        method: Method,
        url: &str,
        body: Option<&Value>,
        capability: &Capability,
        signer: &dyn Signer,
    ) -> Result<Response> {
        let initial = self.send(method.clone(), url, body, None).await?;
        let status = initial.status();
        if status.is_success() {
            return Ok(initial);
        }
        if status != StatusCode::PAYMENT_REQUIRED {
            return Err(X402Error::UnexpectedStatus(status.as_u16()));
        }

        let challenge_text = initial.text().await?;
        let requirements: Vec<PaymentRequirements> =
            serde_json::from_str(&challenge_text)
                .map_err(|e| X402Error::DecodeChallenge(e.to_string()))?;

        let chosen = pick_requirement(&requirements, capability)
            .ok_or(X402Error::NoMatch)?;

        debug!(
            network = %chosen.network,
            amount = %chosen.amount,
            "x402 challenge matched capability; signing"
        );

        let header_value = signer.build_payment(chosen).await?;

        self.send(method, url, body, Some(&header_value)).await
    }

    async fn send(
        &self,
        method: Method,
        url: &str,
        body: Option<&Value>,
        payment_header: Option<&str>,
    ) -> Result<Response> {
        let mut req = self.http.request(method, url);
        if let Some(b) = body {
            req = req.json(b);
        }
        if let Some(h) = payment_header {
            req = req.header("x-payment", h);
        }
        Ok(req.send().await?)
    }
}

/// Picks the first requirement that matches the capability.
///
/// Returns None when no requirement is on the right chain + asset
/// or all matching requirements exceed the per-call cap.
fn pick_requirement<'a>(
    requirements: &'a [PaymentRequirements],
    capability: &Capability,
) -> Option<&'a PaymentRequirements> {
    requirements.iter().find(|r| {
        if r.network != capability.network || r.asset != capability.asset {
            return false;
        }
        match r.amount.parse::<u128>() {
            Ok(n) => n <= capability.per_call_cap,
            Err(_) => {
                warn!(amount = %r.amount, "x402 requirement has unparseable amount");
                false
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signer::MockSigner;
    use wiremock::{
        matchers::{header, header_exists, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn req(network: &str, asset: &str, amount: &str) -> PaymentRequirements {
        PaymentRequirements {
            network: network.into(),
            asset: asset.into(),
            amount: amount.into(),
            amount_usdc: 0.0,
            pay_to: "AnyPubkey".into(),
            scheme: "exact".into(),
            extra: None,
        }
    }

    fn cap(network: &str, asset: &str, per_call: u128) -> Capability {
        Capability {
            provider: "test".into(),
            network: network.into(),
            asset: asset.into(),
            per_call_cap: per_call,
        }
    }

    #[test]
    fn pick_matches_first_qualifying_requirement() {
        let reqs = vec![
            req("base:8453", "usdc-base", "80000"),
            req("solana:mainnet", "usdc-sol", "80000"),
        ];
        let c = cap("solana:mainnet", "usdc-sol", 100_000);
        let picked = pick_requirement(&reqs, &c).expect("match");
        assert_eq!(picked.network, "solana:mainnet");
    }

    #[test]
    fn pick_rejects_amount_over_cap() {
        let reqs = vec![req("solana:mainnet", "usdc-sol", "200000")];
        let c = cap("solana:mainnet", "usdc-sol", 100_000);
        assert!(pick_requirement(&reqs, &c).is_none());
    }

    #[test]
    fn pick_rejects_wrong_chain() {
        let reqs = vec![req("base:8453", "usdc-base", "80000")];
        let c = cap("solana:mainnet", "usdc-sol", 100_000);
        assert!(pick_requirement(&reqs, &c).is_none());
    }

    #[tokio::test]
    async fn full_loop_pays_and_returns_success() {
        let server = MockServer::start().await;

        let challenge = serde_json::json!([{
            "network": "solana:mainnet",
            "asset": "usdc-sol",
            "amount": "80000",
            "amountUsdc": 0.08,
            "payTo": "9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRSTEA",
            "scheme": "exact"
        }]);

        // First hit: no x-payment header, return 402 + challenge.
        Mock::given(method("POST"))
            .and(path("/image/creative-director"))
            .respond_with(
                ResponseTemplate::new(402)
                    .set_body_json(challenge.clone()),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;

        // Second hit: x-payment present, return 200.
        Mock::given(method("POST"))
            .and(path("/image/creative-director"))
            .and(header_exists("x-payment"))
            .and(header(
                "x-payment",
                "mock:solana:mainnet:80000",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "ok": true })),
            )
            .mount(&server)
            .await;

        let client = Client::new(reqwest::Client::new());
        let c = cap("solana:mainnet", "usdc-sol", 100_000);
        let signer = MockSigner;

        let resp = client
            .request_paid(
                Method::POST,
                &format!("{}/image/creative-director", server.uri()),
                None,
                &c,
                &signer,
            )
            .await
            .expect("paid response");

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
    }

    #[tokio::test]
    async fn loop_returns_no_match_when_capability_too_low() {
        let server = MockServer::start().await;
        let challenge = serde_json::json!([{
            "network": "solana:mainnet",
            "asset": "usdc-sol",
            "amount": "200000",
            "amountUsdc": 0.20,
            "payTo": "AnyPubkey",
            "scheme": "exact"
        }]);

        Mock::given(method("GET"))
            .and(path("/expensive"))
            .respond_with(
                ResponseTemplate::new(402).set_body_json(challenge),
            )
            .mount(&server)
            .await;

        let client = Client::new(reqwest::Client::new());
        let c = cap("solana:mainnet", "usdc-sol", 100_000);
        let signer = MockSigner;

        let err = client
            .request_paid(
                Method::GET,
                &format!("{}/expensive", server.uri()),
                None,
                &c,
                &signer,
            )
            .await
            .expect_err("over cap");
        assert!(matches!(err, X402Error::NoMatch));
    }
}
