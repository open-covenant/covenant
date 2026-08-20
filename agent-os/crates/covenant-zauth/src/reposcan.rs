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

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use covenant_x402::Signer;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;
use tracing::debug;

use crate::challenge;
use crate::{Result, ZauthError};

const DEFAULT_BASE_URL: &str = "https://api.zauth.inc";

/// Per-request timeout for the unpaid first POST. It only fetches the 402
/// challenge (or a cached gratis result) and never settles, so a hung or
/// slow-drip `api.zauth.inc` must fail fast rather than wedge the CLI — the
/// time-axis sibling of the body-size cap on the same client.
const FIRST_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Floor and ceiling for the paid retry's timeout. The challenge advertises
/// `maxTimeoutSeconds` as the settlement window the server commits to, so the
/// paid request must be willing to wait that long — a short fixed ceiling
/// would abort a legitimate settlement after funds have already committed.
/// The advertised value is untrusted: a hostile 402 could push it to 0
/// (instant abort) or to `u32::MAX` (~forever), so the derived timeout is
/// clamped to a sane band.
const PAID_TIMEOUT_MIN_SECS: u64 = 30;
const PAID_TIMEOUT_MAX_SECS: u64 = 600;

/// The reqwest client the CLI should drive [`RepoScanClient`] with — the
/// unpaid first request is bounded so an unresponsive registry cannot hang
/// the command. The paid retry overrides this per-request with a
/// settlement-aware timeout (see [`paid_timeout`]).
pub fn http_client() -> reqwest::Client {
    http_client_with_timeout(FIRST_REQUEST_TIMEOUT)
}

fn http_client_with_timeout(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Derive the paid retry's timeout from the challenge's advertised
/// `maxTimeoutSeconds`, clamped so an untrusted value cannot abort a real
/// settlement instantly or hang the CLI indefinitely.
fn paid_timeout(max_timeout_seconds: u32) -> Duration {
    Duration::from_secs(
        (max_timeout_seconds as u64).clamp(PAID_TIMEOUT_MIN_SECS, PAID_TIMEOUT_MAX_SECS),
    )
}

#[derive(Debug, Clone)]
pub struct RepoScanClient {
    http: reqwest::Client,
    base_url: String,
    max_bytes: usize,
}

impl RepoScanClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self::with_limits(http, DEFAULT_BASE_URL, crate::http::MAX_RESPONSE_BYTES)
    }

    pub fn with_base_url(http: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self::with_limits(http, base_url, crate::http::MAX_RESPONSE_BYTES)
    }

    fn with_limits(http: reqwest::Client, base_url: impl Into<String>, max_bytes: usize) -> Self {
        Self {
            http,
            base_url: base_url.into(),
            max_bytes,
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
            let body_text =
                crate::http::read_capped(first, self.max_bytes, ZauthError::ResponseTooLarge)
                    .await?;
            return Ok(RepoScanResult {
                status: status.as_u16(),
                body: body_text,
                paid_amount: None,
                error_detail: None,
            });
        }
        if status.as_u16() != 402 {
            return Err(ZauthError::UnexpectedStatus(status.as_u16()));
        }

        let headers = first.headers().clone();
        let _drained =
            crate::http::read_capped(first, self.max_bytes, ZauthError::ResponseTooLarge).await?;
        let parsed = challenge::decode_from_headers(&headers)?;
        let raw = challenge::decode_value_from_headers(&headers)?;

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
        let inner = signer
            .build_payment(&requirements)
            .await
            .map_err(|e| ZauthError::Sign(e.to_string()))?;

        // zauth verifies with the mainline x402 v2 matcher, which
        // deep-equals the requirement against the payload's `accepted`.
        // The sidecar signer returns a v1 envelope; lift its signed
        // transaction into the v2 shape and echo the chosen accept
        // verbatim so the match succeeds.
        let header_value = build_v2_payment(&inner, accept_value(&raw, accept))?;

        let paid = self
            .http
            .post(&url)
            .header("x-payment", header_value)
            .timeout(paid_timeout(accept.max_timeout_seconds))
            .json(&body)
            .send()
            .await?;
        let paid_status = paid.status();
        let paid_headers = paid.headers().clone();
        let paid_body =
            crate::http::read_capped(paid, self.max_bytes, ZauthError::ResponseTooLarge)
                .await
                .unwrap_or_default();
        let paid_amount = paid_status
            .is_success()
            .then(|| requirements.amount.clone());
        // A rejected retry carries the reason in the same header-encoded
        // challenge; surface it so a failure is diagnosable, not opaque.
        let error_detail = (!paid_status.is_success())
            .then(|| {
                challenge::decode_value_from_headers(&paid_headers)
                    .ok()
                    .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
            })
            .flatten();
        Ok(RepoScanResult {
            status: paid_status.as_u16(),
            body: paid_body,
            paid_amount,
            error_detail,
        })
    }
}

/// Pull the raw JSON of the accept option that matches the selected
/// (typed) accept, to echo verbatim in the v2 payload. Falls back to a
/// reconstructed object if the raw array can't be indexed.
fn accept_value(raw_challenge: &Value, accept: &challenge::Accept) -> Value {
    raw_challenge
        .get("accepts")
        .and_then(|a| a.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|v| {
                    v.get("network").and_then(|x| x.as_str()) == Some(accept.network.as_str())
                        && v.get("payTo").and_then(|x| x.as_str()) == Some(accept.pay_to.as_str())
                        && v.get("asset").and_then(|x| x.as_str()) == Some(accept.asset.as_str())
                })
                .cloned()
        })
        .unwrap_or_else(|| {
            json!({
                "scheme": accept.scheme,
                "network": accept.network,
                "asset": accept.asset,
                "amount": accept.amount,
                "payTo": accept.pay_to,
                "maxTimeoutSeconds": accept.max_timeout_seconds,
            })
        })
}

