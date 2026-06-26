//! Keyless x402 pay-per-call for AceData.
//!
//! AceData exposes an optional crypto pay-per-call path alongside Bearer
//! billing: a POST to a billed endpoint *without* an API key answers with
//! HTTP 402 carrying a mainline x402 **v2** challenge (the `accepts` array
//! in the response body). The agent pays the Solana option in USDC and
//! retries with an `X-PAYMENT` header — no API key, the payment is the
//! authorization.
//!
//! Verified live on mainnet 2026-06-23 (`/serp/google`, 0.000952 USDC,
//! `settlement: "authorized"`). Two wire-shape facts the signer has to
//! account for, both handled here:
//!
//! 1. The signer ([`covenant_x402::Signer`], a sidecar over the funding
//!    key) takes a **CAIP-2** network (`solana:5eykt4…`) and returns a v1
//!    envelope. AceData's challenge uses the **short** network id
//!    `"solana"` and a v2 payload.
//! 2. So we lift the signed `payload.transaction` out of the signer's v1
//!    envelope and re-wrap it as `{x402Version:2, scheme:"exact",
//!    network:"solana", payload:{transaction}}`. Sending CAIP-2 or v1 to
//!    AceData is rejected with `unsupported_kind`.
//!
//! Safety: the chosen accept must be USDC on Solana and its amount must be
//! at or under [`X402Payer::max_atomic`], so a hostile or misconfigured
//! 402 cannot drain the funding key. The payee is whatever the challenge
//! advertises (AceData's treasury) — not pinned, since it is theirs to
//! rotate, but the amount cap bounds the exposure per call.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use covenant_x402::{PaymentRequirements, Signer};
use serde_json::{json, Value};

use crate::{AceDataError, Result};

/// CAIP-2 id handed to the signer (it resolves the mint decimals + builds
/// the transfer against this). AceData's challenge uses the short
/// `"solana"`; the X-PAYMENT envelope must echo that short form.
const SOLANA_CAIP2: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";
const SOLANA_SHORT: &str = "solana";
/// USDC mint payments must be denominated in.
const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
/// Default per-call ceiling in atomic USDC (1 USDC). AceData calls cost
/// fractions of a cent; this is a generous backstop against a bad 402.
pub const DEFAULT_MAX_ATOMIC: u64 = 1_000_000;

/// Runs the 402-then-pay loop for AceData using a funding-key signer.
pub struct X402Payer {
    signer: std::sync::Arc<dyn Signer>,
    /// Refuse any 402 whose USDC amount exceeds this (atomic units).
    pub max_atomic: u64,
}

impl X402Payer {
    pub fn new(signer: std::sync::Arc<dyn Signer>) -> Self {
        Self {
            signer,
            max_atomic: DEFAULT_MAX_ATOMIC,
        }
    }

    pub fn with_max_atomic(mut self, max_atomic: u64) -> Self {
        self.max_atomic = if max_atomic == 0 {
            DEFAULT_MAX_ATOMIC
        } else {
            max_atomic
        };
        self
    }

    /// POST `body` to `url`. If the endpoint answers 402, pay the Solana
    /// USDC option (within the cap) and retry; otherwise return the body.
    pub async fn pay_and_post(
        &self,
        http: &reqwest::Client,
        url: &str,
        body: Value,
    ) -> Result<Value> {
        let first = http.post(url).json(&body).send().await?;
        if first.status().as_u16() != 402 {
            return decode_response(first).await;
        }

        let challenge: Value = first.json().await?;
        let accept = select_solana_usdc(&challenge, self.max_atomic)?;
        let requirements = to_requirements(&accept)?;

        let envelope = self
            .signer
            .build_payment(&requirements)
            .await
            .map_err(|e| AceDataError::Unexpected(format!("x402 sign: {e}")))?;
        let header = rewrap_v2(&envelope)?;

        let paid = http
            .post(url)
            .header("X-PAYMENT", header)
            .json(&body)
            .send()
            .await?;
        decode_response(paid).await
    }
}

