//! PayAI x402 facilitator client — **diagnostic only**.
//!
//! On the production hot path Hyre's own middleware is what calls
//! `https://facilitator.payai.network/verify` and `/settle`; the client
//! never POSTs there. This module exists so the
//! `live_payai_verify_smoke` and `live_facilitator_smoke` examples can
//! probe the facilitator directly — useful when production calls fail
//! and we need to isolate "our envelope is wrong" from "Hyre's
//! middleware is wrong". Do not wire this into `execute_paid`.
//!
//! Both endpoints answer HTTP 200 even on validation failure — branch
//! on `isValid` / `success`, not status.

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::config::FACILITATOR;
use crate::{HyreError, Result};

const X402_VERSION_V1: u32 = 1;
const SCHEME_EXACT: &str = "exact";

/// `paymentRequirements` field on PayAI's wire. Mainline x402 shape —
/// uses `maxAmountRequired` (not the xona `amount`/`amountUsdc` pair)
/// and carries the sponsor's `feePayer` in `extra`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FacilitatorRequirements {
    pub scheme: String,
    pub network: String,
    #[serde(rename = "maxAmountRequired")]
    pub max_amount_required: String,
    pub resource: String,
    pub description: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    #[serde(rename = "outputSchema")]
    pub output_schema: serde_json::Value,
    #[serde(rename = "payTo")]
    pub pay_to: String,
    #[serde(rename = "maxTimeoutSeconds")]
    pub max_timeout_seconds: u32,
    pub asset: String,
    pub extra: FacilitatorExtra,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FacilitatorExtra {
    #[serde(rename = "feePayer")]
    pub fee_payer: String,
}

impl FacilitatorRequirements {
    /// Translates a `covenant_x402::PaymentRequirements` (xona-style)
    /// into the mainline shape PayAI expects, supplying the fields the
    /// inner type doesn't carry. `resource` is typically the called
    /// URL; `description` comes from the Hyre manifest entry.
    pub fn from_x402(
        req: &covenant_x402::PaymentRequirements,
        resource: impl Into<String>,
        description: impl Into<String>,
        fee_payer: impl Into<String>,
    ) -> Self {
        Self {
            scheme: SCHEME_EXACT.into(),
            network: req.network.clone(),
            max_amount_required: req.amount.clone(),
            resource: resource.into(),
            description: description.into(),
            mime_type: "application/json".into(),
            output_schema: serde_json::json!({}),
            pay_to: req.pay_to.clone(),
            max_timeout_seconds: 30,
            asset: req.asset.clone(),
            extra: FacilitatorExtra { fee_payer: fee_payer.into() },
        }
    }
}

/// `paymentPayload` envelope. `payload.transaction` is base64 of a
/// bincode-serialized v0 `VersionedTransaction`, funder-signed only —
/// the fee-payer signature slot is left for PayAI to fill at settle
/// time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FacilitatorPayload {
    #[serde(rename = "x402Version")]
    pub x402_version: u32,
    pub scheme: String,
    pub network: String,
    pub payload: FacilitatorTxEnvelope,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FacilitatorTxEnvelope {
    pub transaction: String,
}

impl FacilitatorPayload {
    pub fn new(network: impl Into<String>, transaction_b64: impl Into<String>) -> Self {
        Self {
            x402_version: X402_VERSION_V1,
            scheme: SCHEME_EXACT.into(),
            network: network.into(),
            payload: FacilitatorTxEnvelope { transaction: transaction_b64.into() },
        }
    }
}

/// Identical request body for `/verify` and `/settle`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FacilitatorRequest {
    #[serde(rename = "x402Version")]
    pub x402_version: u32,
    #[serde(rename = "paymentPayload")]
    pub payment_payload: FacilitatorPayload,
    #[serde(rename = "paymentRequirements")]
    pub payment_requirements: FacilitatorRequirements,
}

