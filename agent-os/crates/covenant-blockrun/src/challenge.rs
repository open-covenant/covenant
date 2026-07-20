//! Parsing BlockRun's x402 402 challenge.
//!
//! BlockRun returns the challenge base64-encoded in the `x-payment-required`
//! response header (and mirrors it in `www-authenticate: X402 requirements="…"`),
//! carrying an `accepts` array of payment options rather than the bare
//! `Vec<PaymentRequirements>` some x402 sellers use. We decode it ourselves and
//! convert the chosen option into the shape `covenant_x402::Signer` expects.

use base64::Engine;
use covenant_x402::{PaymentExtra, PaymentRequirements};
use serde::{Deserialize, Serialize};

use crate::{BlockRunError, Result};

/// USDC is 6 decimals on both Base and Solana; used to project the atomic
/// `amount` into a USD figure for display and cap checks.
const USDC_DECIMALS: u32 = 6;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Challenge {
    #[serde(rename = "x402Version", default)]
    pub version: u8,
    #[serde(default)]
    pub accepts: Vec<Accept>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<Resource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Accept {
    pub scheme: String,
    pub network: String,
    pub amount: String,
    pub asset: String,
    #[serde(rename = "payTo")]
    pub pay_to: String,
    #[serde(rename = "maxTimeoutSeconds", default)]
    pub max_timeout_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<AcceptExtra>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AcceptExtra {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Resource {
    #[serde(default)]
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

impl Challenge {
    /// Decode the base64 blob carried in `x-payment-required` (or the
    /// `requirements="…"` field of `www-authenticate`).
    pub fn from_header_b64(value: &str) -> Result<Self> {
        let blob = extract_requirements(value);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(blob.trim())
            .map_err(|e| BlockRunError::Challenge(format!("base64: {e}")))?;
        serde_json::from_slice(&bytes).map_err(|e| BlockRunError::Challenge(format!("json: {e}")))
    }

    /// Decode a challenge already in JSON form (the 402 response body also
    /// carries the same shape).
    pub fn from_json(value: &serde_json::Value) -> Result<Self> {
        serde_json::from_value(value.clone())
            .map_err(|e| BlockRunError::Challenge(format!("json: {e}")))
    }

    /// The first option matching `network` (CAIP-2), else the first option.
    pub fn pick(&self, network: Option<&str>) -> Option<&Accept> {
        match network {
            Some(n) => self
                .accepts
                .iter()
                .find(|a| a.network == n)
                .or_else(|| self.accepts.first()),
            None => self.accepts.first(),
        }
    }
}

impl Accept {
    /// Atomic `amount` projected to USDC (6 decimals).
    pub fn amount_usdc(&self) -> f64 {
        self.amount.parse::<u128>().unwrap_or(0) as f64 / 10u128.pow(USDC_DECIMALS) as f64
    }

    /// Convert into the requirements a `covenant_x402::Signer` consumes.
    pub fn to_requirements(&self) -> PaymentRequirements {
        PaymentRequirements {
            network: self.network.clone(),
            asset: self.asset.clone(),
            amount: self.amount.clone(),
            amount_usdc: self.amount_usdc(),
            pay_to: self.pay_to.clone(),
            scheme: self.scheme.clone(),
            extra: self.extra.as_ref().map(|e| PaymentExtra {
                fee_payer: None,
                name: e.name.clone(),
                version: e.version.clone(),
            }),
        }
    }
}

/// Pull the base64 payload out of a `www-authenticate: X402 requirements="…"`
/// header, or return the input unchanged when it is already the bare blob (the
/// `x-payment-required` header carries it directly).
fn extract_requirements(value: &str) -> String {
    if let Some(idx) = value.find("requirements=") {
        let rest = &value[idx + "requirements=".len()..];
        return rest.trim().trim_matches('"').to_string();
    }
    value.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The exact base64 BlockRun returns for a default gpt-4o-mini 402.
    const LIVE_B64: &str = "eyJ4NDAyVmVyc2lvbiI6MiwiYWNjZXB0cyI6W3sic2NoZW1lIjoiZXhhY3QiLCJuZXR3b3JrIjoiZWlwMTU1Ojg0NTMiLCJhbW91bnQiOiIzMDAwIiwiYXNzZXQiOiIweDgzMzU4OWZDRDZlRGI2RTA4ZjRjN0MzMkQ0ZjcxYjU0YmRBMDI5MTMiLCJwYXlUbyI6IjB4ZTkwMzAwMTRGNURBZTIxN2QwQTE1MmYwMkEwNDM1NjdiMTZjMWFCZiIsIm1heFRpbWVvdXRTZWNvbmRzIjozMDAsImV4dHJhIjp7Im5hbWUiOiJVU0QgQ29pbiIsInZlcnNpb24iOiIyIn19XSwicmVzb3VyY2UiOnsidXJsIjoiaHR0cHM6Ly9ibG9ja3J1bi5haS9hcGkvdjEvY2hhdC9jb21wbGV0aW9ucyIsImRlc2NyaXB0aW9uIjoiR1BULTRvIE1pbmkgQVBJIGNhbGwgKH4xNyBpbnB1dCwgODE5MiBtYXggb3V0cHV0IHRva2VucykiLCJtaW1lVHlwZSI6ImFwcGxpY2F0aW9uL2pzb24ifX0=";

    #[test]
    fn decodes_live_header() {
        let c = Challenge::from_header_b64(LIVE_B64).unwrap();
        assert_eq!(c.version, 2);
        let a = c.pick(Some("eip155:8453")).unwrap();
        assert_eq!(a.scheme, "exact");
        assert_eq!(a.amount, "3000");
        assert_eq!(a.asset, "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
        assert_eq!(a.pay_to, "0xe9030014F5DAe217d0A152f02A043567b16c1aBf");
        assert_eq!(a.max_timeout_seconds, 300);
        assert_eq!(a.extra.as_ref().unwrap().name.as_deref(), Some("USD Coin"));
    }

    #[test]
    fn amount_projects_to_usdc() {
        let c = Challenge::from_header_b64(LIVE_B64).unwrap();
        // 3000 atomic / 1e6 = 0.003 USDC
        assert!((c.accepts[0].amount_usdc() - 0.003).abs() < 1e-9);
    }

    #[test]
    fn accept_converts_to_requirements() {
        let c = Challenge::from_header_b64(LIVE_B64).unwrap();
        let req = c.accepts[0].to_requirements();
        assert_eq!(req.network, "eip155:8453");
        assert_eq!(req.amount, "3000");
        assert_eq!(req.pay_to, "0xe9030014F5DAe217d0A152f02A043567b16c1aBf");
        assert_eq!(req.extra.unwrap().name.as_deref(), Some("USD Coin"));
    }

    #[test]
    fn extracts_from_www_authenticate() {
        let hdr = format!("X402 requirements=\"{LIVE_B64}\"");
        let c = Challenge::from_header_b64(&hdr).unwrap();
        assert_eq!(c.accepts.len(), 1);
    }

    #[test]
    fn rejects_garbage() {
        assert!(Challenge::from_header_b64("not base64 !!!").is_err());
    }
}