/// Map AceData's `{success:false,error}` envelope (and non-2xx) onto our
/// error type; otherwise return the parsed body.
async fn decode_response(resp: reqwest::Response) -> Result<Value> {
    let status = resp.status();
    let value: Value = resp.json().await?;
    if value.get("success").and_then(Value::as_bool) == Some(false) || !status.is_success() {
        let err = value.get("error");
        let code = err
            .and_then(|e| e.get("code"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| if status.is_success() { "error" } else { "http" })
            .to_string();
        let message = err
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| format!("status {}", status.as_u16()));
        return Err(AceDataError::Api { code, message });
    }
    Ok(value)
}

/// Pick the Solana USDC accept within the cap from a v2 challenge.
fn select_solana_usdc(challenge: &Value, max_atomic: u64) -> Result<Value> {
    let accepts = challenge
        .get("accepts")
        .and_then(Value::as_array)
        .ok_or_else(|| AceDataError::Unexpected("402 challenge has no accepts array".into()))?;
    for a in accepts {
        if a.get("network").and_then(Value::as_str) != Some(SOLANA_SHORT) {
            continue;
        }
        if a.get("asset").and_then(Value::as_str) != Some(USDC_MINT) {
            continue;
        }
        let amount = accept_amount(a)
            .ok_or_else(|| AceDataError::Unexpected("solana accept missing amount".into()))?;
        if amount > max_atomic {
            return Err(AceDataError::Unexpected(format!(
                "x402 amount {amount} exceeds per-call cap {max_atomic} atomic USDC"
            )));
        }
        return Ok(a.clone());
    }
    Err(AceDataError::Unexpected(
        "no Solana USDC payment option in the 402 challenge".into(),
    ))
}

fn accept_amount(accept: &Value) -> Option<u64> {
    let raw = accept
        .get("maxAmountRequired")
        .or_else(|| accept.get("amount"))?;
    raw.as_u64()
        .or_else(|| raw.as_str().and_then(|s| s.parse().ok()))
}

/// Normalise a Solana USDC accept into the signer's [`PaymentRequirements`].
/// The signer needs the CAIP-2 network; `extra.feePayer` is absent (AceData
/// does not sponsor gas), so the funder pays.
fn to_requirements(accept: &Value) -> Result<PaymentRequirements> {
    let amount = accept_amount(accept)
        .ok_or_else(|| AceDataError::Unexpected("accept missing amount".into()))?;
    let pay_to = accept
        .get("payTo")
        .and_then(Value::as_str)
        .ok_or_else(|| AceDataError::Unexpected("accept missing payTo".into()))?
        .to_string();
    Ok(PaymentRequirements {
        network: SOLANA_CAIP2.to_string(),
        asset: USDC_MINT.to_string(),
        amount: amount.to_string(),
        amount_usdc: amount as f64 / 1_000_000.0,
        pay_to,
        scheme: "exact".to_string(),
        extra: None,
    })
}

