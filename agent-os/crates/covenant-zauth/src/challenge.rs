//! Decode the mainline x402 v2 challenge zauth carries in the
//! `payment-required` response header.
//!
//! Zauth's RepoScan responds with `HTTP 402` plus a base64-encoded
//! JSON payload in the `payment-required` header. The body is `{}`.
//! The decoded shape is mainline v2:
//!
//! ```json
//! {
//!   "x402Version": 2,
//!   "error": "Payment required",
//!   "resource": { "url": "...", "description": "...", "mimeType": "..." },
//!   "accepts": [
//!     {
//!       "scheme": "exact",
//!       "network": "eip155:8453" | "solana:...",
//!       "amount": "50000",
//!       "asset": "0x833589..." | "EPjFW...",
//!       "payTo": "0x2f513..." | "ZAU64...",
//!       "maxTimeoutSeconds": 300,
//!       "extra": { "feePayer": "..." } | { "name": "...", "version": "..." }
//!     }
//!   ],
//!   "extensions": { "sign-in-with-x": { ... } }
//! }
//! ```
//!
//! `extra.feePayer` is the v0 message payer on Solana, set to the
//! zauth treasury which signs at settle time. On Base there is no
//! `feePayer`; the caller pays gas.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use covenant_x402::{PaymentExtra, PaymentRequirements};
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};

use crate::{Result, ZauthError};

const HEADER: &str = "payment-required";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Challenge {
    #[serde(rename = "x402Version")]
    pub x402_version: u32,
    pub accepts: Vec<Accept>,
    #[serde(default)]
    pub resource: Option<Resource>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Resource {
    pub url: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "mimeType", default)]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Accept {
    pub scheme: String,
    pub network: String,
    pub asset: String,
    pub amount: String,
    #[serde(rename = "payTo")]
    pub pay_to: String,
    #[serde(rename = "maxTimeoutSeconds", default)]
    pub max_timeout_seconds: u32,
    #[serde(default)]
    pub extra: Option<AcceptExtra>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AcceptExtra {
    #[serde(rename = "feePayer", default)]
    pub fee_payer: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

/// Decode the challenge from the `payment-required` response header.
pub fn decode_from_headers(headers: &HeaderMap) -> Result<Challenge> {
    let raw = headers
        .get(HEADER)
        .ok_or(ZauthError::MissingChallengeHeader)?;
    let b64 = raw
        .to_str()
        .map_err(|e| ZauthError::DecodeChallenge(format!("header not ascii: {e}")))?;
    let json = B64
        .decode(b64)
        .map_err(|e| ZauthError::DecodeChallenge(format!("base64: {e}")))?;
    serde_json::from_slice(&json).map_err(|e| ZauthError::DecodeChallenge(format!("json: {e}")))
}

/// Pick the first accept option for `(network, asset)` that pays to
/// `expected_pay_to` and whose `amount` is within `per_call_cap`.
/// Rejects any matching `(network, asset)` option whose `payTo` does
/// not match the pinned treasury — defensive against a hostile or
/// misconfigured 402.
pub fn select<'a>(
    accepts: &'a [Accept],
    network: &str,
    asset: &str,
    expected_pay_to: &str,
    per_call_cap: u128,
) -> Result<&'a Accept> {
    let mut matched_chain_asset = false;
    for a in accepts {
        if a.scheme != "exact" || a.network != network || a.asset != asset {
            continue;
        }
        matched_chain_asset = true;
        if a.pay_to != expected_pay_to {
            return Err(ZauthError::UnpinnedPayTo {
                network: network.into(),
                got: a.pay_to.clone(),
                expected: expected_pay_to.into(),
            });
        }
        let n: u128 = a
            .amount
            .parse()
            .map_err(|_| ZauthError::DecodeChallenge(format!("amount: {}", a.amount)))?;
        if n <= per_call_cap {
            return Ok(a);
        }
    }
    Err(ZauthError::NoMatch(if matched_chain_asset {
        format!("all accepts on {network} / {asset} exceed cap {per_call_cap} atomic")
    } else {
        format!("no accept on {network} / {asset}")
    }))
}

