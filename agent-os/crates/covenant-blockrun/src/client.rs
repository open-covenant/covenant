//! The paid round-trip: hit a BlockRun endpoint, satisfy the 402, and capture
//! the whole exchange as a [`CallReceipt`]. The signing itself is delegated to a
//! `covenant_x402::Signer`, so this crate never holds a key.

use std::sync::Arc;

use covenant_x402::Signer;
use reqwest::StatusCode;
use serde_json::Value;

use crate::challenge::Challenge;
use crate::receipt::{CallReceipt, PaymentInfo, RoutingClaim};
use crate::{BlockRunError, Result};

/// Header BlockRun uses to carry the settlement signature on the paid response.
const RECEIPT_HEADER: &str = "x-payment-receipt";
const REQUIRED_HEADER: &str = "x-payment-required";
const PAYMENT_HEADER: &str = "x-payment";

pub struct BlockRunClient {
    http: reqwest::Client,
    base_url: String,
    signer: Arc<dyn Signer>,
}

/// A completed call: the provider's response plus the receipt binding it.
#[derive(Debug, Clone)]
pub struct PaidCall {
    pub response: Value,
    pub receipt: CallReceipt,
}

impl BlockRunClient {
    pub fn new(base_url: impl Into<String>, signer: Arc<dyn Signer>) -> Self {
        Self::with_client(covenant_x402::http_client(), base_url, signer)
    }

    pub fn with_client(
        http: reqwest::Client,
        base_url: impl Into<String>,
        signer: Arc<dyn Signer>,
    ) -> Self {
        BlockRunClient {
            http,
            base_url: base_url.into(),
            signer,
        }
    }

    /// POST `body` to `endpoint`, paying the x402 challenge if one is returned,
    /// and return the response bound into a receipt.
    pub async fn call(&self, endpoint: &str, body: Value) -> Result<PaidCall> {
        let url = format!("{}{}", self.base_url.trim_end_matches('/'), endpoint);
        let first = self.http.post(&url).json(&body).send().await?;

        if first.status() == StatusCode::PAYMENT_REQUIRED {
            return self.pay_and_capture(endpoint, &url, body, first).await;
        }
        // Free endpoint (or an error). Surface errors; otherwise emit a receipt
        // with an empty payment so the exchange is still recorded.
        let routing = RoutingClaim::from_headers(first.headers());
        let response = read_json(first).await?;
        let payment = PaymentInfo {
            network: String::new(),
            asset: String::new(),
            amount: "0".into(),
            amount_usdc: 0.0,
            pay_to: String::new(),
            tx: None,
        };
        let receipt = CallReceipt::from_exchange(endpoint, &body, &response, payment, routing);
        Ok(PaidCall { response, receipt })
    }

