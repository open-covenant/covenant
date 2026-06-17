//! Syra's x402 challenge format and the 402-then-pay loop.
//!
//! Syra serves the official x402 v2 shape: a `402` body of
//! `{"x402Version": 2, "accepts": [ … ], "resource": {…}}` whose options
//! use `amount`, a CAIP-2 `network`, and `extra.feePayer` (Syra's
//! sponsor). The challenge is also base64 in a `payment-required` header.
//!
//! Two details vs the generic x402 client:
//! 1. The payment header is the standard `x-payment`, and the payload is
//!    x402 **v2** `{x402Version:2, accepted:<echoed requirement>,
//!    payload:{transaction}}`. The official matcher deep-equals the
//!    requirement against `accepted`, so we echo the selected accept
//!    verbatim.
//! 2. The retry hits the same URL (Syra's `resource.url` is the endpoint
//!    itself; no job indirection).
//!
//! Solana signing is reused from `covenant-x402`'s sidecar signer (it
//! returns a v1 envelope); we extract the signed `transaction` and
//! re-wrap it in Syra's v2 payload here.

use base64::Engine as _;
use covenant_x402::{PaymentRequirements, Signer};
use serde::Deserialize;
use serde_json::Value;
use tracing::debug;

use crate::tools::PaidRequest;
use crate::{Result, SyraError};

const PAYMENT_HEADER: &str = "x-payment";
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

/// One payment option from a Syra 402 challenge.
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

/// Parse a Syra 402 body into its typed payment options.
pub fn parse_challenge(body: &str) -> Result<Vec<Accept>> {
    accepts_value(body)?
        .into_iter()
        .map(|v| {
            serde_json::from_value(v)
                .map_err(|e| SyraError::Challenge(format!("decode accept: {e}")))
        })
        .collect()
}

