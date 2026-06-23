//! Client over the AceData REST API, with two billing modes.
//!
//! [`AceDataClient::post`] sends a JSON body to a path and returns the
//! parsed JSON, mapping AceData's `{success:false, error:{code,message}}`
//! envelope onto [`AceDataError::Api`]. The generation endpoints (Flux,
//! Suno) answer synchronously with the finished asset under `data`, so
//! there is no polling loop to own.
//!
//! Two auth modes, chosen at construction:
//! - [`AceDataClient::new`] — **Bearer** billing with an API key (a credit
//!   credential, held here and never serialized).
//! - [`AceDataClient::with_x402`] — **keyless** crypto pay-per-call: no API
//!   key, each call walks AceData's 402 challenge and pays USDC on Solana
//!   through the funding-key signer (see [`crate::x402`]).
//!
//! The tools call `post` identically either way; the mode is transparent.

use std::sync::Arc;

use serde_json::Value;

use crate::x402::X402Payer;
use crate::{AceDataError, Result};

#[derive(Clone)]
enum Auth {
    /// Bearer billing credential (API key).
    Bearer(String),
    /// Keyless x402 pay-per-call over the funding-key signer.
    X402(Arc<X402Payer>),
}

/// HTTP client carrying the API host and the chosen billing mode.
#[derive(Clone)]
pub struct AceDataClient {
    http: reqwest::Client,
    base_url: String,
    auth: Auth,
}

impl AceDataClient {
    /// New Bearer-billed client against `base_url` authenticating with `api_key`.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self::with_client(reqwest::Client::new(), base_url, api_key)
    }

    /// Bearer client over a caller-supplied [`reqwest::Client`] (custom
    /// timeouts or a proxy).
    pub fn with_client(
        http: reqwest::Client,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            auth: Auth::Bearer(api_key.into()),
        }
    }

    /// New **keyless** client that pays per call via x402 USDC on Solana,
    /// using `payer` (a funding-key signer + per-call cap). No API key.
    pub fn with_x402(base_url: impl Into<String>, payer: X402Payer) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            auth: Auth::X402(Arc::new(payer)),
        }
    }

    /// True when this client pays per call over x402 (no API key).
    pub fn is_x402(&self) -> bool {
        matches!(self.auth, Auth::X402(_))
    }

    /// POST `body` to `path` (e.g. `/serp/google`) and return the parsed
    /// JSON response. In Bearer mode the API key authenticates; in x402
    /// mode the call walks the 402 challenge and pays USDC.
    ///
    /// A transport failure maps to [`AceDataError::Http`]; an AceData
    /// `{success:false, error}` envelope maps to [`AceDataError::Api`].
    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let url = format!("{}/{}", self.base_url, path.trim_start_matches('/'));
        match &self.auth {
            Auth::X402(payer) => payer.pay_and_post(&self.http, &url, body).await,
            Auth::Bearer(key) => {
                let resp = self
                    .http
                    .post(url)
                    .bearer_auth(key)
                    .json(&body)
                    .send()
                    .await?;
                let value: Value = resp.json().await?;
                if value.get("success").and_then(Value::as_bool) == Some(false) {
                    let err = value.get("error");
                    let code = err
                        .and_then(|e| e.get("code"))
                        .and_then(Value::as_str)
                        .unwrap_or("error")
                        .to_string();
                    let message = err
                        .and_then(|e| e.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                        .to_string();
                    return Err(AceDataError::Api { code, message });
                }
                Ok(value)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn post_sends_bearer_and_returns_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/serp/google"))
            .and(header("authorization", "Bearer secret-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "organic": [] })))
            .mount(&server)
            .await;

        let client = AceDataClient::new(server.uri(), "secret-key");
        let out = client
            .post("/serp/google", json!({ "query": "q" }))
            .await
            .unwrap();
        assert!(out.get("organic").is_some());
    }

    #[tokio::test]
    async fn post_maps_error_envelope_to_api_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/flux/images"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": false,
                "error": { "code": "bad_request", "message": "size is required" }
            })))
            .mount(&server)
            .await;

        let client = AceDataClient::new(server.uri(), "k");
        let err = client
            .post("/flux/images", json!({ "prompt": "x" }))
            .await
            .unwrap_err();
        match err {
            AceDataError::Api { code, message } => {
                assert_eq!(code, "bad_request");
                assert_eq!(message, "size is required");
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }
}
