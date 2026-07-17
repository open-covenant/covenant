//! REST client for Invoica's invoice API.
//!
//! Holds the API key and injects it as `Authorization: Bearer` on every
//! request; the daemon constructs the client, so the key never reaches the
//! agent. Reads (get/list) retry a cold-started gateway; the create write is
//! single-shot, since a created invoice is not idempotent. Responses are
//! returned as raw JSON, because Invoica's invoice shape differs between its
//! published SDK and its live backend.

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::types::CreateInvoiceRequest;
use crate::{InvoicaError, Result};

#[derive(Clone)]
pub struct InvoicaClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl InvoicaClient {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .build()
            .expect("build invoica http client");
        // A key or host sourced from an env file or secret often carries a
        // trailing newline; left in, the key makes an invalid `Authorization`
        // header that fails every request at send time. Trim at the boundary.
        Self {
            http,
            base_url: base_url.into().trim().trim_end_matches('/').to_string(),
            api_key: api_key.into().trim().to_string(),
        }
    }

    /// `POST /v1/invoices`. Not retried: a created invoice is not idempotent.
    pub async fn create_invoice(&self, req: &CreateInvoiceRequest) -> Result<Value> {
        self.post("/v1/invoices", req).await
    }

    /// `GET /v1/invoices/:id`.
    pub async fn get_invoice(&self, id: &str) -> Result<Value> {
        self.get(&format!("/v1/invoices/{}", urlencode(id))).await
    }

    /// `GET /v1/invoices` with optional filters. Returns the `{invoices, total,
    /// page}` blob verbatim.
    pub async fn list_invoices(&self, params: &[(String, String)]) -> Result<Value> {
        let mut path = String::from("/v1/invoices");
        if !params.is_empty() {
            let q: Vec<String> = params
                .iter()
                .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
                .collect();
            path.push('?');
            path.push_str(&q.join("&"));
        }
        self.get(&path).await
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.http.get(&url).bearer_auth(&self.api_key).send().await {
                Ok(resp) => {
                    if attempt < MAX_ATTEMPTS && is_retryable_status(resp.status()) {
                        let delay = retry_after(&resp).unwrap_or_else(|| backoff(attempt));
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return read(resp, path).await;
                }
                Err(e) if attempt < MAX_ATTEMPTS && is_transient(&e) => {
                    tokio::time::sleep(backoff(attempt)).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    async fn post(&self, path: &str, body: &impl Serialize) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .await?;
        read(resp, path).await
    }
}

async fn read(resp: reqwest::Response, ctx: &str) -> Result<Value> {
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        let (message, code) = parse_error(&body, status);
        return Err(InvoicaError::Api {
            status: status.as_u16(),
            code,
            message,
        });
    }
    if body.trim().is_empty() {
        return Err(InvoicaError::Decode(format!("{ctx}: empty response body")));
    }
    let value: Value = serde_json::from_str(&body)
        .map_err(|e| InvoicaError::Decode(format!("{ctx}: {e}; body: {}", truncate(&body))))?;
    if value.is_null() {
        return Err(InvoicaError::Decode(format!(
            "{ctx}: response body was null"
        )));
    }
    Ok(value)
}

/// Invoica reports failures two ways: the invoice routes return a flat
/// `{ error: "..." }`, the tax and settlement routes a nested
/// `{ error: { message, code } }`. Pull message and code from whichever shape
/// is present.
fn parse_error(body: &str, status: reqwest::StatusCode) -> (String, Option<String>) {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let message = v
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| v.pointer("/error/message").and_then(Value::as_str))
        .map(String::from)
        .unwrap_or_else(|| {
            if body.is_empty() {
                status.canonical_reason().unwrap_or("request failed").into()
            } else {
                truncate(body)
            }
        });
    let code = v
        .get("code")
        .and_then(Value::as_str)
        .or_else(|| v.pointer("/error/code").and_then(Value::as_str))
        .map(String::from);
    (message, code)
}

/// Total GET attempts before giving up.
const MAX_ATTEMPTS: u32 = 3;

/// Cap on an honored `Retry-After`, so a hostile or fat-fingered value can't
/// stall a call up against the request timeout.
const RETRY_AFTER_CAP_SECS: u64 = 5;

/// Backoff before the next GET attempt: 400ms, then 800ms.
fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(400 * u64::from(attempt))
}

/// Statuses worth a retry: a cold-starting gateway (502/503/504) or a brief
/// rate-limit (429). A 429 or 503 may carry `Retry-After`, which [`retry_after`]
/// honors in place of the default backoff.
fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 502 | 503 | 504 | 429)
}

/// The `Retry-After` delay a response asks for, in delta-seconds, capped by
/// [`RETRY_AFTER_CAP_SECS`]. The HTTP-date form is not read; Invoica's gateway
/// sends seconds.
fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    let secs: u64 = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(secs.min(RETRY_AFTER_CAP_SECS)))
}