/// The raw `accepts` JSON objects (kept verbatim so the selected one can
/// be echoed back into the v2 `accepted` field for matching).
fn accepts_value(body: &str) -> Result<Vec<Value>> {
    let value: Value = serde_json::from_str(body)
        .map_err(|e| SyraError::Challenge(format!("decode 402 body: {e}")))?;
    let accepts = if value.is_array() {
        value
    } else {
        value
            .get("accepts")
            .cloned()
            .ok_or_else(|| SyraError::Challenge("402 body has no accepts array".into()))?
    };
    serde_json::from_value(accepts)
        .map_err(|e| SyraError::Challenge(format!("decode accepts: {e}")))
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
/// payee and sponsor to Syra's published wallets.
fn to_requirements(accept: &Accept, caip2_network: &str) -> Result<PaymentRequirements> {
    if accept.pay_to != crate::config::PAY_TO {
        return Err(SyraError::NotAllowed(format!(
            "challenge pay_to {} does not match pinned Syra payee {}",
            accept.pay_to,
            crate::config::PAY_TO
        )));
    }
    // feePayer is the facilitator's gas sponsor, not the funds
    // destination, and Syra rotates it. Require it present (the signer
    // needs a sponsor to build the v0 message) but don't pin its value: a
    // wrong feePayer can't redirect funds (those go to the pinned payTo)
    // and simply fails to settle.
    let fee_payer = accept
        .extra
        .as_ref()
        .and_then(|e| e.fee_payer.as_deref())
        .ok_or_else(|| {
            SyraError::NotAllowed(
                "challenge missing extra.feePayer — Syra sponsors the fee, it is required".into(),
            )
        })?;
    let amount = accept
        .atomic_amount()
        .ok_or_else(|| SyraError::Challenge("accept missing amount".into()))?
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

/// Extract the base64 signed transaction from the sidecar signer's (v1)
/// envelope so we can re-wrap it in Syra's v2 payload.
fn extract_transaction(envelope_b64: &str) -> Result<String> {
    let bytes = B64
        .decode(envelope_b64.trim())
        .map_err(|e| SyraError::Execute(format!("decode signer envelope: {e}")))?;
    let v: Value = serde_json::from_slice(&bytes)
        .map_err(|e| SyraError::Execute(format!("parse signer envelope: {e}")))?;
    v.get("payload")
        .and_then(|p| p.get("transaction"))
        .and_then(|t| t.as_str())
        .map(String::from)
        .ok_or_else(|| SyraError::Execute("signer envelope missing payload.transaction".into()))
}

/// Build Syra's x402 v2 payment header: the chosen requirement echoed
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

/// Run the 402-then-pay loop for one resolved Syra call.
pub async fn execute_paid(
    http: &reqwest::Client,
    signer: &dyn Signer,
    plan: &PaidRequest,
) -> Result<PaidHttp> {
    let method = reqwest::Method::from_bytes(plan.method.as_bytes())
        .map_err(|_| SyraError::Execute(format!("invalid HTTP method: {:?}", plan.method)))?;

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
        return Err(SyraError::Execute(format!(
            "{} returned {} (not 402): {}",
            plan.url,
            status.as_u16(),
            truncate(&body)
        )));
    }

    let challenge = first.text().await?;
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
        SyraError::NotAllowed(format!(
            "no x402 option on {} / {} within cap {} atomic",
            plan.network, plan.asset, plan.per_call_cap
        ))
    })?;
    if let Some(fee_payer) = accept.extra.as_ref().and_then(|e| e.fee_payer.as_deref()) {
        debug!(%fee_payer, url = %plan.url, "Syra feePayer sponsors the tx");
    }

    let requirements = to_requirements(&accept, &plan.network)?;
    let inner = signer
        .build_payment(&requirements)
        .await
        .map_err(|e| SyraError::Execute(e.to_string()))?;
    let transaction = extract_transaction(&inner)?;
    let header = wrap_v2(&raw_accept, &transaction);

    // Syra retries the same URL (the resource is the endpoint itself).
    let paid = send(http, &method, &plan.url, plan.body.as_ref(), Some(&header)).await?;
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

    /// The exact /signal 402 body Syra's live API returns (Solana accept
    /// only, trimmed). If this stops parsing, Syra changed its wire form.
    const LIVE_SYRA_402: &str = r#"{
        "x402Version": 2,
        "error": "Payment required",
        "resource": { "url": "http://api.syraa.fun/signal", "description": "trading signal", "mimeType": "application/json" },
        "accepts": [{
            "scheme": "exact",
            "network": "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
            "amount": "100000",
            "asset": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            "payTo": "53JhuF8bgxvUQ59nDG6kWs4awUQYCS3wswQmUsV5uC7t",
            "maxTimeoutSeconds": 60,
            "extra": { "feePayer": "2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4" }
        }]
    }"#;

    const NETWORK: &str = crate::config::SOLANA_NETWORK;
    const ASSET: &str = crate::config::USDC_MINT;

    /// Returns a v1 envelope (what the real sidecar emits) carrying a
    /// known transaction, so the v2 re-wrap can be exercised offline.
    struct FakeEnvelopeSigner;
    #[async_trait]
    impl Signer for FakeEnvelopeSigner {
        async fn build_payment(&self, _r: &PaymentRequirements) -> covenant_x402::Result<String> {
            let env = serde_json::json!({
                "x402Version": 1, "scheme": "exact", "network": "solana",
                "payload": { "transaction": "FAKETX==" }
            });
            Ok(B64.encode(env.to_string().as_bytes()))
        }
    }

    fn plan(url: &str, cap: u128) -> PaidRequest {
        PaidRequest {
            provider: "syra".into(),
            slug: "signal".into(),
            url: url.into(),
            method: "GET".into(),
            body: None,
            network: NETWORK.into(),
            asset: ASSET.into(),
            per_call_cap: cap,
        }
    }

    #[test]
    fn parses_live_syra_challenge() {
        let accepts = parse_challenge(LIVE_SYRA_402).expect("parse");
        // Only the Solana accept is in this fixture.
        let sol = accepts
            .iter()
            .find(|a| a.network.starts_with("solana"))
            .expect("solana accept");
        assert_eq!(sol.atomic_amount(), Some("100000"));
        assert_eq!(sol.pay_to, crate::config::PAY_TO);
        assert!(
            sol.extra.as_ref().unwrap().fee_payer.is_some(),
            "challenge must carry a sponsor feePayer"
        );
    }

    #[test]
    fn select_matches_caip2_and_respects_cap() {
        let accepts = parse_challenge(LIVE_SYRA_402).unwrap();
        assert!(select(&accepts, NETWORK, ASSET, 100_000).is_some());
        assert!(
            select(&accepts, NETWORK, ASSET, 99_999).is_none(),
            "over cap"
        );
        assert!(
            select(&accepts, NETWORK, "OTHER_MINT", 100_000).is_none(),
            "wrong asset"
        );
    }

    #[test]
    fn to_requirements_pins_payee_requires_but_does_not_pin_sponsor() {
        let base = parse_challenge(LIVE_SYRA_402).expect("parse")[0].clone();
        to_requirements(&base, NETWORK).expect("live option normalises");

        // payTo is the funds destination — pinned hard.
        let mut wrong_payee = base.clone();
        wrong_payee.pay_to = "So11111111111111111111111111111111111111112".into();
        assert!(matches!(
            to_requirements(&wrong_payee, NETWORK),
            Err(SyraError::NotAllowed(m)) if m.contains("does not match pinned Syra payee")
        ));

        // A different feePayer is ACCEPTED — Syra rotates facilitators and
        // the sponsor never receives our funds (those go to the pinned
        // payTo). This is the case that broke when feePayer was hard-pinned.
        let mut rotated_fee_payer = base.clone();
        rotated_fee_payer.extra = Some(Extra {
            fee_payer: Some("AepWpq3GQwL8CeKMtZyKtKPa7W91Coygh3ropAJapVdU".into()),
        });
        to_requirements(&rotated_fee_payer, NETWORK)
            .expect("a rotated sponsor feePayer must still be accepted");

        // But a missing feePayer is rejected — the signer needs a sponsor.
        let mut no_fee_payer = base.clone();
        no_fee_payer.extra = None;
        assert!(matches!(
            to_requirements(&no_fee_payer, NETWORK),
            Err(SyraError::NotAllowed(m)) if m.contains("missing extra.feePayer")
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
            "scheme": "exact", "network": NETWORK, "amount": "100000",
            "asset": ASSET, "payTo": crate::config::PAY_TO,
            "maxTimeoutSeconds": 60, "extra": { "feePayer": crate::config::OBSERVED_FEE_PAYER }
        });
        let header = wrap_v2(&accepted, "TXDATA");
        let decoded: Value = serde_json::from_slice(&B64.decode(header).unwrap()).unwrap();
        assert_eq!(decoded["x402Version"], 2);
        assert_eq!(decoded["payload"]["transaction"], "TXDATA");
        assert_eq!(decoded["accepted"], accepted);
    }

    #[tokio::test]
    async fn full_loop_pays_v2_with_x_payment_same_url() {
        let server = MockServer::start().await;
        let challenge = serde_json::json!({
            "x402Version": 2,
            "error": "Payment required",
            "resource": { "url": format!("{}/signal", server.uri()) },
            "accepts": [{
                "scheme": "exact", "network": NETWORK, "amount": "100000",
                "asset": ASSET, "payTo": crate::config::PAY_TO, "maxTimeoutSeconds": 60,
                "extra": { "feePayer": crate::config::OBSERVED_FEE_PAYER }
            }]
        })
        .to_string();

        Mock::given(method("GET"))
            .and(path("/signal"))
            .respond_with(ResponseTemplate::new(402).set_body_string(challenge))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/signal"))
            .and(header_exists(PAYMENT_HEADER))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true, "paid": true, "signal": "neutral"
            })))
            .mount(&server)
            .await;

        let url = format!("{}/signal?token=solana", server.uri());
        let out = execute_paid(
            &reqwest::Client::new(),
            &FakeEnvelopeSigner,
            &plan(&url, 500_000),
        )
        .await
        .expect("paid");
        assert_eq!(out.status, 200);
        assert_eq!(out.paid_amount.as_deref(), Some("100000"));
        let body: Value = serde_json::from_str(&out.body).unwrap();
        assert_eq!(body["signal"], "neutral");
    }

    #[tokio::test]
    async fn over_cap_rejected_before_payment() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/signal"))
            .respond_with(ResponseTemplate::new(402).set_body_string(LIVE_SYRA_402))
            .mount(&server)
            .await;
        let url = format!("{}/signal", server.uri());
        let err = execute_paid(
            &reqwest::Client::new(),
            &FakeEnvelopeSigner,
            &plan(&url, 99_999),
        )
        .await
        .expect_err("over cap");
        assert!(matches!(err, SyraError::NotAllowed(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn free_2xx_needs_no_payment() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dashboard-summary"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })),
            )
            .mount(&server)
            .await;
        let url = format!("{}/dashboard-summary", server.uri());
        let out = execute_paid(
            &reqwest::Client::new(),
            &FakeEnvelopeSigner,
            &plan(&url, 500_000),
        )
        .await
        .expect("free");
        assert_eq!(out.status, 200);
        assert!(out.paid_amount.is_none());
    }
}
