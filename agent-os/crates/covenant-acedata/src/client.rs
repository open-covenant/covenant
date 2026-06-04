//! Thin Bearer-token client over the AceData REST API.
//!
//! One method — [`AceDataClient::post`] — sends a JSON body to a path
//! and returns the parsed JSON, mapping AceData's `{success:false,
//! error:{code,message}}` envelope onto [`AceDataError::Api`]. The
//! generation endpoints (Flux, Suno) answer synchronously with the
//! finished asset under `data`, so there is no polling loop to own.

use serde_json::Value;

use crate::{AceDataError, Result};

/// HTTP client carrying the API host and the Bearer token. The token is
/// a billing credential, not a signing key; it is held here and never
/// serialized.
#[derive(Clone)]
pub struct AceDataClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl AceDataClient {
    /// New client against `base_url` authenticating with `api_key`.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self::with_client(reqwest::Client::new(), base_url, api_key)
    }

    /// New client over a caller-supplied [`reqwest::Client`] (for custom
    /// timeouts or a proxy).
    pub fn with_client(
        http: reqwest::Client,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
        }
    }

    /// POST `body` to `path` (e.g. `/serp/google`) and return the parsed
    /// JSON response.
    ///
    /// A transport failure maps to [`AceDataError::Http`]; an AceData
    /// `{success:false, error}` envelope maps to [`AceDataError::Api`].
    /// Endpoints that answer without the envelope (e.g. search) pass
    /// their body straight through.
    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let url = format!("{}/{}", self.base_url, path.trim_start_matches('/'));
        let resp = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
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