/// Lift the signed transaction out of the signer's v1 envelope and
/// re-wrap it as the x402 **v2** payload AceData accepts, with the short
/// network id `"solana"`.
fn rewrap_v2(envelope_b64: &str) -> Result<String> {
    let bytes = B64
        .decode(envelope_b64.trim())
        .map_err(|e| AceDataError::Unexpected(format!("decode signer envelope: {e}")))?;
    let v: Value = serde_json::from_slice(&bytes)
        .map_err(|e| AceDataError::Unexpected(format!("parse signer envelope: {e}")))?;
    let transaction = v
        .get("payload")
        .and_then(|p| p.get("transaction"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AceDataError::Unexpected("signer envelope missing payload.transaction".into())
        })?;
    let v2 = json!({
        "x402Version": 2,
        "scheme": "exact",
        "network": SOLANA_SHORT,
        "payload": { "transaction": transaction },
    });
    Ok(B64.encode(v2.to_string().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    fn live_challenge() -> Value {
        json!({
            "x402Version": 2,
            "accepts": [
                { "scheme": "exact", "network": "base", "maxAmountRequired": "952",
                  "asset": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913", "payTo": "0xabc" },
                { "scheme": "exact", "network": "solana", "maxAmountRequired": "952",
                  "asset": USDC_MINT, "payTo": "5iVXFrYaYWX2GUTbkQj8mDBoBhAX8bneYigS2LJTia43" }
            ],
            "error": "Payment Required"
        })
    }

    #[test]
    fn selects_solana_usdc_and_pins_requirements() {
        let accept = select_solana_usdc(&live_challenge(), DEFAULT_MAX_ATOMIC).expect("solana");
        let req = to_requirements(&accept).expect("requirements");
        assert_eq!(req.network, SOLANA_CAIP2);
        assert_eq!(req.asset, USDC_MINT);
        assert_eq!(req.amount, "952");
        assert!(req.extra.is_none(), "AceData does not sponsor gas");
        assert_eq!(req.pay_to, "5iVXFrYaYWX2GUTbkQj8mDBoBhAX8bneYigS2LJTia43");
    }

    #[test]
    fn rejects_amount_over_cap() {
        let err = select_solana_usdc(&live_challenge(), 100).unwrap_err();
        assert!(matches!(err, AceDataError::Unexpected(m) if m.contains("exceeds per-call cap")));
    }

    #[test]
    fn rejects_when_no_solana_option() {
        let only_base = json!({ "x402Version": 2, "accepts": [
            { "scheme": "exact", "network": "base", "maxAmountRequired": "1", "asset": "0x", "payTo": "0x" }
        ]});
        assert!(select_solana_usdc(&only_base, DEFAULT_MAX_ATOMIC).is_err());
    }

    #[test]
    fn rewrap_lifts_tx_into_v2_short_network() {
        let v1 = json!({ "x402Version": 1, "scheme": "exact",
            "network": "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
            "payload": { "transaction": "BASE64TX==" } });
        let header = rewrap_v2(&B64.encode(v1.to_string().as_bytes())).unwrap();
        let decoded: Value = serde_json::from_slice(&B64.decode(header).unwrap()).unwrap();
        assert_eq!(decoded["x402Version"], 2);
        assert_eq!(decoded["network"], "solana");
        assert_eq!(decoded["payload"]["transaction"], "BASE64TX==");
    }

    struct FakeSigner;
    #[async_trait]
    impl Signer for FakeSigner {
        async fn build_payment(&self, _r: &PaymentRequirements) -> covenant_x402::Result<String> {
            let env = json!({ "x402Version": 1, "payload": { "transaction": "SIGNEDTX==" } });
            Ok(B64.encode(env.to_string().as_bytes()))
        }
    }

    #[tokio::test]
    async fn pays_402_then_returns_paid_body() {
        use wiremock::matchers::{header_exists, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        // Unpaid POST → 402 challenge.
        Mock::given(method("POST"))
            .and(path("/serp/google"))
            .respond_with(ResponseTemplate::new(402).set_body_json(live_challenge()))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // Paid retry (carries X-PAYMENT) → results + usdc cost.
        Mock::given(method("POST"))
            .and(path("/serp/google"))
            .and(header_exists("x-payment"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "organic": [{ "title": "r" }],
                "cost": { "amount": 0.000952, "currency": "usdc", "settlement": "authorized" }
            })))
            .mount(&server)
            .await;

        let payer = X402Payer::new(std::sync::Arc::new(FakeSigner));
        let url = format!("{}/serp/google", server.uri());
        let out = payer
            .pay_and_post(&reqwest::Client::new(), &url, json!({ "query": "q" }))
            .await
            .expect("paid");
        assert_eq!(out["organic"][0]["title"], "r");
        assert_eq!(out["cost"]["currency"], "usdc");
    }
}
