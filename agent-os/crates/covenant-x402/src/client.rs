//! The 402-then-pay loop.

use reqwest::{Method, Response, StatusCode};
use serde_json::Value;
use std::time::Duration;
use tracing::{debug, warn};

use crate::{
    signer::Signer,
    types::{Capability, PaymentRequirements},
    Result, X402Error,
};

/// Total per-request ceiling for the paid loop. The endpoint and its
/// facilitator are untrusted, so a hung or never-completing response must
/// not wedge the daemon worker forever. It is a safety ceiling, not an
/// enforcement of a per-challenge window: the paid retry settles
/// synchronously, so the ceiling sits well above a typical settlement to
/// avoid aborting a legitimate payment after funds may be committed.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// The reqwest client the daemon should drive [`Client::request_paid`]
/// with — bounded so an unresponsive endpoint cannot block a worker.
pub fn http_client() -> reqwest::Client {
    http_client_with_timeout(REQUEST_TIMEOUT)
}

fn http_client_with_timeout(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

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
        let requirements: Vec<PaymentRequirements> = serde_json::from_str(&challenge_text)
            .map_err(|e| X402Error::DecodeChallenge(e.to_string()))?;

        let chosen = pick_requirement(&requirements, capability).ok_or(X402Error::NoMatch)?;

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
/// Returns None when no requirement is on the right chain + asset,
/// or every matching requirement is priced at zero or above the
/// per-call cap — a zero price is a malformed challenge, not a free
/// call, and must never be signed.
fn pick_requirement<'a>(
    requirements: &'a [PaymentRequirements],
    capability: &Capability,
) -> Option<&'a PaymentRequirements> {
    requirements.iter().find(|r| {
        if r.network != capability.network || r.asset != capability.asset {
            return false;
        }
        match r.amount.parse::<u128>() {
            Ok(n) => n > 0 && n <= capability.per_call_cap,
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
    fn pick_rejects_zero_amount() {
        // A "0" amount parses cleanly and is trivially <= any cap, so without a
        // positive lower bound pick_requirement would hand it to build_payment
        // and sign a zero-price authorization — the zero-transfer hazard the
        // pick_skips_unparseable_amount comment names but does not itself cover.
        let reqs = vec![req("solana:mainnet", "usdc-sol", "0")];
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
    async fn slow_endpoint_times_out_instead_of_hanging() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/image/creative-director"))
            .respond_with(ResponseTemplate::new(402).set_delay(Duration::from_secs(30)))
            .mount(&server)
            .await;
        let client = Client::new(http_client_with_timeout(Duration::from_millis(50)));
        let err = client
            .request_paid(
                Method::POST,
                &format!("{}/image/creative-director", server.uri()),
                None,
                &cap("solana:mainnet", "usdc-sol", 100_000),
                &MockSigner,
            )
            .await
            .expect_err("must time out");
        assert!(matches!(err, X402Error::Http(_)), "got {err:?}");
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
            .respond_with(ResponseTemplate::new(402).set_body_json(challenge.clone()))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        // Second hit: x-payment present, return 200.
        Mock::given(method("POST"))
            .and(path("/image/creative-director"))
            .and(header_exists("x-payment"))
            .and(header("x-payment", "mock:solana:mainnet:80000"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })),
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
            .respond_with(ResponseTemplate::new(402).set_body_json(challenge))
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

    #[tokio::test]
    async fn gratis_2xx_first_hit_returns_without_signing() {
        // Some endpoints in a paid catalog are free. A 2xx on the first hit must
        // be returned as-is, never run through the signer and retried — which
        // would pay for a call that cost nothing. expect(1) proves the loop made
        // exactly one request.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/free"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = Client::new(reqwest::Client::new());
        let c = cap("solana:mainnet", "usdc-sol", 100_000);
        let resp = client
            .request_paid(
                Method::GET,
                &format!("{}/free", server.uri()),
                None,
                &c,
                &MockSigner,
            )
            .await
            .expect("gratis response");
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
    }

    #[tokio::test]
    async fn non_402_status_surfaces_unexpected_status() {
        // A first hit that is neither 2xx nor 402 is a provider fault, not a
        // challenge. request_paid must surface UnexpectedStatus with the code,
        // never parse a 5xx body as a requirements list.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/boom"))
            .respond_with(ResponseTemplate::new(500).set_body_string("upstream error"))
            .mount(&server)
            .await;
        let client = Client::new(reqwest::Client::new());
        let c = cap("solana:mainnet", "usdc-sol", 100_000);
        let err = client
            .request_paid(
                Method::GET,
                &format!("{}/boom", server.uri()),
                None,
                &c,
                &MockSigner,
            )
            .await
            .expect_err("non-402");
        assert!(
            matches!(err, X402Error::UnexpectedStatus(500)),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn malformed_402_challenge_surfaces_decode_error() {
        // A 402 whose body is not an array of requirements is a provider
        // wire-format change or a hostile response. request_paid must surface
        // DecodeChallenge, never sign against a requirement it failed to read.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/garbled"))
            .respond_with(
                ResponseTemplate::new(402).set_body_json(serde_json::json!({ "error": "pay up" })),
            )
            .mount(&server)
            .await;
        let client = Client::new(reqwest::Client::new());
        let c = cap("solana:mainnet", "usdc-sol", 100_000);
        let err = client
            .request_paid(
                Method::GET,
                &format!("{}/garbled", server.uri()),
                None,
                &c,
                &MockSigner,
            )
            .await
            .expect_err("malformed challenge");
        assert!(matches!(err, X402Error::DecodeChallenge(_)), "got {err:?}");
    }

    #[test]
    fn pick_skips_unparseable_amount() {
        // A requirement whose amount does not parse as u128 must be skipped, not
        // matched — signing against it would send an unbounded or zero transfer.
        let reqs = vec![req("solana:mainnet", "usdc-sol", "not-a-number")];
        let c = cap("solana:mainnet", "usdc-sol", 100_000);
        assert!(pick_requirement(&reqs, &c).is_none());
    }
}
