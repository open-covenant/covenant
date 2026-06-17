//! WURK's x402 challenge format and the 402-then-pay loop.
//!
//! WURK serves the object x402 v2 shape: a `402` body of
//! `{"x402Version": 2, "accepts": [ … ], "resource": {…}}` whose options
//! use `amount`, a CAIP-2 `network`, and `extra.feePayer` (the PayAI
//! sponsor). The job-specific pay URL is in `resource.url`.
//!
//! Two WURK-specific quirks vs the generic x402 client:
//! 1. The retry goes to `resource.url` (a fresh job is minted per create
//!    GET), not the create URL.
//! 2. The payment header is `PAYMENT-SIGNATURE`, and the payload is the
//!    x402 **v2** shape `{x402Version:2, accepted:<echoed requirement>,
//!    payload:{transaction}}`. The official x402 matcher does a
//!    `deepEqual` of the requirement against `accepted`, so we echo the
//!    selected accept verbatim.
//!
//! The Solana signing is reused from `covenant-x402`'s sidecar signer
//! (it returns a v1 envelope); we extract the signed `transaction` from
//! it and re-wrap it in WURK's v2 payload here.

use base64::Engine as _;
use covenant_x402::{PaymentRequirements, Signer};
use serde::Deserialize;
use serde_json::Value;
use tracing::debug;

use crate::tools::PaidRequest;
use crate::{Result, WurkError};

const PAYMENT_HEADER: &str = "PAYMENT-SIGNATURE";
const B64: base64::engine::general_purpose::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// One payment option from a WURK 402 challenge.
#[derive(Debug, Clone, Deserialize)]
pub struct Accept {
    #[serde(default)]
    pub scheme: String,
    #[serde(default)]
    pub network: String,
    #[serde(default)]
    pub asset: String,
    #[serde(rename = "payTo", default)]
    pub pay_to: String,
    #[serde(default)]
    pub amount: Option<String>,
    #[serde(rename = "maxAmountRequired", default)]
    pub max_amount_required: Option<String>,
    #[serde(default)]
    pub extra: Option<Extra>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Extra {
    #[serde(rename = "feePayer", default)]
    pub fee_payer: Option<String>,
}

impl Accept {
    pub fn atomic_amount(&self) -> Option<&str> {
        self.amount
            .as_deref()
            .or(self.max_amount_required.as_deref())
    }
}

/// Outcome of the loop. `paid_amount` is the atomic amount settled;
/// `None` means a 2xx without a 402 (a free call).
#[derive(Debug, Clone, PartialEq)]
pub struct PaidHttp {
    pub status: u16,
    pub body: String,
    pub paid_amount: Option<String>,
}

/// Parse a WURK 402 body into its typed payment options.
pub fn parse_challenge(body: &str) -> Result<Vec<Accept>> {
    accepts_value(body)?
        .into_iter()
        .map(|v| {
            serde_json::from_value(v).map_err(|e| WurkError::Challenge(format!("decode accept: {e}")))
        })
        .collect()
}

/// The raw `accepts` JSON objects (kept verbatim so the selected one can
/// be echoed back into the v2 `accepted` field for matching).
fn accepts_value(body: &str) -> Result<Vec<Value>> {
    let value: Value = serde_json::from_str(body)
        .map_err(|e| WurkError::Challenge(format!("decode 402 body: {e}")))?;
    let accepts = if value.is_array() {
        value
    } else {
        value
            .get("accepts")
            .cloned()
            .ok_or_else(|| WurkError::Challenge("402 body has no accepts array".into()))?
    };
    serde_json::from_value(accepts)
        .map_err(|e| WurkError::Challenge(format!("decode accepts: {e}")))
}

/// The job-specific pay URL WURK returns in the 402 `resource.url`.
pub fn resource_url(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .get("resource")?
        .get("url")?
        .as_str()
        .map(String::from)
}

fn accept_matches(a: &Accept, network: &str, asset: &str, per_call_cap: u128) -> bool {
    a.scheme == "exact"
        && a.asset == asset
        && network_matches(&a.network, network)
        && a.atomic_amount()
            .and_then(|s| s.parse::<u128>().ok())
            .is_some_and(|n| n <= per_call_cap)
}

/// First `exact` option on the caller's `(network, asset)` within cap.
pub fn select<'a>(
    accepts: &'a [Accept],
    network: &str,
    asset: &str,
    per_call_cap: u128,
) -> Option<&'a Accept> {
    accepts
        .iter()
        .find(|a| accept_matches(a, network, asset, per_call_cap))
}

fn network_matches(accept: &str, want: &str) -> bool {
    accept == want
        || want.starts_with(&format!("{accept}:"))
        || accept.starts_with(&format!("{want}:"))
}

