//! Xona's x402 challenge handling and the 402-then-pay loop.
//!
//! Xona serves the mainline x402 shape: a `402` body that is either a
//! bare array of payment options or `{"accepts": [ … ]}`, each option an
//! [`covenant_x402::PaymentRequirements`] with `amount`, `payTo`, and a
//! CAIP-2 `network`. Unlike the Hyre profile there is no sponsor
//! `feePayer`: Xona's Solana endpoints are self-paid, so the option's
//! `extra` is absent and the daemon's signer sidecar settles with
//! `SolanaSigner` (the funder pays its own fees).
//!
//! What's reused from `covenant-x402`: the [`Signer`] trait (the
//! funding-key sidecar) and the [`PaymentRequirements`] type it consumes.
//! Budget, settlement, and audit accounting wrap this loop in the daemon.

use covenant_x402::{PaymentRequirements, Signer};
use serde_json::Value;
use tracing::debug;

use crate::tools::PaidRequest;
use crate::{Result, XonaError};

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

/// Parse a Xona 402 body into its payment options. Tolerates both the
/// `{"accepts": [...]}` object and a bare `[...]` array.
pub fn parse_challenge(body: &str) -> Result<Vec<PaymentRequirements>> {
    let value: Value = serde_json::from_str(body)
        .map_err(|e| XonaError::Challenge(format!("decode 402 body: {e}")))?;
    let options = if value.is_array() {
        value
    } else {
        value
            .get("accepts")
            .cloned()
            .ok_or_else(|| XonaError::Challenge("402 body has no accepts array".into()))?
    };
    serde_json::from_value(options)
        .map_err(|e| XonaError::Challenge(format!("decode accepts: {e}")))
}

/// First option that settles on the caller's `(network, asset)` for an
/// `exact` payment within `per_call_cap`. Network match is lenient across
/// the short (`solana`) and CAIP-2 (`solana:…`) spellings.
pub fn select<'a>(
    options: &'a [PaymentRequirements],
    network: &str,
    asset: &str,
    per_call_cap: u128,
) -> Option<&'a PaymentRequirements> {
    options.iter().find(|o| {
        o.scheme == "exact"
            && o.asset == asset
            && network_matches(&o.network, network)
            && o.amount
                .parse::<u128>()
                .ok()
                .is_some_and(|n| n <= per_call_cap)
    })
}

fn network_matches(option: &str, want: &str) -> bool {
    option == want
        || want.starts_with(&format!("{option}:"))
        || option.starts_with(&format!("{want}:"))
}

/// Normalise a selected option into the signer's input. The payee is
/// pinned to the registry-advertised `expected_pay_to` so a manipulated
/// 402 challenge can't steer the funding key to another address, and the
/// network is forced to the operator's CAIP-2 id (the on-chain rail).
/// `extra` is carried through unchanged — absent for Xona's self-paid
/// Solana scheme, which routes the sidecar to `SolanaSigner`.
fn to_requirements(
    option: &PaymentRequirements,
    caip2_network: &str,
    expected_pay_to: &str,
) -> Result<PaymentRequirements> {
    if !expected_pay_to.is_empty() && option.pay_to != expected_pay_to {
        return Err(XonaError::NotAllowed(format!(
            "challenge payTo {} does not match the registry-advertised Xona payee {}",
            option.pay_to, expected_pay_to
        )));
    }
    Ok(PaymentRequirements {
        network: caip2_network.to_string(),
        asset: option.asset.clone(),
        amount: option.amount.clone(),
        amount_usdc: option.amount_usdc,
        pay_to: option.pay_to.clone(),
        scheme: option.scheme.clone(),
        extra: option.extra.clone(),
    })
}

