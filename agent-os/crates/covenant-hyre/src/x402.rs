//! Hyre's x402 challenge format and the 402-then-pay loop.
//!
//! The x402 *challenge* wire shape is provider-specific, so Hyre parses
//! its own. Hyre serves the mainline x402 shape: a `402` body of
//! `{"accepts": [ … ], "x402Version": 1}` whose options use
//! `maxAmountRequired` and a short `"network": "solana"`, with the
//! CAIP-2 id and `feePayer` carried in the base64 `payment-required`
//! header.
//!
//! What's reused from `covenant-x402`: the [`Signer`] trait (the
//! funding-key sidecar) and the [`PaymentRequirements`] type the signer
//! consumes. We normalise a selected Hyre option into that type —
//! settling on the operator's CAIP-2 network and the exact
//! `maxAmountRequired` — and hand it to the signer. Budget, settlement,
//! and audit accounting wrap this loop in the daemon.
//!
//! The selected option's `extra.feePayer` carries PayAI's sponsor
//! wallet (pinned in [`crate::config::PAYAI_FEE_PAYER`]). The sidecar's
//! [`PayaiSolanaSigner`] uses it as the v0 message's `payerKey` and
//! partial-signs as the funder only — the fee-payer signature slot is
//! left empty for PayAI to fill at settle time. The standard x402
//! envelope (with the funder-signed tx) goes out in the `x-payment`
//! header on the retry; Hyre's middleware is the party that calls
//! PayAI's `/verify` and `/settle` to land the transfer.

use covenant_x402::{PaymentRequirements, Signer};
use serde::Deserialize;
use serde_json::Value;
use tracing::debug;

use crate::tools::PaidRequest;
use crate::{HyreError, Result};

/// One payment option from a Hyre 402 challenge.
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
    /// x402 v2 spells the price `amount`; v1 spells it
    /// `maxAmountRequired`. Accept either.
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
    /// Atomic price, from whichever of the two field spellings is set.
    pub fn atomic_amount(&self) -> Option<&str> {
        self.amount
            .as_deref()
            .or(self.max_amount_required.as_deref())
    }
}

/// Outcome of the loop. `paid_amount` is the atomic amount actually
/// settled (the live, authoritative figure recorded on the receipt);
/// `None` means the endpoint answered 2xx without a 402 — a free call
/// that needs no settlement.
#[derive(Debug, Clone, PartialEq)]
pub struct PaidHttp {
    pub status: u16,
    pub body: String,
    pub paid_amount: Option<String>,
}

/// Parse a Hyre 402 body into its payment options. Tolerates both the
/// `{"accepts": [...]}` object and a bare `[...]` array so the same
/// parser also handles array-shaped upstreams.
pub fn parse_challenge(body: &str) -> Result<Vec<Accept>> {
    let value: Value = serde_json::from_str(body)
        .map_err(|e| HyreError::Challenge(format!("decode 402 body: {e}")))?;
    let accepts = if value.is_array() {
        value
    } else {
        value
            .get("accepts")
            .cloned()
            .ok_or_else(|| HyreError::Challenge("402 body has no accepts array".into()))?
    };
    serde_json::from_value(accepts)
        .map_err(|e| HyreError::Challenge(format!("decode accepts: {e}")))
}

/// First option that settles on the caller's `(network, asset)` for an
/// `exact` payment within `per_call_cap`. Network match is lenient
/// across the short (`solana`) and CAIP-2 (`solana:…`) spellings since
/// Hyre's body and header disagree on which to use.
pub fn select<'a>(
    accepts: &'a [Accept],
    network: &str,
    asset: &str,
    per_call_cap: u128,
) -> Option<&'a Accept> {
    accepts.iter().find(|a| {
        a.scheme == "exact"
            && a.asset == asset
            && network_matches(&a.network, network)
            && a.atomic_amount()
                .and_then(|s| s.parse::<u128>().ok())
                .is_some_and(|n| n <= per_call_cap)
    })
}

fn network_matches(accept: &str, want: &str) -> bool {
    accept == want
        || want.starts_with(&format!("{accept}:"))
        || accept.starts_with(&format!("{want}:"))
}

