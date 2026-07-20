//! The Covenant receipt for a BlockRun paid call.
//!
//! A [`CallReceipt`] binds one x402 exchange into a single hash-addressable
//! record: the endpoint and requested model, the model BlockRun actually
//! served, the input and output hashes, the routing claim, and the settlement.
//! It is unsigned here — integrity comes from the daemon's audit chain and the
//! settlement receipt that carries the on-chain payment signature — but
//! [`CallReceipt::digest`] gives a stable hash a signer or anchor can bind to.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const PROVIDER: &str = "blockrun";

/// How the served model related to what the caller asked for.
pub const VERDICT_DELIVERED: &str = "delivered";
pub const VERDICT_SUBSTITUTED: &str = "substituted";
pub const VERDICT_UNVERIFIED: &str = "unverified";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PaymentInfo {
    pub network: String,
    pub asset: String,
    pub amount: String,
    #[serde(rename = "amountUsdc")]
    pub amount_usdc: f64,
    #[serde(rename = "payTo")]
    pub pay_to: String,
    /// The on-chain settlement signature, once known. BlockRun returns it in the
    /// `x-payment-receipt` response header after the paid retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx: Option<String>,
}

/// The routing claim, surfaced so "cheapest capable model" stops being an
/// unfalsifiable assertion: we record what was requested against what the router
/// reports it served, and any savings figure it advertises.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RoutingClaim {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(
        rename = "savingsPct",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub savings_pct: Option<f64>,
}

