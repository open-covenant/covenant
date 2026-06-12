//! One-shot RepoScan call.
//!
//! Posts to `https://api.zauth.inc/x402/reposcan` with the repo URL in
//! the body, walks the 402 challenge in the `payment-required` header,
//! signs the chosen accept with the caller-supplied signer, retries
//! with the `x-payment` header, and returns the result body.
//!
//! Idempotency is server-side: the same `repoUrl` may return a cached
//! result without a payment challenge. The caller pays only when the
//! first hit is a 402.

use covenant_x402::Signer;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::debug;

use crate::challenge;
use crate::{Result, ZauthError};

const DEFAULT_BASE_URL: &str = "https://api.zauth.inc";

#[derive(Debug, Clone)]
pub struct RepoScanClient {
    http: reqwest::Client,
    base_url: String,
}

impl RepoScanClient {
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

    pub async fn scan(
        &self,
        req: &RepoScanRequest<'_>,
        signer: &dyn Signer,
    ) -> Result<RepoScanResult> {
        let url = format!("{}/x402/reposcan", self.base_url);
        let body = json!({ "repoUrl": req.repo_url });

        let first = self.http.post(&url).json(&body).send().await?;
        let status = first.status();

        if status.is_success() {
            let body_text = first.text().await?;
            return Ok(RepoScanResult {
                status: status.as_u16(),
                body: body_text,
                paid_amount: None,
            });
        }
        if status.as_u16() != 402 {
            return Err(ZauthError::UnexpectedStatus(status.as_u16()));
        }

        let headers = first.headers().clone();
        let _drained = first.text().await?;
        let parsed = challenge::decode_from_headers(&headers)?;

        let accept = challenge::select(
            &parsed.accepts,
            req.network,
            req.asset,
            req.expected_pay_to,
            req.per_call_cap,
        )?;

        debug!(
            network = %accept.network,
            amount = %accept.amount,
            pay_to = %accept.pay_to,
            "zauth reposcan: paying challenge"
        );

        let requirements = challenge::to_payment_requirements(accept);
        let header_value = signer
            .build_payment(&requirements)
            .await
            .map_err(|e| ZauthError::Sign(e.to_string()))?;

        let paid = self
            .http
            .post(&url)
            .header("x-payment", header_value)
            .json(&body)
            .send()
            .await?;
        let paid_status = paid.status();
        let paid_body = paid.text().await.unwrap_or_default();
        let paid_amount = paid_status
            .is_success()
            .then(|| requirements.amount.clone());
        Ok(RepoScanResult {
            status: paid_status.as_u16(),
            body: paid_body,
            paid_amount,
        })
    }
}