/// A connect failure or timeout against a parked instance; retryable.
fn is_transient(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect()
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn truncate(s: &str) -> String {
    let cut: String = s.chars().take(200).collect();
    if cut.chars().count() < s.chars().count() {
        format!("{cut}...")
    } else {
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{bearer_token, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn req() -> CreateInvoiceRequest {
        CreateInvoiceRequest {
            amount: 100.0,
            customer_email: "buyer@acme.test".into(),
            customer_name: "Acme Inc".into(),
            currency: Some("USD".into()),
            chain: Some("solana".into()),
            buyer_country_code: None,
            buyer_state_code: None,
            company_id: None,
        }
    }

    #[tokio::test]
    async fn create_sends_bearer_and_returns_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/invoices"))
            .and(bearer_token("secret-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "inv_1", "invoiceNumber": 1001, "status": "PENDING", "amount": 100.0, "currency": "USD"
            })))
            .mount(&server)
            .await;
        let v = InvoicaClient::new(server.uri(), "secret-key")
            .create_invoice(&req())
            .await
            .unwrap();
        assert_eq!(v["id"], "inv_1");
        assert_eq!(v["invoiceNumber"], 1001);
    }

    #[tokio::test]
    async fn get_retries_cold_start_502_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/invoices/inv_1"))
            .respond_with(ResponseTemplate::new(502))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/invoices/inv_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "inv_1" })))
            .mount(&server)
            .await;
        let v = InvoicaClient::new(server.uri(), "k")
            .get_invoice("inv_1")
            .await
            .unwrap();
        assert_eq!(v["id"], "inv_1");
    }

    #[tokio::test]
    async fn create_is_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/invoices"))
            .respond_with(ResponseTemplate::new(502))
            .expect(1)
            .mount(&server)
            .await;
        let err = InvoicaClient::new(server.uri(), "k")
            .create_invoice(&req())
            .await
            .unwrap_err();
        assert!(matches!(err, InvoicaError::Api { status: 502, .. }));
    }

    #[tokio::test]
    async fn flat_error_surfaces_message_and_code() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/invoices/bad"))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_json(json!({ "error": "invoice not found", "code": "NOT_FOUND" })),
            )
            .mount(&server)
            .await;
        let err = InvoicaClient::new(server.uri(), "k")
            .get_invoice("bad")
            .await
            .unwrap_err();
        match err {
            InvoicaError::Api {
                status,
                code,
                message,
            } => {
                assert_eq!(status, 404);
                assert_eq!(code.as_deref(), Some("NOT_FOUND"));
                assert_eq!(message, "invoice not found");
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn nested_error_shape_is_parsed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/invoices/x"))
            .respond_with(ResponseTemplate::new(400).set_body_json(
                json!({ "success": false, "error": { "message": "bad request", "code": "VALIDATION" } }),
            ))
            .mount(&server)
            .await;
        let err = InvoicaClient::new(server.uri(), "k")
            .get_invoice("x")
            .await
            .unwrap_err();
        match err {
            InvoicaError::Api { code, message, .. } => {
                assert_eq!(message, "bad request");
                assert_eq!(code.as_deref(), Some("VALIDATION"));
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_success_body_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/invoices/inv_1"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let err = InvoicaClient::new(server.uri(), "k")
            .get_invoice("inv_1")
            .await
            .unwrap_err();
        assert!(matches!(err, InvoicaError::Decode(_)));
    }

    #[tokio::test]
    async fn null_success_body_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/invoices/inv_1"))
            .respond_with(ResponseTemplate::new(200).set_body_string("null"))
            .mount(&server)
            .await;
        let err = InvoicaClient::new(server.uri(), "k")
            .get_invoice("inv_1")
            .await
            .unwrap_err();
        assert!(matches!(err, InvoicaError::Decode(_)));
    }

    #[tokio::test]
    async fn get_retries_429_honoring_retry_after() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/invoices/inv_1"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/invoices/inv_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "inv_1" })))
            .mount(&server)
            .await;
        let v = InvoicaClient::new(server.uri(), "k")
            .get_invoice("inv_1")
            .await
            .unwrap();
        assert_eq!(v["id"], "inv_1");
    }

    #[tokio::test]
    async fn new_trims_whitespace_in_key() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/invoices/inv_1"))
            .and(bearer_token("secret-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "inv_1" })))
            .mount(&server)
            .await;
        let v = InvoicaClient::new(server.uri(), "secret-key\n")
            .get_invoice("inv_1")
            .await
            .unwrap();
        assert_eq!(v["id"], "inv_1");
    }

    #[test]
    fn api_error_display_includes_code_when_present() {
        let with = InvoicaError::Api {
            status: 404,
            code: Some("NOT_FOUND".into()),
            message: "invoice not found".into(),
        };
        assert_eq!(
            with.to_string(),
            "invoica api [404]: invoice not found [NOT_FOUND]"
        );
        let without = InvoicaError::Api {
            status: 500,
            code: None,
            message: "boom".into(),
        };
        assert_eq!(without.to_string(), "invoica api [500]: boom");
    }
}
