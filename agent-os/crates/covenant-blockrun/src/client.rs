//! The paid round-trip: hit a BlockRun endpoint, satisfy the 402, and capture
//! the whole exchange as a [`CallReceipt`]. The signing itself is delegated to a
//! `covenant_x402::Signer`, so this crate never holds a key.

use std::sync::Arc;

use base64::Engine;
use covenant_x402::Signer;
use reqwest::StatusCode;
use serde_json::Value;

use crate::challenge::{Accept, Challenge};
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
    max_atomic: u64,
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
            max_atomic: crate::config::DEFAULT_MAX_ATOMIC,
        }
    }

    /// Cap the atomic amount this client will sign for. `0` keeps the default.
    pub fn with_max_atomic(mut self, max_atomic: u64) -> Self {
        if max_atomic != 0 {
            self.max_atomic = max_atomic;
        }
        self
    }

    /// POST `body` to `endpoint`, paying the x402 challenge if one is returned,
    /// and return the response bound into a receipt.
    pub async fn call(&self, endpoint: &str, body: Value) -> Result<PaidCall> {
        let url = format!("{}{}", self.base_url.trim_end_matches('/'), endpoint);
        let first = self.http.post(&url).json(&body).send().await?;

        if first.status() == StatusCode::PAYMENT_REQUIRED {
            return self.pay_and_capture(endpoint, &url, body, first).await;
        }
        // Not a 402: a free call (2xx) or an error. Emit a receipt only for a
        // real success; a 4xx/5xx with a JSON error body must surface as an
        // error, not a $0 "delivered" receipt.
        let status = first.status();
        let routing = RoutingClaim::from_headers(first.headers());
        let response = read_json(first).await?;
        if !status.is_success() {
            return Err(BlockRunError::Api {
                status: status.as_u16(),
                message: response.to_string(),
            });
        }
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
        // BlockRun mainnet returns a single Base USDC option today, so taking the
        // first is correct. If it ever offers multiple chains, this needs to pin
        // the network the configured signer can actually sign for; see `pick`.
        let accept = challenge
            .pick(None)
            .ok_or_else(|| BlockRunError::Challenge("no payment options offered".into()))?
            .clone();

        // Refuse an implausible charge before signing. The signer would authorize
        // whatever value the 402 names, so a compromised or buggy challenge is
        // capped here, not at the funding key.
        let atomic: u64 = accept.amount.parse().map_err(|_| {
            BlockRunError::Challenge(format!("non-numeric amount {:?}", accept.amount))
        })?;
        if atomic > self.max_atomic {
            return Err(BlockRunError::Challenge(format!(
                "402 amount {atomic} exceeds the {} atomic cap",
                self.max_atomic
            )));
        }

        let signed = self
            .signer
            .build_payment(&accept.to_requirements())
            .await
            .map_err(|e| BlockRunError::Signer(e.to_string()))?;
        // The signer emits a bare v2 envelope ({x402Version, scheme, network,
        // payload}); the CDP facilitator's x402V2PaymentPayload instead wants the
        // full `accepted` requirement echoed back and no top-level scheme/network.
        // Rewrap before sending; a header that is not a base64 JSON envelope
        // (e.g. a mock) passes through unchanged.
        let header = to_cdp_v2_header(&signed, &accept);

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

/// Rewrap the signer's base64 v2 envelope into the shape the CDP facilitator
/// verifies: `{ x402Version: 2, accepted: <the chosen requirement>, payload }`,
/// carrying the signer's `payload` verbatim and dropping the top-level
/// scheme/network the CDP schema does not accept. A header that is not a base64
/// JSON envelope carrying a `payload` (e.g. a mock signer) is returned unchanged.
fn to_cdp_v2_header(signed: &str, accept: &Accept) -> String {
    let engine = base64::engine::general_purpose::STANDARD;
    let Ok(bytes) = engine.decode(signed.trim()) else {
        return signed.to_string();
    };
    let Ok(env) = serde_json::from_slice::<Value>(&bytes) else {
        return signed.to_string();
    };
    let Some(payload) = env.get("payload").cloned() else {
        return signed.to_string();
    };
    let v2 = serde_json::json!({
        "x402Version": 2,
        "accepted": accept.to_accepted_json(),
        "payload": payload,
    });
    engine.encode(v2.to_string().as_bytes())
}

/// A remote that answers a paid request controls its response, so an unbounded
/// read lets a hostile or broken one exhaust a worker's memory. The 120s timeout
/// bounds latency, not size; this bounds size. Mirrors `covenant_x402`'s cap.
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

async fn read_json(resp: reqwest::Response) -> Result<Value> {
    let status = resp.status();
    let bytes = read_capped(resp, MAX_RESPONSE_BYTES).await?;
    serde_json::from_slice(&bytes).map_err(|_| BlockRunError::Api {
        status: status.as_u16(),
        message: if bytes.is_empty() {
            "empty response".into()
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        },
    })
}

/// Read the body chunk by chunk, refusing once it exceeds `cap`. A truthful
/// `Content-Length` lets us reject up front; a lying or absent one is caught by
/// the running total.
async fn read_capped(mut resp: reqwest::Response, cap: usize) -> Result<Vec<u8>> {
    if let Some(len) = resp.content_length() {
        if len as usize > cap {
            return Err(BlockRunError::Api {
                status: resp.status().as_u16(),
                message: format!("response body {len} bytes exceeds the {cap}-byte cap"),
            });
        }
    }
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if buf.len() + chunk.len() > cap {
            return Err(BlockRunError::Api {
                status: resp.status().as_u16(),
                message: format!("response body exceeds the {cap}-byte cap"),
            });
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
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
    async fn surfaces_error_on_non_402_first_response() {
        // A 500 with a JSON body must be an error, not a $0 "delivered" receipt.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(500)
                    .set_body_json(json!({"error": "upstream", "model": "gpt-4o-mini"})),
            )
            .mount(&server)
            .await;
        let client = BlockRunClient::new(server.uri(), signer());
        let err = client
            .call("/v1/chat/completions", json!({"model": "gpt-4o-mini"}))
            .await
            .unwrap_err();
        assert!(matches!(err, BlockRunError::Api { status: 500, .. }));
    }

    #[tokio::test]
    async fn rejects_a_402_over_the_spend_cap() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(402).insert_header("x-payment-required", LIVE_B64))
            .mount(&server)
            .await;
        // Cap below the challenge's 3000 atomic → refuse before signing.
        let client = BlockRunClient::new(server.uri(), signer()).with_max_atomic(1000);
        let err = client
            .call("/v1/chat/completions", json!({"model": "gpt-4o-mini"}))
            .await
            .unwrap_err();
        assert!(matches!(err, BlockRunError::Challenge(m) if m.contains("cap")));
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