/// Normalise a selected option into the signer's input, pinning the
/// payee and sponsor to WURK's published wallets.
fn to_requirements(accept: &Accept, caip2_network: &str) -> Result<PaymentRequirements> {
    if accept.pay_to != crate::config::PAY_TO {
        return Err(WurkError::NotAllowed(format!(
            "challenge pay_to {} does not match pinned WURK payee {}",
            accept.pay_to,
            crate::config::PAY_TO
        )));
    }
    let fee_payer = accept
        .extra
        .as_ref()
        .and_then(|e| e.fee_payer.as_deref())
        .ok_or_else(|| {
            WurkError::NotAllowed(
                "challenge missing extra.feePayer — WURK sponsors the fee, it is required".into(),
            )
        })?;
    if fee_payer != crate::config::FEE_PAYER {
        return Err(WurkError::NotAllowed(format!(
            "challenge feePayer {fee_payer} does not match pinned sponsor {}",
            crate::config::FEE_PAYER
        )));
    }
    let amount = accept
        .atomic_amount()
        .ok_or_else(|| WurkError::Challenge("accept missing amount".into()))?
        .to_string();
    let amount_usdc = amount.parse::<u128>().unwrap_or(0) as f64 / 1_000_000.0;
    Ok(PaymentRequirements {
        network: caip2_network.to_string(),
        asset: accept.asset.clone(),
        amount,
        amount_usdc,
        pay_to: accept.pay_to.clone(),
        scheme: accept.scheme.clone(),
        extra: Some(covenant_x402::PaymentExtra {
            fee_payer: Some(fee_payer.to_string()),
        }),
    })
}

/// Extract the base64 signed transaction from the sidecar signer's
/// (v1) envelope so we can re-wrap it in WURK's v2 payload.
fn extract_transaction(envelope_b64: &str) -> Result<String> {
    let bytes = B64
        .decode(envelope_b64.trim())
        .map_err(|e| WurkError::Execute(format!("decode signer envelope: {e}")))?;
    let v: Value = serde_json::from_slice(&bytes)
        .map_err(|e| WurkError::Execute(format!("parse signer envelope: {e}")))?;
    v.get("payload")
        .and_then(|p| p.get("transaction"))
        .and_then(|t| t.as_str())
        .map(String::from)
        .ok_or_else(|| WurkError::Execute("signer envelope missing payload.transaction".into()))
}

/// Build WURK's x402 v2 payment header: the chosen requirement echoed
/// into `accepted` (so the server's deepEqual match succeeds) plus the
/// signed transaction.
fn wrap_v2(accepted: &Value, transaction: &str) -> String {
    let envelope = serde_json::json!({
        "x402Version": 2,
        "accepted": accepted,
        "payload": { "transaction": transaction },
    });
    B64.encode(envelope.to_string().as_bytes())
}

/// Run the 402-then-pay loop for one resolved WURK call.
pub async fn execute_paid(
    http: &reqwest::Client,
    signer: &dyn Signer,
    plan: &PaidRequest,
) -> Result<PaidHttp> {
    let method = reqwest::Method::from_bytes(plan.method.as_bytes())
        .map_err(|_| WurkError::Execute(format!("invalid HTTP method: {:?}", plan.method)))?;

    let first = send(http, &method, &plan.url, plan.body.as_ref(), None).await?;
    let status = first.status();
    if status.is_success() {
        let body = first.text().await?;
        return Ok(PaidHttp {
            status: status.as_u16(),
            body,
            paid_amount: None,
        });
    }
    if status.as_u16() != 402 {
        let body = first.text().await.unwrap_or_default();
        return Err(WurkError::Execute(format!(
            "{} returned {} (not 402): {}",
            plan.url,
            status.as_u16(),
            truncate(&body)
        )));
    }

    let challenge = first.text().await?;
    // WURK mints a fresh job per create GET and returns a job-specific
    // pay URL in `resource.url`. Retry the payment against THAT url.
    let retry_url = resource_url(&challenge).unwrap_or_else(|| plan.url.clone());

    let raws = accepts_value(&challenge)?;
    let mut selected: Option<(Accept, Value)> = None;
    for raw in raws {
        if let Ok(a) = serde_json::from_value::<Accept>(raw.clone()) {
            if accept_matches(&a, &plan.network, &plan.asset, plan.per_call_cap) {
                selected = Some((a, raw));
                break;
            }
        }
    }
    let (accept, raw_accept) = selected.ok_or_else(|| {
        WurkError::NotAllowed(format!(
            "no x402 option on {} / {} within cap {} atomic",
            plan.network, plan.asset, plan.per_call_cap
        ))
    })?;
    if let Some(fee_payer) = accept.extra.as_ref().and_then(|e| e.fee_payer.as_deref()) {
        debug!(%fee_payer, url = %retry_url, "WURK feePayer sponsors the tx");
    }

    let requirements = to_requirements(&accept, &plan.network)?;
    let inner = signer
        .build_payment(&requirements)
        .await
        .map_err(|e| WurkError::Execute(e.to_string()))?;
    let transaction = extract_transaction(&inner)?;
    let header = wrap_v2(&raw_accept, &transaction);

    let paid = send(http, &method, &retry_url, plan.body.as_ref(), Some(&header)).await?;
    let status = paid.status();
    let body = paid.text().await?;
    let paid_amount = status.is_success().then(|| requirements.amount.clone());
    Ok(PaidHttp {
        status: status.as_u16(),
        body,
        paid_amount,
    })
}