impl FacilitatorRequest {
    pub fn new(payload: FacilitatorPayload, requirements: FacilitatorRequirements) -> Self {
        Self {
            x402_version: X402_VERSION_V1,
            payment_payload: payload,
            payment_requirements: requirements,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct VerifyResponse {
    #[serde(rename = "isValid")]
    pub is_valid: bool,
    #[serde(rename = "invalidReason", default)]
    pub invalid_reason: Option<String>,
    #[serde(rename = "invalidMessage", default)]
    pub invalid_message: Option<String>,
    #[serde(default)]
    pub payer: Option<String>,
}

/// `/settle` response. `transaction` is the on-chain signature on
/// success, empty string on failure.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SettleResponse {
    pub success: bool,
    #[serde(rename = "errorReason", default)]
    pub error_reason: Option<String>,
    #[serde(rename = "errorMessage", default)]
    pub error_message: Option<String>,
    #[serde(default)]
    pub payer: Option<String>,
    pub transaction: String,
    pub network: String,
}

pub struct FacilitatorClient {
    base_url: String,
    http: reqwest::Client,
}

impl FacilitatorClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self { base_url: FACILITATOR.into(), http }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub async fn verify(&self, req: &FacilitatorRequest) -> Result<VerifyResponse> {
        let resp: VerifyResponse = self.post("/verify", req).await?;
        debug!(is_valid = resp.is_valid, reason = ?resp.invalid_reason, "facilitator /verify");
        Ok(resp)
    }

    pub async fn settle(&self, req: &FacilitatorRequest) -> Result<SettleResponse> {
        let resp: SettleResponse = self.post("/settle", req).await?;
        debug!(success = resp.success, sig = %resp.transaction, "facilitator /settle");
        Ok(resp)
    }

    async fn post<R: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        req: &FacilitatorRequest,
    ) -> Result<R> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.http.post(&url).json(req).send().await?;
        let status = resp.status();
        let bytes = resp.bytes().await?;
        if !status.is_success() {
            return Err(HyreError::Execute(format!(
                "facilitator {path}: HTTP {status} — {}",
                String::from_utf8_lossy(&bytes)
            )));
        }
        serde_json::from_slice(&bytes).map_err(|e| {
            HyreError::Execute(format!(
                "facilitator {path} decode: {e} — body: {}",
                String::from_utf8_lossy(&bytes)
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requirements_serialize_with_mainline_field_names() {
        let req = FacilitatorRequirements {
            scheme: "exact".into(),
            network: "solana".into(),
            max_amount_required: "80000".into(),
            resource: "https://mpp.hyreagent.fun/defi/tvl".into(),
            description: "TVL snapshot".into(),
            mime_type: "application/json".into(),
            output_schema: serde_json::json!({}),
            pay_to: "7G73PLhKvAPBGTzG5ESAE4coE7QrVeTTKfhTxQZbyGgC".into(),
            max_timeout_seconds: 30,
            asset: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".into(),
            extra: FacilitatorExtra {
                fee_payer: "2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4".into(),
            },
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["maxAmountRequired"], "80000");
        assert_eq!(json["payTo"], "7G73PLhKvAPBGTzG5ESAE4coE7QrVeTTKfhTxQZbyGgC");
        assert_eq!(json["mimeType"], "application/json");
        assert_eq!(json["outputSchema"], serde_json::json!({}));
        assert_eq!(json["maxTimeoutSeconds"], 30);
        assert_eq!(json["extra"]["feePayer"], "2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4");
        assert!(json.get("amount").is_none(), "must not emit xona-style amount");
        assert!(json.get("amountUsdc").is_none());
    }

    #[test]
    fn request_wraps_payload_and_requirements_with_x402_version() {
        let req = FacilitatorRequest::new(
            FacilitatorPayload::new("solana", "BASE64TX"),
            FacilitatorRequirements {
                scheme: "exact".into(),
                network: "solana".into(),
                max_amount_required: "1".into(),
                resource: "r".into(),
                description: "d".into(),
                mime_type: "application/json".into(),
                output_schema: serde_json::json!({}),
                pay_to: "p".into(),
                max_timeout_seconds: 30,
                asset: "a".into(),
                extra: FacilitatorExtra { fee_payer: "f".into() },
            },
        );
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["x402Version"], 1);
        assert_eq!(json["paymentPayload"]["scheme"], "exact");
        assert_eq!(json["paymentPayload"]["network"], "solana");
        assert_eq!(json["paymentPayload"]["payload"]["transaction"], "BASE64TX");
        assert_eq!(json["paymentRequirements"]["maxAmountRequired"], "1");
    }

    #[test]
    fn verify_response_decodes_invalid_with_reason() {
        let body = r#"{"isValid":false,"invalidReason":"invalid_exact_svm_payload_transaction_could_not_be_decoded","payer":""}"#;
        let resp: VerifyResponse = serde_json::from_str(body).unwrap();
        assert!(!resp.is_valid);
        assert_eq!(resp.invalid_reason.as_deref(), Some("invalid_exact_svm_payload_transaction_could_not_be_decoded"));
        assert_eq!(resp.payer.as_deref(), Some(""));
    }

    #[test]
    fn settle_response_decodes_success_with_signature() {
        let body = r#"{"success":true,"payer":"7G73…","transaction":"5j7s…sig","network":"solana"}"#;
        let resp: SettleResponse = serde_json::from_str(body).unwrap();
        assert!(resp.success);
        assert_eq!(resp.transaction, "5j7s…sig");
        assert_eq!(resp.network, "solana");
    }

    #[test]
    fn from_x402_translates_amount_to_max_amount_required() {
        let src = covenant_x402::PaymentRequirements {
            network: "solana".into(),
            asset: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".into(),
            amount: "80000".into(),
            amount_usdc: 0.08,
            pay_to: "7G73PLhKvAPBGTzG5ESAE4coE7QrVeTTKfhTxQZbyGgC".into(),
            scheme: "exact".into(),
            extra: None,
        };
        let req = FacilitatorRequirements::from_x402(
            &src,
            "https://mpp.hyreagent.fun/defi/tvl",
            "TVL snapshot",
            "2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4",
        );
        assert_eq!(req.max_amount_required, "80000");
        assert_eq!(req.asset, src.asset);
        assert_eq!(req.pay_to, src.pay_to);
        assert_eq!(req.extra.fee_payer, "2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4");
    }
}