    async fn pay_and_capture(
        &self,
        endpoint: &str,
        url: &str,
        body: Value,
        challenge_resp: reqwest::Response,
    ) -> Result<PaidCall> {
        let challenge = decode_challenge(challenge_resp).await?;
        let accept = challenge
            .pick(None)
            .ok_or_else(|| BlockRunError::Challenge("no payment options offered".into()))?
            .clone();

        let header = self
            .signer
            .build_payment(&accept.to_requirements())
            .await
            .map_err(|e| BlockRunError::Signer(e.to_string()))?;

        let paid = self
            .http
            .post(url)
            .header(PAYMENT_HEADER, header)
            .json(&body)
            .send()
            .await?;

        let routing = RoutingClaim::from_headers(paid.headers());
        let tx = paid
            .headers()
            .get(RECEIPT_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let status = paid.status();
        let response = read_json(paid).await?;
        if !status.is_success() {
            return Err(BlockRunError::Api {
                status: status.as_u16(),
                message: response.to_string(),
            });
        }

        let payment = PaymentInfo {
            network: accept.network.clone(),
            asset: accept.asset.clone(),
            amount: accept.amount.clone(),
            amount_usdc: accept.amount_usdc(),
            pay_to: accept.pay_to.clone(),
            tx,
        };
        let receipt = CallReceipt::from_exchange(endpoint, &body, &response, payment, routing);
        Ok(PaidCall { response, receipt })
    }
}

/// Decode the 402 challenge from the `x-payment-required` header, falling back
/// to `www-authenticate`, then to the response body.
async fn decode_challenge(resp: reqwest::Response) -> Result<Challenge> {
    let header = resp
        .headers()
        .get(REQUIRED_HEADER)
        .or_else(|| resp.headers().get(reqwest::header::WWW_AUTHENTICATE))
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if let Some(h) = header {
        if let Ok(c) = Challenge::from_header_b64(&h) {
            return Ok(c);
        }
    }
    let body = read_json(resp).await?;
    Challenge::from_json(&body)
}

async fn read_json(resp: reqwest::Response) -> Result<Value> {
    let status = resp.status();
    let text = resp.text().await?;
    serde_json::from_str(&text).map_err(|_| BlockRunError::Api {
        status: status.as_u16(),
        message: if text.is_empty() {
            "empty response".into()
        } else {
            text
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // BlockRun's real 402 header for a default gpt-4o-mini call.
    const LIVE_B64: &str = "eyJ4NDAyVmVyc2lvbiI6MiwiYWNjZXB0cyI6W3sic2NoZW1lIjoiZXhhY3QiLCJuZXR3b3JrIjoiZWlwMTU1Ojg0NTMiLCJhbW91bnQiOiIzMDAwIiwiYXNzZXQiOiIweDgzMzU4OWZDRDZlRGI2RTA4ZjRjN0MzMkQ0ZjcxYjU0YmRBMDI5MTMiLCJwYXlUbyI6IjB4ZTkwMzAwMTRGNURBZTIxN2QwQTE1MmYwMkEwNDM1NjdiMTZjMWFCZiIsIm1heFRpbWVvdXRTZWNvbmRzIjozMDAsImV4dHJhIjp7Im5hbWUiOiJVU0QgQ29pbiIsInZlcnNpb24iOiIyIn19XSwicmVzb3VyY2UiOnsidXJsIjoiaHR0cHM6Ly9ibG9ja3J1bi5haS9hcGkvdjEvY2hhdC9jb21wbGV0aW9ucyIsImRlc2NyaXB0aW9uIjoiR1BULTRvIE1pbmkgQVBJIGNhbGwgKH4xNyBpbnB1dCwgODE5MiBtYXggb3V0cHV0IHRva2VucykiLCJtaW1lVHlwZSI6ImFwcGxpY2F0aW9uL2pzb24ifX0=";

    fn signer() -> Arc<dyn Signer> {
        Arc::new(covenant_x402::MockSigner)
    }

    #[tokio::test]
    async fn pays_402_then_captures_receipt() {
        let server = MockServer::start().await;
        // First hit: 402 with the challenge header.
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(402).insert_header("x-payment-required", LIVE_B64))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // Paid retry (carries x-payment): 200 with served model + settlement tx.
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("x-payment", "mock:eip155:8453:3000"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-payment-receipt", "0xdeadbeef")
                    .insert_header("x-clawrouter-model", "gpt-4o-mini")
                    .set_body_json(json!({"model": "gpt-4o-mini", "choices": [{"text": "hi"}]})),
            )
            .mount(&server)
            .await;

        let client = BlockRunClient::new(server.uri(), signer());
        let out = client
            .call(
                "/v1/chat/completions",
                json!({"model": "gpt-4o-mini", "messages": []}),
            )
            .await
            .unwrap();

        assert_eq!(out.receipt.verdict, "delivered");
        assert_eq!(out.receipt.model_served, "gpt-4o-mini");
        assert_eq!(out.receipt.payment.tx.as_deref(), Some("0xdeadbeef"));
        assert_eq!(out.receipt.payment.amount, "3000");
        assert!((out.receipt.payment.amount_usdc - 0.003).abs() < 1e-9);
        assert!(!out.receipt.digest().is_empty());
    }

    #[tokio::test]
    async fn free_endpoint_still_yields_a_receipt() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/free"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"model": "x", "ok": true})),
            )
            .mount(&server)
            .await;
        let client = BlockRunClient::new(server.uri(), signer());
        let out = client
            .call("/v1/free", json!({"model": "x"}))
            .await
            .unwrap();
        assert_eq!(out.receipt.payment.amount, "0");
        assert_eq!(out.receipt.model_served, "x");
    }

    #[tokio::test]
    async fn surfaces_api_error_after_payment() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(402).insert_header("x-payment-required", LIVE_B64))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(500).set_body_json(json!({"error": "upstream"})))
            .mount(&server)
            .await;
        let client = BlockRunClient::new(server.uri(), signer());
        let err = client
            .call("/v1/chat/completions", json!({"model": "gpt-4o-mini"}))
            .await
            .unwrap_err();
        assert!(matches!(err, BlockRunError::Api { status: 500, .. }));
    }
}