#[derive(Debug, Clone)]
pub struct RepoScanRequest<'a> {
    pub repo_url: &'a str,
    /// CAIP-2 network the caller's signer will settle on.
    pub network: &'a str,
    /// Asset (USDC mint or contract) the caller will pay in.
    pub asset: &'a str,
    /// Treasury address the caller pins as `payTo`. Use one of
    /// [`crate::treasury::SOLANA`] / [`crate::treasury::BASE`].
    pub expected_pay_to: &'a str,
    /// Maximum atomic amount the caller will pay for one scan.
    /// RepoScan is currently 50000 atomic (0.05 USDC, 6 decimals).
    pub per_call_cap: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepoScanResult {
    pub status: u16,
    pub body: String,
    /// Atomic amount actually settled. `None` when the first hit was
    /// a gratis 2xx (cached) or when the paid retry was rejected.
    pub paid_amount: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    use covenant_x402::MockSigner;
    use wiremock::matchers::{header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const LIVE_DECODED: &str = r#"{
        "x402Version": 2,
        "error": "Payment required",
        "accepts": [
            {
                "scheme": "exact",
                "network": "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
                "amount": "50000",
                "asset": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                "payTo": "ZAU64eKWAgiGNux8BzvgRn8RvWqFhdMVrpJytF7V1qm",
                "maxTimeoutSeconds": 300,
                "extra": { "feePayer": "ZAU64eKWAgiGNux8BzvgRn8RvWqFhdMVrpJytF7V1qm" }
            }
        ]
    }"#;

    fn req<'a>() -> RepoScanRequest<'a> {
        RepoScanRequest {
            repo_url: "https://github.com/open-covenant/covenant",
            network: crate::networks::SOLANA_MAINNET,
            asset: crate::assets::USDC_SOLANA,
            expected_pay_to: crate::treasury::SOLANA,
            per_call_cap: 50_000,
        }
    }

    fn challenge_header() -> String {
        B64.encode(LIVE_DECODED.as_bytes())
    }

    #[tokio::test]
    async fn full_loop_pays_and_returns_result() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/x402/reposcan"))
            .respond_with(
                ResponseTemplate::new(402)
                    .insert_header("payment-required", challenge_header().as_str())
                    .set_body_string("{}"),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/x402/reposcan"))
            .and(header_exists("x-payment"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "scanned",
                "score": 92,
                "scanId": "abc123"
            })))
            .mount(&server)
            .await;

        let client = RepoScanClient::with_base_url(reqwest::Client::new(), server.uri());
        let result = client.scan(&req(), &MockSigner).await.expect("paid");
        assert_eq!(result.status, 200);
        assert_eq!(result.paid_amount.as_deref(), Some("50000"));
        let body: serde_json::Value = serde_json::from_str(&result.body).unwrap();
        assert_eq!(body["score"], 92);
    }

    #[tokio::test]
    async fn cached_gratis_2xx_skips_payment() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/x402/reposcan"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "cached", "score": 88
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = RepoScanClient::with_base_url(reqwest::Client::new(), server.uri());
        let result = client.scan(&req(), &MockSigner).await.expect("gratis");
        assert_eq!(result.status, 200);
        assert!(result.paid_amount.is_none());
    }

    #[tokio::test]
    async fn missing_challenge_header_surfaces_typed_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/x402/reposcan"))
            .respond_with(ResponseTemplate::new(402).set_body_string("{}"))
            .mount(&server)
            .await;
        let client = RepoScanClient::with_base_url(reqwest::Client::new(), server.uri());
        let err = client
            .scan(&req(), &MockSigner)
            .await
            .expect_err("missing header");
        assert!(matches!(err, ZauthError::MissingChallengeHeader));
    }

    #[tokio::test]
    async fn non_402_error_response_surfaces_unexpected_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/x402/reposcan"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let client = RepoScanClient::with_base_url(reqwest::Client::new(), server.uri());
        let err = client.scan(&req(), &MockSigner).await.expect_err("503");
        assert!(matches!(err, ZauthError::UnexpectedStatus(503)));
    }

    #[tokio::test]
    async fn rejected_retry_records_no_paid_amount() {
        // The retry can be rejected (bad signature, expired blockhash,
        // insufficient funds). The loop must report that status with
        // paid_amount None so no settlement is credited for a transfer
        // that never landed.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/x402/reposcan"))
            .respond_with(
                ResponseTemplate::new(402)
                    .insert_header("payment-required", challenge_header().as_str())
                    .set_body_string("{}"),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/x402/reposcan"))
            .and(header_exists("x-payment"))
            .respond_with(ResponseTemplate::new(402).set_body_string("payment invalid"))
            .mount(&server)
            .await;

        let client = RepoScanClient::with_base_url(reqwest::Client::new(), server.uri());
        let result = client
            .scan(&req(), &MockSigner)
            .await
            .expect("loop surfaces rejection, not an error");
        assert_eq!(result.status, 402);
        assert!(result.paid_amount.is_none());
    }

    #[tokio::test]
    async fn signer_failure_surfaces_as_typed_sign_error() {
        // The money path must fail closed: if the signer cannot build the
        // payment header (custody key offline, blockhash expired), scan() must
        // return a typed ZauthError::Sign and stop, never panic and never fall
        // through to the paid POST for a payment that was never signed.
        struct FailingSigner;
        #[async_trait::async_trait]
        impl Signer for FailingSigner {
            async fn build_payment(
                &self,
                _requirements: &covenant_x402::PaymentRequirements,
            ) -> covenant_x402::Result<String> {
                Err(covenant_x402::X402Error::Sign(
                    "funding key unavailable".into(),
                ))
            }
        }

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/x402/reposcan"))
            .respond_with(
                ResponseTemplate::new(402)
                    .insert_header("payment-required", challenge_header().as_str())
                    .set_body_string("{}"),
            )
            .mount(&server)
            .await;
        let client = RepoScanClient::with_base_url(reqwest::Client::new(), server.uri());
        let err = client
            .scan(&req(), &FailingSigner)
            .await
            .expect_err("a signer failure must abort the scan");
        assert!(
            matches!(err, ZauthError::Sign(ref m) if m.contains("funding key unavailable")),
            "got {err:?}",
        );
    }
}