async fn send(
    http: &reqwest::Client,
    method: &reqwest::Method,
    url: &str,
    body: Option<&Value>,
    payment_header: Option<&str>,
) -> Result<reqwest::Response> {
    let mut req = http.request(method.clone(), url);
    if let Some(b) = body {
        req = req.json(b);
    }
    if let Some(h) = payment_header {
        req = req.header(PAYMENT_HEADER, h);
    }
    Ok(req.send().await?)
}

fn truncate(s: &str) -> String {
    let cut: String = s.chars().take(200).collect();
    if cut.len() < s.len() {
        format!("{cut}…")
    } else {
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use wiremock::matchers::{header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The exact advanced-create 402 body WURK's live API returns.
    const LIVE_WURK_402: &str = r#"{
        "x402Version": 2,
        "accepts": [{
            "scheme": "exact",
            "network": "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
            "amount": "1000000",
            "asset": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            "payTo": "SAT8g2xU7AFy7eUmNJ9SNrM6yYo7LDCi13GXJ8Ez9kC",
            "maxTimeoutSeconds": 60,
            "extra": { "feePayer": "2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4" }
        }],
        "resource": { "url": "https://wurkapi.fun/api/x402/jobs/6a5f09dc/pay", "description": "Pay to unlock job execution", "mimeType": "application/json" }
    }"#;

    const NETWORK: &str = crate::config::SOLANA_NETWORK;
    const ASSET: &str = crate::config::USDC_MINT;

    /// Returns a v1 envelope (what the real sidecar emits) carrying a
    /// known transaction, so the v2 re-wrap can be exercised offline.
    struct FakeEnvelopeSigner;
    #[async_trait]
    impl Signer for FakeEnvelopeSigner {
        async fn build_payment(
            &self,
            _r: &PaymentRequirements,
        ) -> covenant_x402::Result<String> {
            let env = serde_json::json!({
                "x402Version": 1, "scheme": "exact", "network": "solana",
                "payload": { "transaction": "FAKETX==" }
            });
            Ok(B64.encode(env.to_string().as_bytes()))
        }
    }

    fn plan(url: &str, cap: u128) -> PaidRequest {
        PaidRequest {
            provider: "wurk".into(),
            slug: "agenttohuman".into(),
            url: url.into(),
            method: "GET".into(),
            body: None,
            network: NETWORK.into(),
            asset: ASSET.into(),
            per_call_cap: cap,
        }
    }

    #[test]
    fn parses_live_wurk_challenge() {
        let accepts = parse_challenge(LIVE_WURK_402).expect("parse");
        assert_eq!(accepts.len(), 1);
        assert_eq!(accepts[0].atomic_amount(), Some("1000000"));
        assert_eq!(accepts[0].pay_to, crate::config::PAY_TO);
        assert_eq!(
            accepts[0].extra.as_ref().unwrap().fee_payer.as_deref(),
            Some(crate::config::FEE_PAYER)
        );
    }

    #[test]
    fn resource_url_extracted() {
        assert_eq!(
            resource_url(LIVE_WURK_402).as_deref(),
            Some("https://wurkapi.fun/api/x402/jobs/6a5f09dc/pay")
        );
    }

    #[test]
    fn select_matches_caip2_and_respects_cap() {
        let accepts = parse_challenge(LIVE_WURK_402).unwrap();
        assert!(select(&accepts, NETWORK, ASSET, 1_000_000).is_some());
        assert!(select(&accepts, NETWORK, ASSET, 999_999).is_none());
        assert!(select(&accepts, NETWORK, "OTHER_MINT", 1_000_000).is_none());
    }

    #[test]
    fn to_requirements_pins_payee_and_sponsor() {
        let base = parse_challenge(LIVE_WURK_402).expect("parse")[0].clone();
        to_requirements(&base, NETWORK).expect("pinned live option normalises");

        let mut wrong_payee = base.clone();
        wrong_payee.pay_to = "So11111111111111111111111111111111111111112".into();
        assert!(matches!(
            to_requirements(&wrong_payee, NETWORK),
            Err(WurkError::NotAllowed(m)) if m.contains("does not match pinned WURK payee")
        ));

        let mut wrong_fee_payer = base.clone();
        wrong_fee_payer.extra = Some(Extra {
            fee_payer: Some("11111111111111111111111111111111".into()),
        });
        assert!(matches!(
            to_requirements(&wrong_fee_payer, NETWORK),
            Err(WurkError::NotAllowed(m)) if m.contains("does not match pinned sponsor")
        ));
    }

    #[test]
    fn extract_transaction_reads_payload() {
        let env = serde_json::json!({ "payload": { "transaction": "ABC123" } });
        let b64 = B64.encode(env.to_string().as_bytes());
        assert_eq!(extract_transaction(&b64).unwrap(), "ABC123");
        assert!(extract_transaction("not base64!!!").is_err());
    }

    #[test]
    fn wrap_v2_echoes_accepted_and_carries_tx() {
        let accepted = serde_json::json!({
            "scheme": "exact", "network": "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
            "amount": "10000", "asset": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            "payTo": "SAT8g2xU7AFy7eUmNJ9SNrM6yYo7LDCi13GXJ8Ez9kC",
            "maxTimeoutSeconds": 60, "extra": { "feePayer": "2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4" }
        });
        let header = wrap_v2(&accepted, "TXDATA");
        let decoded: Value =
            serde_json::from_slice(&B64.decode(header).unwrap()).unwrap();
        assert_eq!(decoded["x402Version"], 2);
        assert_eq!(decoded["payload"]["transaction"], "TXDATA");
        // The accepted block must be echoed verbatim for the server's
        // deepEqual match to succeed.
        assert_eq!(decoded["accepted"], accepted);
    }

    #[tokio::test]
    async fn full_loop_pays_v2_against_resource_url() {
        let server = MockServer::start().await;
        // Challenge whose resource.url points at the mock's pay path.
        let challenge = serde_json::json!({
            "x402Version": 2,
            "accepts": [{
                "scheme": "exact", "network": NETWORK, "amount": "10000",
                "asset": ASSET, "payTo": crate::config::PAY_TO, "maxTimeoutSeconds": 60,
                "extra": { "feePayer": crate::config::FEE_PAYER }
            }],
            "resource": { "url": format!("{}/api/x402/jobs/t/pay", server.uri()) }
        })
        .to_string();

        Mock::given(method("GET"))
            .and(path("/solana/agenttohuman"))
            .respond_with(ResponseTemplate::new(402).set_body_string(challenge))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/x402/jobs/t/pay"))
            .and(header_exists(PAYMENT_HEADER))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true, "paid": true, "jobId": "abc123", "secret": "s3cr3t"
            })))
            .mount(&server)
            .await;

        let url = format!("{}/solana/agenttohuman?description=ping", server.uri());
        let out = execute_paid(&reqwest::Client::new(), &FakeEnvelopeSigner, &plan(&url, 1_000_000))
            .await
            .expect("paid");
        assert_eq!(out.status, 200);
        assert_eq!(out.paid_amount.as_deref(), Some("10000"));
        let body: Value = serde_json::from_str(&out.body).unwrap();
        assert_eq!(body["jobId"], "abc123");
        assert_eq!(body["secret"], "s3cr3t");
    }

    #[tokio::test]
    async fn over_cap_rejected_before_payment() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/solana/agenttohuman"))
            .respond_with(ResponseTemplate::new(402).set_body_string(LIVE_WURK_402))
            .mount(&server)
            .await;
        let url = format!("{}/solana/agenttohuman?description=ping", server.uri());
        let err = execute_paid(&reqwest::Client::new(), &FakeEnvelopeSigner, &plan(&url, 999_999))
            .await
            .expect_err("over cap");
        assert!(matches!(err, WurkError::NotAllowed(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn free_2xx_needs_no_payment() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/solana/agenttohuman"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
            .mount(&server)
            .await;
        let url = format!("{}/solana/agenttohuman?action=view&secret=x", server.uri());
        let out = execute_paid(&reqwest::Client::new(), &FakeEnvelopeSigner, &plan(&url, 1_000_000))
            .await
            .expect("free");
        assert_eq!(out.status, 200);
        assert!(out.paid_amount.is_none());
    }
}
