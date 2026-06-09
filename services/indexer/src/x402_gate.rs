//! Server-side x402 gate for the paid indexer summary endpoint.
//!
//! On no `x-payment` header: returns 402 with a mainline x402 v1
//! challenge body (PayAI-compatible). On `x-payment` present: decodes
//! the base64 envelope, posts it to PayAI `/verify` then `/settle`,
//! and on success forwards to the underlying handler with the settled
//! signature attached as the `x-payment-response` header.
//!
//! The wire shapes are the same ones `covenant-hyre`'s diagnostic
//! facilitator client validates against live PayAI: object body with
//! `accepts[]`, `maxAmountRequired`, `extra.feePayer`. If a paid
//! endpoint lands elsewhere in covenant, lift this module into a
//! `covenant-x402-server` crate.

use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Json, Response},
};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, warn};

pub const X402_VERSION_V1: u32 = 1;
pub const SCHEME_EXACT: &str = "exact";
pub const PAYAI_FACILITATOR: &str = "https://facilitator.payai.network";
pub const PAYAI_FEE_PAYER: &str = "2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4";
pub const SOLANA_NETWORK: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";
pub const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

#[derive(Debug, Clone)]
pub struct X402Config {
    pub pay_to: String,
    pub fee_payer: String,
    pub facilitator: String,
    pub network: String,
    pub asset: String,
    pub price_atomic: String,
    pub resource_base_url: String,
    pub description: String,
}