/// Normalise a selected option into the signer's input. The network is
/// forced to the operator's CAIP-2 id (the on-chain rail), and the
/// amount is the option's exact atomic figure.
fn to_requirements(accept: &Accept, caip2_network: &str) -> Result<PaymentRequirements> {
    if accept.pay_to != crate::config::PAY_TO {
        return Err(HyreError::NotAllowed(format!(
            "challenge pay_to {} does not match pinned Hyre payee {}",
            accept.pay_to,
            crate::config::PAY_TO
        )));
    }
    let fee_payer = accept
        .extra
        .as_ref()
        .and_then(|e| e.fee_payer.as_deref())
        .ok_or_else(|| {
            HyreError::NotAllowed("challenge missing extra.feePayer — PayAI sponsor pubkey is required for the Hyre profile".into())
        })?;
    if fee_payer != crate::config::PAYAI_FEE_PAYER {
        return Err(HyreError::NotAllowed(format!(
            "challenge feePayer {fee_payer} does not match pinned PayAI sponsor {}",
            crate::config::PAYAI_FEE_PAYER
        )));
    }
    let amount = accept
        .atomic_amount()
        .ok_or_else(|| HyreError::Challenge("accept missing amount".into()))?
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

/// Run the 402-then-pay loop for one resolved Hyre call.
///
/// A first hit with no payment header either returns 2xx (free — no
/// settlement) or a 402 challenge. On 402 the matching option is signed
/// by `signer` and the request is retried once with the `x-payment`
/// header. Selection failures surface as [`HyreError::NotAllowed`] —
/// the call is rejected before the signer runs, so no payment leaves
/// the host for an out-of-policy option.
pub async fn execute_paid(
    http: &reqwest::Client,
    signer: &dyn Signer,
    plan: &PaidRequest,
) -> Result<PaidHttp> {
    let method = reqwest::Method::from_bytes(plan.method.as_bytes())
        .map_err(|_| HyreError::Execute(format!("invalid HTTP method: {:?}", plan.method)))?;

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
        return Err(HyreError::Execute(format!(
            "{} returned {} (not 402): {}",
            plan.url,
            status.as_u16(),
            truncate(&body)
        )));
    }

    let challenge = first.text().await?;
    let accepts = parse_challenge(&challenge)?;
    let accept =
        select(&accepts, &plan.network, &plan.asset, plan.per_call_cap).ok_or_else(|| {
            HyreError::NotAllowed(format!(
                "no x402 option on {} / {} within cap {} atomic",
                plan.network, plan.asset, plan.per_call_cap
            ))
        })?;
    if let Some(fee_payer) = accept.extra.as_ref().and_then(|e| e.fee_payer.as_deref()) {
        debug!(%fee_payer, url = %plan.url, "payai facilitator co-signs as feePayer");
    }

    let requirements = to_requirements(accept, &plan.network)?;
    let header = signer
        .build_payment(&requirements)
        .await
        .map_err(|e| HyreError::Execute(e.to_string()))?;

    let paid = send(http, &method, &plan.url, plan.body.as_ref(), Some(&header)).await?;
    let status = paid.status();
    let body = paid.text().await?;
    let paid_amount = if status.is_success() {
        Some(requirements.amount)
    } else {
        None
    };
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
        req = req.header("x-payment", h);
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
    use covenant_x402::MockSigner;
    use wiremock::matchers::{header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The exact 402 body Hyre's live API returns for GET /defi/tvl,
    /// captured from the production endpoint. If this stops parsing,
    /// Hyre changed its challenge wire format.
    const LIVE_DEFI_TVL_402: &str = r#"{
        "error": "X-PAYMENT header is required",
        "accepts": [{
            "scheme": "exact",
            "network": "solana",
            "maxAmountRequired": "10000",
            "resource": "https://mpp.hyreagent.fun/defi/tvl",
            "description": "Total Value Locked across DeFi chains.",
            "mimeType": "application/json",
            "payTo": "7G73PLhKvAPBGTzG5ESAE4coE7QrVeTTKfhTxQZbyGgC",
            "maxTimeoutSeconds": 60,
            "asset": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            "extra": { "feePayer": "2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4" }
        }],
        "x402Version": 1
    }"#;

    const NETWORK: &str = crate::config::SOLANA_NETWORK;
    const ASSET: &str = crate::config::USDC_MINT;

    fn plan(url: &str, cap: u128) -> PaidRequest {
        PaidRequest {
            provider: "hyre".into(),
            slug: "defi/tvl".into(),
            url: url.into(),
            method: "GET".into(),
            body: None,
            network: NETWORK.into(),
            asset: ASSET.into(),
            per_call_cap: cap,
            credits: 1,
            price_micro_usdc: 10_000,
        }
    }

    #[test]
    fn parses_live_object_shaped_challenge() {
        let accepts = parse_challenge(LIVE_DEFI_TVL_402).expect("parse");
        assert_eq!(accepts.len(), 1);
        assert_eq!(accepts[0].atomic_amount(), Some("10000"));
        assert_eq!(accepts[0].pay_to, crate::config::PAY_TO);
        assert_eq!(
            accepts[0].extra.as_ref().unwrap().fee_payer.as_deref(),
            Some("2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4")
        );
    }

    #[test]
    fn parses_bare_array_shaped_challenge() {
        let body =
            r#"[{"scheme":"exact","network":"solana:x","asset":"M","payTo":"P","amount":"5"}]"#;
        let accepts = parse_challenge(body).expect("parse array");
        assert_eq!(accepts[0].atomic_amount(), Some("5"));
    }

    #[test]
    fn select_matches_short_network_against_caip2_capability() {
        let accepts = parse_challenge(LIVE_DEFI_TVL_402).unwrap();
        // Body says "solana"; capability is the CAIP-2 form — must match.
        assert!(select(&accepts, NETWORK, ASSET, 10_000).is_some());
    }

    #[test]
    fn select_rejects_over_cap_and_wrong_asset() {
        let accepts = parse_challenge(LIVE_DEFI_TVL_402).unwrap();
        assert!(
            select(&accepts, NETWORK, ASSET, 9_999).is_none(),
            "over cap"
        );
        assert!(
            select(&accepts, NETWORK, "OTHER_MINT", 10_000).is_none(),
            "wrong asset"
        );
    }

    #[tokio::test]
    async fn full_loop_pays_live_shape_and_returns_data() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/defi/tvl"))
            .respond_with(ResponseTemplate::new(402).set_body_string(LIVE_DEFI_TVL_402))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/defi/tvl"))
            .and(header_exists("x-payment"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "tvl": 1 }, "signal": "low_yield", "confidence": 0.9,
                "sources": ["DeFiLlama"], "latency_ms": 12, "timestamp": "2026-05-26T00:00:00Z"
            })))
            .mount(&server)
            .await;

        let out = execute_paid(
            &reqwest::Client::new(),
            &MockSigner,
            &plan(&format!("{}/defi/tvl", server.uri()), 10_000),
        )
        .await
        .expect("paid");
        assert_eq!(out.status, 200);
        assert_eq!(out.paid_amount.as_deref(), Some("10000"));
        let body: Value = serde_json::from_str(&out.body).unwrap();
        assert_eq!(body["data"]["tvl"], 1);
    }

    #[tokio::test]
    async fn over_cap_is_rejected_before_payment() {
        let server = MockServer::start().await;
        // Only the 402 is mounted; if the loop tried to pay, the retry
        // would 404 and the status assert below would change. NotAllowed
        // proves we stopped before signing.
        Mock::given(method("GET"))
            .and(path("/defi/tvl"))
            .respond_with(ResponseTemplate::new(402).set_body_string(LIVE_DEFI_TVL_402))
            .mount(&server)
            .await;

        let err = execute_paid(
            &reqwest::Client::new(),
            &MockSigner,
            &plan(&format!("{}/defi/tvl", server.uri()), 9_999),
        )
        .await
        .expect_err("over cap");
        assert!(matches!(err, HyreError::NotAllowed(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn free_2xx_needs_no_payment() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/defi/tvl"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })),
            )
            .mount(&server)
            .await;
        let out = execute_paid(
            &reqwest::Client::new(),
            &MockSigner,
            &plan(&format!("{}/defi/tvl", server.uri()), 10_000),
        )
        .await
        .expect("free");
        assert_eq!(out.status, 200);
        assert!(out.paid_amount.is_none());
    }
}