impl RoutingClaim {
    /// Read the routing claim ClawRouter/BlockRun expose in debug headers. Header
    /// names vary across router versions, so we read the known set defensively
    /// and leave fields `None` when absent rather than failing.
    pub fn from_headers(headers: &reqwest::header::HeaderMap) -> Self {
        let get = |names: &[&str]| -> Option<String> {
            names
                .iter()
                .find_map(|n| headers.get(*n))
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        };
        RoutingClaim {
            profile: get(&["x-clawrouter-profile", "x-blockrun-profile"]),
            model: get(&["x-clawrouter-model", "x-blockrun-model", "x-model"]),
            savings_pct: get(&["x-clawrouter-savings", "x-blockrun-savings"])
                .and_then(|s| s.trim_end_matches('%').trim().parse::<f64>().ok()),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.profile.is_none() && self.model.is_none() && self.savings_pct.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallReceipt {
    pub provider: String,
    pub endpoint: String,
    #[serde(rename = "modelRequested")]
    pub model_requested: String,
    #[serde(rename = "modelServed")]
    pub model_served: String,
    pub verdict: String,
    #[serde(rename = "inputSha256")]
    pub input_sha256: String,
    #[serde(rename = "outputSha256")]
    pub output_sha256: String,
    #[serde(default, skip_serializing_if = "RoutingClaim::is_empty")]
    pub routing: RoutingClaim,
    pub payment: PaymentInfo,
}

impl CallReceipt {
    /// Build a receipt from one exchange: the request body sent, the response
    /// body received, the payment that settled it, and the routing headers.
    pub fn from_exchange(
        endpoint: &str,
        request: &Value,
        response: &Value,
        payment: PaymentInfo,
        routing: RoutingClaim,
    ) -> Self {
        let model_requested = request
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // The router reports the served model in the response body or, failing
        // that, in the routing headers.
        let model_served = response
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| routing.model.clone())
            .unwrap_or_default();
        let verdict = verdict_for(&model_requested, &model_served);
        CallReceipt {
            provider: PROVIDER.to_string(),
            endpoint: endpoint.to_string(),
            model_requested,
            model_served,
            verdict,
            input_sha256: canonical_sha256_hex(request),
            output_sha256: canonical_sha256_hex(response),
            routing,
            payment,
        }
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// Stable hash over the JCS-canonical receipt; the identity a signer or
    /// on-chain anchor binds to.
    pub fn digest(&self) -> String {
        canonical_sha256_hex(&self.to_json())
    }

    /// The verdict the requested/served pair should carry. A stored `verdict`
    /// that disagrees with this is a tampered receipt.
    pub fn expected_verdict(&self) -> String {
        verdict_for(&self.model_requested, &self.model_served)
    }
}

fn verdict_for(requested: &str, served: &str) -> String {
    if served.is_empty() {
        VERDICT_UNVERIFIED
    } else if requested.eq_ignore_ascii_case(served) {
        VERDICT_DELIVERED
    } else {
        VERDICT_SUBSTITUTED
    }
    .to_string()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 over the JCS-canonical form so two structurally-equal JSON values
/// hash identically regardless of key order or whitespace.
pub fn canonical_sha256_hex(value: &Value) -> String {
    match serde_jcs::to_vec(value) {
        Ok(bytes) => sha256_hex(&bytes),
        Err(_) => sha256_hex(value.to_string().as_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payment() -> PaymentInfo {
        PaymentInfo {
            network: "eip155:8453".into(),
            asset: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
            amount: "3000".into(),
            amount_usdc: 0.003,
            pay_to: "0xe9030014F5DAe217d0A152f02A043567b16c1aBf".into(),
            tx: Some("0xabc".into()),
        }
    }

    #[test]
    fn delivered_when_served_matches_requested() {
        let req = json!({"model": "gpt-4o-mini", "messages": []});
        let resp = json!({"model": "gpt-4o-mini", "choices": []});
        let r = CallReceipt::from_exchange(
            "/api/v1/chat/completions",
            &req,
            &resp,
            payment(),
            RoutingClaim::default(),
        );
        assert_eq!(r.verdict, VERDICT_DELIVERED);
        assert_eq!(r.model_requested, "gpt-4o-mini");
        assert_eq!(r.model_served, "gpt-4o-mini");
    }

    #[test]
    fn substituted_when_router_swaps_the_model() {
        let req = json!({"model": "gpt-4o", "messages": []});
        let resp = json!({"model": "gpt-4o-mini", "choices": []});
        let r = CallReceipt::from_exchange("/x", &req, &resp, payment(), RoutingClaim::default());
        assert_eq!(r.verdict, VERDICT_SUBSTITUTED);
    }

    #[test]
    fn served_model_falls_back_to_routing_header() {
        let req = json!({"model": "auto", "messages": []});
        let resp = json!({"choices": []});
        let routing = RoutingClaim {
            profile: Some("auto".into()),
            model: Some("deepseek-v3".into()),
            savings_pct: Some(78.0),
        };
        let r = CallReceipt::from_exchange("/x", &req, &resp, payment(), routing);
        assert_eq!(r.model_served, "deepseek-v3");
        assert_eq!(r.routing.savings_pct, Some(78.0));
    }

    #[test]
    fn unverified_when_no_served_model_anywhere() {
        let req = json!({"model": "gpt-4o-mini"});
        let resp = json!({"choices": []});
        let r = CallReceipt::from_exchange("/x", &req, &resp, payment(), RoutingClaim::default());
        assert_eq!(r.verdict, VERDICT_UNVERIFIED);
    }

    #[test]
    fn digest_is_stable_across_key_order() {
        let req = json!({"model": "gpt-4o-mini", "messages": []});
        let resp_a = json!({"model": "gpt-4o-mini", "choices": [], "id": "x"});
        let resp_b = json!({"id": "x", "choices": [], "model": "gpt-4o-mini"});
        let ra =
            CallReceipt::from_exchange("/x", &req, &resp_a, payment(), RoutingClaim::default());
        let rb =
            CallReceipt::from_exchange("/x", &req, &resp_b, payment(), RoutingClaim::default());
        assert_eq!(ra.digest(), rb.digest());
        assert_eq!(ra.output_sha256, rb.output_sha256);
    }

    #[test]
    fn routing_parsed_from_headers() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("x-clawrouter-model", "kimi-k2".parse().unwrap());
        h.insert("x-clawrouter-savings", "63%".parse().unwrap());
        let rc = RoutingClaim::from_headers(&h);
        assert_eq!(rc.model.as_deref(), Some("kimi-k2"));
        assert_eq!(rc.savings_pct, Some(63.0));
    }
}