/// Normalise a selected accept option into the signer's input.
/// USDC has 6 decimals; `amount_usdc` is a display convenience.
pub fn to_payment_requirements(accept: &Accept) -> PaymentRequirements {
    let amount_atomic = accept.amount.parse::<u128>().unwrap_or(0);
    let amount_usdc = amount_atomic as f64 / 1_000_000.0;
    PaymentRequirements {
        network: accept.network.clone(),
        asset: accept.asset.clone(),
        amount: accept.amount.clone(),
        amount_usdc,
        pay_to: accept.pay_to.clone(),
        scheme: accept.scheme.clone(),
        extra: accept
            .extra
            .as_ref()
            .and_then(|e| e.fee_payer.clone())
            .map(|fee_payer| PaymentExtra {
                fee_payer: Some(fee_payer),
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    /// Decoded live challenge from `POST https://api.zauth.inc/x402/reposcan`,
    /// captured 2026-06-03. Re-encoded via base64 in `live_headers()`.
    const LIVE_DECODED: &str = r#"{
        "x402Version": 2,
        "error": "Payment required",
        "resource": {
            "url": "https://api.zauth.inc/x402/reposcan",
            "description": "Scan a GitHub repository for code provenance and vulnerabilities. Returns cached results immediately or a session token for polling.",
            "mimeType": "application/json"
        },
        "accepts": [
            {
                "scheme": "exact",
                "network": "eip155:8453",
                "amount": "50000",
                "asset": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
                "payTo": "0x2f5134f7cb98AF03099FA682555Ca1dD70D7D688",
                "maxTimeoutSeconds": 300,
                "extra": { "name": "USD Coin", "version": "2" }
            },
            {
                "scheme": "exact",
                "network": "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
                "amount": "50000",
                "asset": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                "payTo": "ZAU64eKWAgiGNux8BzvgRn8RvWqFhdMVrpJytF7V1qm",
                "maxTimeoutSeconds": 300,
                "extra": { "feePayer": "ZAU64eKWAgiGNux8BzvgRn8RvWqFhdMVrpJytF7V1qm" }
            }
        ]
    }"#;

    fn live_headers() -> HeaderMap {
        let mut h = HeaderMap::new();
        let b64 = B64.encode(LIVE_DECODED.as_bytes());
        h.insert(HEADER, HeaderValue::from_str(&b64).unwrap());
        h
    }

    #[test]
    fn header_decodes_live_sample() {
        let c = decode_from_headers(&live_headers()).expect("decode");
        assert_eq!(c.x402_version, 2);
        assert_eq!(c.accepts.len(), 2);
        let base = &c.accepts[0];
        assert_eq!(base.network, crate::networks::BASE_MAINNET);
        assert_eq!(base.pay_to, crate::treasury::BASE);
        assert!(
            base.extra.as_ref().unwrap().fee_payer.is_none(),
            "Base option has no sponsored feePayer"
        );
        let sol = &c.accepts[1];
        assert_eq!(sol.network, crate::networks::SOLANA_MAINNET);
        assert_eq!(sol.amount, "50000");
        assert_eq!(sol.pay_to, crate::treasury::SOLANA);
        assert_eq!(
            sol.extra.as_ref().unwrap().fee_payer.as_deref(),
            Some(crate::treasury::SOLANA),
            "Solana option self-sponsors: feePayer equals payTo"
        );
    }

    #[test]
    fn missing_header_errors() {
        let h = HeaderMap::new();
        assert!(matches!(
            decode_from_headers(&h),
            Err(ZauthError::MissingChallengeHeader)
        ));
    }

    #[test]
    fn invalid_base64_errors() {
        let mut h = HeaderMap::new();
        h.insert(HEADER, HeaderValue::from_static("!!!not-base64!!!"));
        let err = decode_from_headers(&h).expect_err("err");
        assert!(matches!(err, ZauthError::DecodeChallenge(m) if m.contains("base64")));
    }

    #[test]
    fn malformed_json_errors() {
        let mut h = HeaderMap::new();
        let b64 = B64.encode(b"{not-json");
        h.insert(HEADER, HeaderValue::from_str(&b64).unwrap());
        let err = decode_from_headers(&h).expect_err("err");
        assert!(matches!(err, ZauthError::DecodeChallenge(m) if m.contains("json")));
    }

    #[test]
    fn select_picks_solana_within_cap() {
        let c = decode_from_headers(&live_headers()).unwrap();
        let picked = select(
            &c.accepts,
            crate::networks::SOLANA_MAINNET,
            crate::assets::USDC_SOLANA,
            crate::treasury::SOLANA,
            50_000,
        )
        .expect("select");
        assert_eq!(picked.amount, "50000");
        assert_eq!(picked.pay_to, crate::treasury::SOLANA);
    }

    #[test]
    fn select_picks_base_within_cap() {
        let c = decode_from_headers(&live_headers()).unwrap();
        let picked = select(
            &c.accepts,
            crate::networks::BASE_MAINNET,
            crate::assets::USDC_BASE,
            crate::treasury::BASE,
            50_000,
        )
        .expect("select");
        assert_eq!(picked.network, crate::networks::BASE_MAINNET);
    }

    #[test]
    fn select_rejects_over_cap() {
        let c = decode_from_headers(&live_headers()).unwrap();
        let err = select(
            &c.accepts,
            crate::networks::SOLANA_MAINNET,
            crate::assets::USDC_SOLANA,
            crate::treasury::SOLANA,
            49_999,
        )
        .expect_err("over cap");
        assert!(
            matches!(err, ZauthError::NoMatch(ref m) if m.contains("exceed cap")),
            "got {err:?}"
        );
    }

    #[test]
    fn select_rejects_unpinned_pay_to() {
        let mut c = decode_from_headers(&live_headers()).unwrap();
        c.accepts[1].pay_to = "WRONGtreasury111111111111111111111111111111".into();
        let err = select(
            &c.accepts,
            crate::networks::SOLANA_MAINNET,
            crate::assets::USDC_SOLANA,
            crate::treasury::SOLANA,
            50_000,
        )
        .expect_err("unpinned");
        assert!(
            matches!(err, ZauthError::UnpinnedPayTo { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn select_rejects_wrong_network() {
        let c = decode_from_headers(&live_headers()).unwrap();
        let err = select(
            &c.accepts,
            "solana:devnet",
            crate::assets::USDC_SOLANA,
            crate::treasury::SOLANA,
            50_000,
        )
        .expect_err("wrong net");
        assert!(matches!(err, ZauthError::NoMatch(m) if m.contains("no accept on")));
    }

    #[test]
    fn to_requirements_lifts_solana_fee_payer() {
        let c = decode_from_headers(&live_headers()).unwrap();
        let pr = to_payment_requirements(&c.accepts[1]);
        assert_eq!(pr.amount, "50000");
        assert_eq!(pr.amount_usdc, 0.05);
        assert_eq!(pr.pay_to, crate::treasury::SOLANA);
        assert_eq!(
            pr.extra.as_ref().unwrap().fee_payer.as_deref(),
            Some(crate::treasury::SOLANA)
        );
    }

    #[test]
    fn to_requirements_drops_non_fee_payer_extra_on_base() {
        let c = decode_from_headers(&live_headers()).unwrap();
        let pr = to_payment_requirements(&c.accepts[0]);
        assert!(
            pr.extra.is_none(),
            "Base extra is {{name, version}} only; no feePayer to lift"
        );
    }

    fn mk_accept(network: &str, asset: &str, amount: &str, pay_to: &str) -> Accept {
        Accept {
            scheme: "exact".into(),
            network: network.into(),
            asset: asset.into(),
            amount: amount.into(),
            pay_to: pay_to.into(),
            max_timeout_seconds: 0,
            extra: None,
        }
    }

    #[test]
    fn select_rejects_a_first_unpinned_match_even_when_a_later_one_is_pinned() {
        // A hostile or misconfigured 402 that lists a wrong payTo first must be
        // rejected outright, not skipped in favour of a later pinned option.
        let pinned = "PinnedTreasury11111111111111111111111111111";
        let accepts = vec![
            mk_accept(
                "solana",
                "usdc",
                "1000",
                "WrongTreasury111111111111111111111111111111",
            ),
            mk_accept("solana", "usdc", "1000", pinned),
        ];
        let err = select(&accepts, "solana", "usdc", pinned, 50_000)
            .expect_err("first unpinned match shadows the later pinned accept");
        assert!(
            matches!(err, ZauthError::UnpinnedPayTo { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn select_skips_an_over_cap_match_and_takes_a_later_within_cap_one() {
        // Over-cap is not a hard reject: a cheaper later accept on the same
        // pair is still eligible.
        let pinned = "PinnedTreasury11111111111111111111111111111";
        let accepts = vec![
            mk_accept("solana", "usdc", "90000", pinned),
            mk_accept("solana", "usdc", "40000", pinned),
        ];
        let picked = select(&accepts, "solana", "usdc", pinned, 50_000)
            .expect("the later within-cap accept is selected");
        assert_eq!(picked.amount, "40000");
    }

    #[test]
    fn to_requirements_coerces_an_unparseable_amount_to_zero() {
        let a = mk_accept(
            "solana",
            "usdc",
            "abc",
            "PinnedTreasury11111111111111111111111111111",
        );
        let pr = to_payment_requirements(&a);
        assert_eq!(pr.amount, "abc", "raw amount string is preserved");
        assert_eq!(pr.amount_usdc, 0.0, "unparseable amount degrades to 0");
    }

    #[test]
    fn select_rejects_a_matching_accept_with_an_unparseable_amount() {
        // A pinned accept on the right (network, asset) whose amount is not a
        // u128 must hard-fail: to_payment_requirements coerces a bad amount to
        // 0, but select runs first and must never let garbage reach the cap
        // check and get signed for.
        let pinned = "PinnedTreasury11111111111111111111111111111";
        let accepts = vec![mk_accept("solana", "usdc", "abc", pinned)];
        let err = select(&accepts, "solana", "usdc", pinned, 50_000)
            .expect_err("unparseable amount on a matching pinned accept");
        assert!(
            matches!(err, ZauthError::DecodeChallenge(ref m) if m.contains("amount: abc")),
            "got {err:?}"
        );
    }

    #[test]
    fn non_ascii_header_value_errors() {
        // obs-text bytes (0x80-0xff) are a legal header value but not visible
        // ASCII, so to_str() rejects them before any base64/json decode.
        let mut h = HeaderMap::new();
        h.insert(HEADER, HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap());
        let err = decode_from_headers(&h).expect_err("non-ascii header");
        assert!(
            matches!(err, ZauthError::DecodeChallenge(ref m) if m.contains("not ascii")),
            "got {err:?}"
        );
    }
}