/// Lift the signed transaction out of the sidecar's v1 envelope and
/// re-wrap it in the x402 v2 payment payload zauth's matcher expects:
/// `{x402Version:2, accepted:<chosen requirement>, payload:{transaction}}`.
fn build_v2_payment(inner_envelope_b64: &str, accepted: Value) -> Result<String> {
    let bytes = B64
        .decode(inner_envelope_b64.trim())
        .map_err(|e| ZauthError::Sign(format!("decode signer envelope: {e}")))?;
    let v: Value = serde_json::from_slice(&bytes)
        .map_err(|e| ZauthError::Sign(format!("parse signer envelope: {e}")))?;
    let transaction = v
        .get("payload")
        .and_then(|p| p.get("transaction"))
        .and_then(|t| t.as_str())
        .ok_or_else(|| ZauthError::Sign("signer envelope missing payload.transaction".into()))?;
    let envelope = json!({
        "x402Version": 2,
        "accepted": accepted,
        "payload": { "transaction": transaction },
    });
    Ok(B64.encode(envelope.to_string().as_bytes()))
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
    /// Reason a rejected retry carried in its challenge header (e.g.
    /// "unsupported x402 version", "no matching payment requirements").
    /// `None` on success or when no detail was decodable.
    #[serde(default)]
    pub error_detail: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use covenant_x402::{PaymentRequirements, Signer};
    use wiremock::matchers::{header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Returns the v1 envelope shape the real sidecar signer emits,
    /// carrying a known transaction, so the v2 re-wrap path is exercised.
    struct FakeEnvelopeSigner;
    #[async_trait]
    impl Signer for FakeEnvelopeSigner {
        async fn build_payment(&self, _r: &PaymentRequirements) -> covenant_x402::Result<String> {
            let env = json!({
                "x402Version": 1, "scheme": "exact", "network": "solana",
                "payload": { "transaction": "FAKETX==" }
            });
            Ok(B64.encode(env.to_string().as_bytes()))
        }
    }

    const LIVE_DECODED: &str = r#"{
        "x402Version": 2,
        "error": "Payment required",
        "accepts": [
            {
                "scheme": "exact",
                "network": "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
                "amount": "50000",
                "asset": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                "payTo": "ZAU64eKWAgiGNux8bzvgRn8RvWqFhdMVrpJytF7V1qm",
                "maxTimeoutSeconds": 300,
                "extra": { "feePayer": "ZAU64eKWAgiGNux8bzvgRn8RvWqFhdMVrpJytF7V1qm" }
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
        let result = client
            .scan(&req(), &FakeEnvelopeSigner)
            .await
            .expect("paid");
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
        let result = client
            .scan(&req(), &FakeEnvelopeSigner)
            .await
            .expect("gratis");
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
            .scan(&req(), &FakeEnvelopeSigner)
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
        let err = client
            .scan(&req(), &FakeEnvelopeSigner)
            .await
            .expect_err("503");
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
            .scan(&req(), &FakeEnvelopeSigner)
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

    #[tokio::test]
    async fn oversized_reposcan_body_is_rejected_instead_of_buffered() {
        // A gratis 2xx whose body dwarfs the cap must be refused rather than
        // buffered whole into memory — the same OOM guard the directory read
        // already carries, applied to the RepoScan body.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/x402/reposcan"))
            .respond_with(ResponseTemplate::new(200).set_body_string("a".repeat(4096)))
            .mount(&server)
            .await;
        let client = RepoScanClient::with_limits(reqwest::Client::new(), server.uri(), 64);
        let err = client
            .scan(&req(), &FakeEnvelopeSigner)
            .await
            .expect_err("must reject an oversized reposcan body");
        assert!(
            matches!(err, ZauthError::ResponseTooLarge(_)),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn slow_reposcan_request_times_out_instead_of_hanging() {
        // A hung api.zauth.inc on the unpaid first POST must fail fast with a
        // typed error rather than wedge the CLI forever.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/x402/reposcan"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
            .mount(&server)
            .await;
        let client = RepoScanClient::with_base_url(
            http_client_with_timeout(Duration::from_millis(50)),
            server.uri(),
        );
        let err = client
            .scan(&req(), &FakeEnvelopeSigner)
            .await
            .expect_err("must time out");
        assert!(matches!(err, ZauthError::Http(_)), "got {err:?}");
    }

    #[test]
    fn paid_timeout_clamps_untrusted_max_timeout_seconds() {
        // The advertised window is honored within the band; a hostile 0 cannot
        // instant-abort a settlement and a hostile u32::MAX cannot hang forever.
        assert_eq!(paid_timeout(300), Duration::from_secs(300));
        assert_eq!(paid_timeout(0), Duration::from_secs(PAID_TIMEOUT_MIN_SECS));
        assert_eq!(
            paid_timeout(u32::MAX),
            Duration::from_secs(PAID_TIMEOUT_MAX_SECS)
        );
    }
}