/// Run the 402-then-pay loop for one resolved Xona call.
///
/// A first hit with no payment header either returns 2xx (free — no
/// settlement) or a 402 challenge. On 402 the matching option is signed
/// by `signer` and the request is retried once with the `x-payment`
/// header. Selection or payee-pin failures surface as
/// [`XonaError::NotAllowed`] — the call is rejected before the signer
/// runs, so no payment leaves the host for an out-of-policy option.
pub async fn execute_paid(
    http: &reqwest::Client,
    signer: &dyn Signer,
    plan: &PaidRequest,
) -> Result<PaidHttp> {
    let method = reqwest::Method::from_bytes(plan.method.as_bytes())
        .map_err(|_| XonaError::Execute(format!("invalid HTTP method: {:?}", plan.method)))?;

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
        return Err(XonaError::Execute(format!(
            "{} returned {} (not 402): {}",
            plan.url,
            status.as_u16(),
            truncate(&body)
        )));
    }

    let challenge = first.text().await?;
    let options = parse_challenge(&challenge)?;
    let option =
        select(&options, &plan.network, &plan.asset, plan.per_call_cap).ok_or_else(|| {
            XonaError::NotAllowed(format!(
                "no x402 option on {} / {} within cap {} atomic",
                plan.network, plan.asset, plan.per_call_cap
            ))
        })?;

    let requirements = to_requirements(option, &plan.network, &plan.pay_to)?;
    debug!(pay_to = %requirements.pay_to, amount = %requirements.amount, url = %plan.url, "xona x402 option selected; signing");

    let header = signer
        .build_payment(&requirements)
        .await
        .map_err(|e| XonaError::Execute(e.to_string()))?;

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

    const NETWORK: &str = crate::config::SOLANA_NETWORK;
    const ASSET: &str = crate::config::USDC_MINT;
    const PAY_TO: &str = crate::config::PAY_TO;

    /// A live-shaped Xona 402 challenge (bare array, no feePayer).
    fn challenge_body() -> String {
        serde_json::json!([{
            "network": NETWORK,
            "asset": ASSET,
            "amount": "30000",
            "amountUsdc": 0.03,
            "payTo": PAY_TO,
            "scheme": "exact"
        }])
        .to_string()
    }

    fn plan(url: &str, cap: u128) -> PaidRequest {
        PaidRequest {
            provider: "xona".into(),
            slug: "image/creative-director".into(),
            url: url.into(),
            method: "POST".into(),
            body: Some(serde_json::json!({ "prompt": "x" })),
            network: NETWORK.into(),
            asset: ASSET.into(),
            per_call_cap: cap,
            credits: 3,
            price_micro_usdc: 30_000,
            pay_to: PAY_TO.into(),
        }
    }

    #[test]
    fn parses_bare_array_and_object_shaped_challenges() {
        let arr = parse_challenge(&challenge_body()).expect("array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].amount, "30000");
        let obj =
            parse_challenge(&format!("{{\"accepts\": {}}}", challenge_body())).expect("object");
        assert_eq!(obj[0].pay_to, PAY_TO);
    }

    #[test]
    fn parse_challenge_rejects_malformed() {
        assert!(matches!(
            parse_challenge("{not json"),
            Err(XonaError::Challenge(_))
        ));
        assert!(matches!(
            parse_challenge(r#"{"error":"pay up"}"#),
            Err(XonaError::Challenge(m)) if m.contains("no accepts array")
        ));
    }

    #[test]
    fn select_respects_cap_asset_and_network() {
        let opts = parse_challenge(&challenge_body()).unwrap();
        assert!(select(&opts, NETWORK, ASSET, 30_000).is_some());
        assert!(select(&opts, NETWORK, ASSET, 29_999).is_none(), "over cap");
        assert!(
            select(&opts, NETWORK, "OTHER", 30_000).is_none(),
            "wrong asset"
        );
    }

    #[test]
    fn to_requirements_pins_payee() {
        let opt = parse_challenge(&challenge_body()).unwrap()[0].clone();
        to_requirements(&opt, NETWORK, PAY_TO).expect("pinned payee normalises");
        let err = to_requirements(&opt, NETWORK, "SomeOtherPayee").expect_err("payee mismatch");
        assert!(matches!(err, XonaError::NotAllowed(m) if m.contains("does not match")));
    }

    #[tokio::test]
    async fn full_loop_pays_self_paid_shape() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/image/creative-director"))
            .respond_with(ResponseTemplate::new(402).set_body_string(challenge_body()))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/image/creative-director"))
            .and(header_exists("x-payment"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "image_url": "https://x/y.png" })),
            )
            .mount(&server)
            .await;

        let out = execute_paid(
            &reqwest::Client::new(),
            &MockSigner,
            &plan(&format!("{}/image/creative-director", server.uri()), 30_000),
        )
        .await
        .expect("paid");
        assert_eq!(out.status, 200);
        assert_eq!(out.paid_amount.as_deref(), Some("30000"));
        let body: Value = serde_json::from_str(&out.body).unwrap();
        assert_eq!(body["image_url"], "https://x/y.png");
    }

    #[tokio::test]
    async fn over_cap_rejected_before_payment() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/c"))
            .respond_with(ResponseTemplate::new(402).set_body_string(challenge_body()))
            .mount(&server)
            .await;
        let err = execute_paid(
            &reqwest::Client::new(),
            &MockSigner,
            &plan(&format!("{}/c", server.uri()), 29_999),
        )
        .await
        .expect_err("over cap");
        assert!(matches!(err, XonaError::NotAllowed(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn payee_mismatch_rejected_before_signing() {
        let server = MockServer::start().await;
        // Challenge pays a different address than the plan's pinned payee.
        let hostile = serde_json::json!([{
            "network": NETWORK, "asset": ASSET, "amount": "30000", "amountUsdc": 0.03,
            "payTo": "So11111111111111111111111111111111111111112", "scheme": "exact"
        }])
        .to_string();
        Mock::given(method("POST"))
            .and(path("/c"))
            .respond_with(ResponseTemplate::new(402).set_body_string(hostile))
            .mount(&server)
            .await;
        let err = execute_paid(
            &reqwest::Client::new(),
            &MockSigner,
            &plan(&format!("{}/c", server.uri()), 30_000),
        )
        .await
        .expect_err("payee mismatch");
        assert!(
            matches!(err, XonaError::NotAllowed(ref m) if m.contains("payTo")),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn free_2xx_needs_no_payment() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/c"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })),
            )
            .mount(&server)
            .await;
        let out = execute_paid(
            &reqwest::Client::new(),
            &MockSigner,
            &plan(&format!("{}/c", server.uri()), 30_000),
        )
        .await
        .expect("free");
        assert_eq!(out.status, 200);
        assert!(out.paid_amount.is_none());
    }

    #[tokio::test]
    async fn rejected_payment_records_no_amount() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/c"))
            .respond_with(ResponseTemplate::new(402).set_body_string(challenge_body()))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/c"))
            .and(header_exists("x-payment"))
            .respond_with(ResponseTemplate::new(402).set_body_string("payment rejected"))
            .mount(&server)
            .await;
        let out = execute_paid(
            &reqwest::Client::new(),
            &MockSigner,
            &plan(&format!("{}/c", server.uri()), 30_000),
        )
        .await
        .expect("loop returns rejection");
        assert_eq!(out.status, 402);
        assert!(out.paid_amount.is_none());
    }
}