impl X402Config {
    /// Read config from env. Returns `None` when `COVENANT_X402_PAY_TO`
    /// is missing — the operator must set a payTo before the paid
    /// route is registered.
    pub fn from_env() -> Option<Self> {
        let pay_to = std::env::var("COVENANT_X402_PAY_TO").ok()?;
        Some(Self {
            pay_to,
            fee_payer: env_or("COVENANT_X402_FEE_PAYER", PAYAI_FEE_PAYER),
            facilitator: env_or("COVENANT_X402_FACILITATOR", PAYAI_FACILITATOR),
            network: env_or("COVENANT_X402_NETWORK", SOLANA_NETWORK),
            asset: env_or("COVENANT_X402_ASSET", USDC_MINT),
            price_atomic: env_or("COVENANT_X402_PRICE_ATOMIC", "50000"),
            resource_base_url: env_or("COVENANT_X402_RESOURCE_BASE_URL", "http://localhost:8080"),
            description: env_or(
                "COVENANT_X402_DESCRIPTION",
                "Covenant indexer stats summary.",
            ),
        })
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[derive(Clone)]
pub struct X402Gate {
    pub config: Arc<X402Config>,
    pub http: reqwest::Client,
}

impl X402Gate {
    pub fn new(config: X402Config) -> Self {
        Self {
            config: Arc::new(config),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    pub fn challenge_for(&self, resource_path: &str) -> ChallengeBody {
        let cfg = &self.config;
        let resource = format!(
            "{}{}",
            cfg.resource_base_url.trim_end_matches('/'),
            resource_path
        );
        ChallengeBody {
            x402_version: X402_VERSION_V1,
            error: "X-PAYMENT header is required".into(),
            accepts: vec![ChallengeAccept {
                scheme: SCHEME_EXACT.into(),
                network: cfg.network.clone(),
                max_amount_required: cfg.price_atomic.clone(),
                resource,
                description: cfg.description.clone(),
                mime_type: "application/json".into(),
                output_schema: serde_json::json!({}),
                pay_to: cfg.pay_to.clone(),
                max_timeout_seconds: 60,
                asset: cfg.asset.clone(),
                extra: ChallengeExtra {
                    fee_payer: cfg.fee_payer.clone(),
                },
            }],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChallengeBody {
    #[serde(rename = "x402Version")]
    pub x402_version: u32,
    pub error: String,
    pub accepts: Vec<ChallengeAccept>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChallengeAccept {
    pub scheme: String,
    pub network: String,
    #[serde(rename = "maxAmountRequired")]
    pub max_amount_required: String,
    pub resource: String,
    pub description: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    #[serde(rename = "outputSchema")]
    pub output_schema: serde_json::Value,
    #[serde(rename = "payTo")]
    pub pay_to: String,
    #[serde(rename = "maxTimeoutSeconds")]
    pub max_timeout_seconds: u32,
    pub asset: String,
    pub extra: ChallengeExtra,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChallengeExtra {
    #[serde(rename = "feePayer")]
    pub fee_payer: String,
}

#[derive(Debug, Clone, Serialize)]
struct FacilitatorRequest<'a> {
    #[serde(rename = "x402Version")]
    x402_version: u32,
    #[serde(rename = "paymentPayload")]
    payment_payload: serde_json::Value,
    #[serde(rename = "paymentRequirements")]
    payment_requirements: &'a ChallengeAccept,
}

#[derive(Debug, Clone, Deserialize)]
struct VerifyResponse {
    #[serde(rename = "isValid")]
    is_valid: bool,
    #[serde(rename = "invalidReason", default)]
    invalid_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SettleResponse {
    success: bool,
    #[serde(rename = "errorReason", default)]
    error_reason: Option<String>,
    #[serde(default)]
    transaction: String,
}

/// Outcome of the gate. Caller's responsibility to use [`Settled`] to
/// run the underlying handler and attach the signature on response.
pub enum Gated {
    Challenge(ChallengeBody),
    Settled { tx_signature: String },
}

impl X402Gate {
    /// Inspect the request headers, run verify+settle through the
    /// facilitator, return either the challenge to send back or a
    /// settled signature the handler can attach.
    pub async fn gate(&self, headers: &HeaderMap, resource_path: &str) -> Gated {
        let challenge = self.challenge_for(resource_path);

        let Some(payment_b64) = headers
            .get("x-payment")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
        else {
            return Gated::Challenge(challenge);
        };

        let payload_json = match B64
            .decode(payment_b64.as_bytes())
            .map_err(|e| format!("base64: {e}"))
            .and_then(|bytes| {
                serde_json::from_slice::<serde_json::Value>(&bytes)
                    .map_err(|e| format!("json: {e}"))
            }) {
            Ok(v) => v,
            Err(e) => {
                warn!("x402 payload decode: {e}");
                return Gated::Challenge(challenge);
            }
        };

        let accept = &challenge.accepts[0];
        let fac_req = FacilitatorRequest {
            x402_version: X402_VERSION_V1,
            payment_payload: payload_json,
            payment_requirements: accept,
        };

        match self.call_verify_then_settle(&fac_req).await {
            Ok(sig) => Gated::Settled { tx_signature: sig },
            Err(reason) => {
                warn!("x402 facilitator rejected: {reason}");
                Gated::Challenge(challenge)
            }
        }
    }

    async fn call_verify_then_settle(
        &self,
        req: &FacilitatorRequest<'_>,
    ) -> Result<String, String> {
        let verify_url = format!("{}/verify", self.config.facilitator);
        let verify: VerifyResponse = self.post_json(&verify_url, req).await?;
        if !verify.is_valid {
            return Err(format!(
                "verify rejected: {}",
                verify.invalid_reason.unwrap_or_default()
            ));
        }

        let settle_url = format!("{}/settle", self.config.facilitator);
        let settle: SettleResponse = self.post_json(&settle_url, req).await?;
        if !settle.success {
            return Err(format!(
                "settle failed: {}",
                settle.error_reason.unwrap_or_default()
            ));
        }
        debug!(sig = %settle.transaction, "x402 settled");
        Ok(settle.transaction)
    }

    async fn post_json<R: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        req: &FacilitatorRequest<'_>,
    ) -> Result<R, String> {
        let resp = self
            .http
            .post(url)
            .json(req)
            .send()
            .await
            .map_err(|e| format!("post {url}: {e}"))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("read body {url}: {e}"))?;
        if !status.is_success() {
            return Err(format!(
                "facilitator {url}: HTTP {status} — {}",
                String::from_utf8_lossy(&bytes)
            ));
        }
        serde_json::from_slice(&bytes).map_err(|e| {
            format!(
                "decode {url}: {e} — body: {}",
                String::from_utf8_lossy(&bytes)
            )
        })
    }
}

/// Handler for `GET /x402/stats/summary`. Runs the gate, then returns
/// the same JSON shape as `/stats/summary` on success.
pub async fn paid_summary(
    State(state): State<crate::api::AppState>,
    headers: HeaderMap,
) -> Response {
    let Some(gate) = state.x402.as_ref() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "x402 not configured").into_response();
    };
    let resource_path = "/x402/stats/summary";
    match gate.gate(&headers, resource_path).await {
        Gated::Challenge(c) => (StatusCode::PAYMENT_REQUIRED, Json(c)).into_response(),
        Gated::Settled { tx_signature } => {
            let snapshot = crate::api::summary_snapshot(&state);
            let mut resp = (StatusCode::OK, Json(snapshot)).into_response();
            if let Ok(v) = HeaderValue::from_str(&tx_signature) {
                resp.headers_mut().insert("x-payment-response", v);
            }
            resp
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderName;

    fn cfg() -> X402Config {
        X402Config {
            pay_to: "9covntsTreasuryAddressForTestingPurposesOnly".into(),
            fee_payer: PAYAI_FEE_PAYER.into(),
            facilitator: "http://test-facilitator".into(),
            network: SOLANA_NETWORK.into(),
            asset: USDC_MINT.into(),
            price_atomic: "50000".into(),
            resource_base_url: "https://covenant-indexer.example".into(),
            description: "Test endpoint.".into(),
        }
    }

    #[test]
    fn challenge_has_mainline_v1_field_names() {
        let gate = X402Gate::new(cfg());
        let c = gate.challenge_for("/x402/stats/summary");
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["x402Version"], 1);
        let opt = &json["accepts"][0];
        assert_eq!(opt["scheme"], "exact");
        assert_eq!(opt["maxAmountRequired"], "50000");
        assert_eq!(opt["payTo"], "9covntsTreasuryAddressForTestingPurposesOnly");
        assert_eq!(opt["asset"], USDC_MINT);
        assert_eq!(opt["network"], SOLANA_NETWORK);
        assert_eq!(opt["mimeType"], "application/json");
        assert_eq!(opt["maxTimeoutSeconds"], 60);
        assert_eq!(opt["extra"]["feePayer"], PAYAI_FEE_PAYER);
        assert_eq!(
            opt["resource"],
            "https://covenant-indexer.example/x402/stats/summary"
        );
        assert!(
            opt.get("amount").is_none(),
            "must use v1 maxAmountRequired, not xona-style amount"
        );
    }

    #[tokio::test]
    async fn gate_returns_challenge_when_no_payment_header() {
        let gate = X402Gate::new(cfg());
        let headers = HeaderMap::new();
        match gate.gate(&headers, "/x402/stats/summary").await {
            Gated::Challenge(c) => {
                assert_eq!(c.x402_version, 1);
                assert_eq!(c.accepts[0].max_amount_required, "50000");
            }
            Gated::Settled { .. } => panic!("expected challenge"),
        }
    }

    #[tokio::test]
    async fn gate_returns_challenge_on_malformed_payment_header() {
        let gate = X402Gate::new(cfg());
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-payment"),
            HeaderValue::from_static("!!!not-valid-base64!!!"),
        );
        assert!(matches!(
            gate.gate(&headers, "/x402/stats/summary").await,
            Gated::Challenge(_)
        ));
    }

    #[tokio::test]
    async fn gate_settles_when_facilitator_verifies_and_settles() {
        // Drives the real PayAI request shape through a mock server: a single
        // FacilitatorRequest envelope hits /verify, then /settle, and the
        // gate surfaces the settle signature.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/verify"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "isValid": true,
                "payer": "PAYER1111111111111111111111111111111111111"
            })))
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/settle"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "transaction": "5sigSettleSignature1111111111111111111111111111111",
                "network": "solana"
            })))
            .mount(&server)
            .await;

        let mut c = cfg();
        c.facilitator = server.uri();
        let gate = X402Gate::new(c);

        let payload = serde_json::json!({
            "x402Version": 1,
            "scheme": "exact",
            "network": SOLANA_NETWORK,
            "payload": { "transaction": "FAKE_BASE64_TX" }
        });
        let b64 = B64.encode(serde_json::to_vec(&payload).unwrap());
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-payment"),
            HeaderValue::from_str(&b64).unwrap(),
        );

        match gate.gate(&headers, "/x402/stats/summary").await {
            Gated::Settled { tx_signature } => {
                assert!(tx_signature.starts_with("5sig"));
            }
            Gated::Challenge(_) => panic!("expected settle"),
        }
    }

    #[tokio::test]
    async fn gate_returns_challenge_when_facilitator_rejects_verify() {
        // The facilitator answers HTTP 200 with isValid=false on rejection,
        // not 4xx. The gate must read the JSON body, not status, and surface
        // a fresh challenge so the caller can retry instead of leaking a
        // 5xx that would look like our service is broken.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/verify"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "isValid": false,
                "invalidReason": "invalid_exact_svm_payload_transaction_could_not_be_decoded"
            })))
            .mount(&server)
            .await;

        let mut c = cfg();
        c.facilitator = server.uri();
        let gate = X402Gate::new(c);

        let payload = serde_json::json!({"x402Version":1,"scheme":"exact","network":SOLANA_NETWORK,"payload":{"transaction":"X"}});
        let b64 = B64.encode(serde_json::to_vec(&payload).unwrap());
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-payment"),
            HeaderValue::from_str(&b64).unwrap(),
        );
        assert!(matches!(
            gate.gate(&headers, "/x402/stats/summary").await,
            Gated::Challenge(_)
        ));
    }

    #[tokio::test]
    async fn gate_returns_challenge_when_settle_fails_after_verify_ok() {
        // verify can pass while settle later fails (blockhash expired, RPC
        // hiccup). The gate must surface the challenge so the caller retries
        // with a fresh payment, never let the request through unsettled.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/verify"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"isValid": true})),
            )
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/settle"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": false,
                "errorReason": "blockhash_not_found",
                "transaction": "",
                "network": "solana"
            })))
            .mount(&server)
            .await;

        let mut c = cfg();
        c.facilitator = server.uri();
        let gate = X402Gate::new(c);

        let payload = serde_json::json!({"x402Version":1,"scheme":"exact","network":SOLANA_NETWORK,"payload":{"transaction":"X"}});
        let b64 = B64.encode(serde_json::to_vec(&payload).unwrap());
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-payment"),
            HeaderValue::from_str(&b64).unwrap(),
        );
        assert!(matches!(
            gate.gate(&headers, "/x402/stats/summary").await,
            Gated::Challenge(_)
        ));
    }
}
