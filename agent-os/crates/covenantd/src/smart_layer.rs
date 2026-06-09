//! Optional inbound payload validator for the x402 resale surface.
//!
//! When an operator resells daemon-fronted tools, a paying caller's
//! request can be routed through this layer for an AI payload-integrity
//! / quality check *before* the gateway settles payment. It is shaped
//! against Xona's `x402-smart-layer` Express middleware, configured by
//! `{ realm, programId, connection, price, qualityGuarantee }`.
//!
//! ## Scope
//!
//! Inbound only. The outbound payment path ([`crate::x402`]) does not
//! touch this layer — nothing the daemon pays for is gated here, only
//! requests the daemon would serve and settle for a remote payer.
//!
//! ## Defaults and failure mode
//!
//! The layer is off unless an operator opts in via
//! `COVENANT_X402_SMART_LAYER_ENABLED`. Off resolves to [`NoOpValidator`]
//! (or no validator at all), so a daemon with no opt-in behaves exactly
//! as it would without this module.
//!
//! When enabled the layer is **fail-closed**: any condition that
//! prevents a verdict — a missing Gemini key, a transport error, a
//! malformed model response, or an explicit model rejection — returns
//! [`Err`] and must block settlement. An enabled-but-unconfigured
//! validator rejecting every call is the correct, honest behavior; it
//! never silently approves.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;

/// A paying caller's inbound request, presented to the validator before
/// the gateway settles. Carries enough of the request for the model to
/// judge payload integrity against the operator's quality guarantee.
#[derive(Debug, Clone)]
pub struct InboundCall<'a> {
    /// Settlement realm the resold resource belongs to (Xona's `realm`).
    pub realm: &'a str,
    /// Resource / tool slug the payer is calling.
    pub resource: &'a str,
    /// Decoded request payload, when it parsed as JSON.
    pub payload: Option<&'a Value>,
    /// Raw request body, always present even when `payload` is `None`.
    pub payload_bytes: &'a [u8],
    /// Advertised price for this call, atomic USDC.
    pub price_micro_usdc: u128,
}

/// Why the smart layer refused to clear an inbound call. Every variant
/// blocks settlement; the daemon should map this onto a payment refusal,
/// not a soft skip.
#[derive(Debug, thiserror::Error)]
pub enum SmartLayerReject {
    /// The model judged the payload below the operator's guarantee.
    #[error("payload rejected by smart layer: {0}")]
    Rejected(String),
    /// The layer is enabled but missing the Gemini key, so no verdict is
    /// possible. Fail-closed: treated as a rejection.
    #[error("smart layer enabled but unconfigured: {0}")]
    Unconfigured(&'static str),
    /// The verdict call failed to reach the model or complete.
    #[error("smart layer transport error: {0}")]
    Transport(String),
    /// The model responded but the verdict could not be parsed.
    #[error("smart layer response malformed: {0}")]
    Malformed(String),
}

/// Validates inbound resale requests before settlement. The integration
/// seam: the future inbound-resale serve path holds an
/// `Arc<dyn SmartLayerValidator>` and calls [`validate`](Self::validate)
/// before serving or settling. On `Err` it must refuse the call.
#[async_trait]
pub trait SmartLayerValidator: Send + Sync {
    async fn validate(&self, req: &InboundCall<'_>) -> Result<(), SmartLayerReject>;
}

/// The off / absent configuration. Approves every call, imposing no
/// behavior change on a daemon that has not opted in.
#[derive(Debug, Default, Clone)]
pub struct NoOpValidator;

#[async_trait]
impl SmartLayerValidator for NoOpValidator {
    async fn validate(&self, _req: &InboundCall<'_>) -> Result<(), SmartLayerReject> {
        Ok(())
    }
}

/// Gemini-backed inbound validator.
///
/// Holds the middleware config (`{ realm, programId, connection, price,
/// qualityGuarantee }`) plus the Gemini credentials. [`validate`]
/// POSTs the payload and the quality guarantee to Gemini and maps the
/// verdict onto `Ok`/`Err`.
pub struct GeminiSmartLayer {
    pub realm: String,
    pub program_id: String,
    pub connection: String,
    pub price_micro_usdc: u128,
    pub quality_guarantee: String,
    gemini_api_key: String,
    gemini_endpoint: String,
    client: reqwest::Client,
}

/// PROVISIONAL CONTRACT. Xona has not shipped the final
/// `x402-smart-layer` middleware signature, so the exact Gemini
/// request/response shape is not yet fixed. This module commits to the
/// documented config (`realm`, `programId`, `connection`, `price`,
/// `qualityGuarantee`) and a minimal verdict contract: the request
/// carries the guarantee and payload; the response is expected to expose
/// a boolean `approved` (with an optional `reason`). When Xona publishes
/// the final contract, only [`SmartLayerVerdict`] and the request body
/// in [`GeminiSmartLayer::validate`] need to change — the trait, the
/// config, and the integration seam are stable.
#[derive(Debug, Deserialize)]
struct SmartLayerVerdict {
    approved: bool,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Serialize)]
struct VerdictRequest<'a> {
    realm: &'a str,
    program_id: &'a str,
    connection: &'a str,
    resource: &'a str,
    price_micro_usdc: u128,
    quality_guarantee: &'a str,
    payload: Value,
}

impl GeminiSmartLayer {
    pub fn new(
        realm: impl Into<String>,
        program_id: impl Into<String>,
        connection: impl Into<String>,
        price_micro_usdc: u128,
        quality_guarantee: impl Into<String>,
        gemini_api_key: impl Into<String>,
        gemini_endpoint: impl Into<String>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client");
        Self {
            realm: realm.into(),
            program_id: program_id.into(),
            connection: connection.into(),
            price_micro_usdc,
            quality_guarantee: quality_guarantee.into(),
            gemini_api_key: gemini_api_key.into(),
            gemini_endpoint: gemini_endpoint.into(),
            client,
        }
    }

    fn request_body<'a>(&'a self, req: &InboundCall<'a>) -> VerdictRequest<'a> {
        let payload = req.payload.cloned().unwrap_or_else(|| {
            Value::String(String::from_utf8_lossy(req.payload_bytes).into_owned())
        });
        VerdictRequest {
            realm: &self.realm,
            program_id: &self.program_id,
            connection: &self.connection,
            resource: req.resource,
            price_micro_usdc: req.price_micro_usdc,
            quality_guarantee: &self.quality_guarantee,
            payload,
        }
    }
}

#[async_trait]
impl SmartLayerValidator for GeminiSmartLayer {
    async fn validate(&self, req: &InboundCall<'_>) -> Result<(), SmartLayerReject> {
        if self.gemini_api_key.is_empty() {
            return Err(SmartLayerReject::Unconfigured("gemini api key missing"));
        }
        if self.gemini_endpoint.is_empty() {
            return Err(SmartLayerReject::Unconfigured("gemini endpoint missing"));
        }

        let resp = self
            .client
            .post(&self.gemini_endpoint)
            .header("x-goog-api-key", &self.gemini_api_key)
            .json(&self.request_body(req))
            .send()
            .await
            .map_err(|e| SmartLayerReject::Transport(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(SmartLayerReject::Transport(format!(
                "gemini returned {}: {}",
                status.as_u16(),
                body.trim()
            )));
        }

        let verdict: SmartLayerVerdict = resp
            .json()
            .await
            .map_err(|e| SmartLayerReject::Malformed(e.to_string()))?;

        if verdict.approved {
            Ok(())
        } else {
            Err(SmartLayerReject::Rejected(
                verdict.reason.unwrap_or_else(|| "no reason given".into()),
            ))
        }
    }
}

fn parse_bool(value: Option<&str>) -> bool {
    match value.map(str::trim).map(str::to_ascii_lowercase) {
        Some(s) => matches!(s.as_str(), "1" | "true" | "yes"),
        None => false,
    }
}

/// Resolve a validator from the process environment.
///
/// Returns `None` when `COVENANT_X402_SMART_LAYER_ENABLED` is unset or
/// falsy — the caller then skips the check (or substitutes
/// [`NoOpValidator`]), preserving today's behavior exactly. When
/// enabled, returns a [`GeminiSmartLayer`]; a missing Gemini key is
/// *not* an error here — the validator still constructs and rejects
/// fail-closed at call time, so an operator who flips the switch without
/// finishing configuration gets refusals rather than silent approvals.
pub fn from_env() -> Option<Arc<dyn SmartLayerValidator>> {
    from_env_map(std::env::vars())
}

fn from_env_map<I, K, V>(env: I) -> Option<Arc<dyn SmartLayerValidator>>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    use std::collections::HashMap;
    let map: HashMap<String, String> = env
        .into_iter()
        .map(|(k, v)| (k.as_ref().to_string(), v.as_ref().to_string()))
        .collect();
    let get = |key: &str| map.get(key).map(String::as_str);

    if !parse_bool(get("COVENANT_X402_SMART_LAYER_ENABLED")) {
        return None;
    }

    let realm = get("COVENANT_X402_SMART_LAYER_REALM").unwrap_or_default();
    let program_id = get("COVENANT_X402_SMART_LAYER_PROGRAM_ID").unwrap_or_default();
    let connection = get("COVENANT_X402_SMART_LAYER_CONNECTION").unwrap_or_default();
    let price = get("COVENANT_X402_SMART_LAYER_PRICE")
        .and_then(|s| s.trim().parse::<u128>().ok())
        .unwrap_or(0);
    let quality_guarantee = get("COVENANT_X402_SMART_LAYER_QUALITY_GUARANTEE").unwrap_or_default();
    let api_key = get("COVENANT_X402_SMART_LAYER_GEMINI_API_KEY").unwrap_or_default();
    let endpoint = get("COVENANT_X402_SMART_LAYER_GEMINI_ENDPOINT").unwrap_or_default();

    if api_key.is_empty() || endpoint.is_empty() {
        warn!("x402 smart layer enabled without a complete Gemini config; it will reject inbound calls fail-closed");
    }

    Some(Arc::new(GeminiSmartLayer::new(
        realm,
        program_id,
        connection,
        price,
        quality_guarantee,
        api_key,
        endpoint,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call<'a>(payload: &'a Value) -> InboundCall<'a> {
        InboundCall {
            realm: "covenant",
            resource: "image/creative-director",
            payload: Some(payload),
            payload_bytes: b"{}",
            price_micro_usdc: 120_000,
        }
    }

    struct RejectStub;

    #[async_trait]
    impl SmartLayerValidator for RejectStub {
        async fn validate(&self, _req: &InboundCall<'_>) -> Result<(), SmartLayerReject> {
            Err(SmartLayerReject::Rejected("stub".into()))
        }
    }

    #[tokio::test]
    async fn noop_approves() {
        let payload = json!({"prompt": "hi"});
        assert!(NoOpValidator.validate(&call(&payload)).await.is_ok());
    }

    #[tokio::test]
    async fn stub_rejects() {
        let payload = json!({"prompt": "hi"});
        let err = RejectStub
            .validate(&call(&payload))
            .await
            .expect_err("reject");
        assert!(matches!(err, SmartLayerReject::Rejected(_)));
    }

    #[test]
    fn from_env_none_when_disabled() {
        assert!(from_env_map::<_, &str, &str>([]).is_none());
        assert!(from_env_map([("COVENANT_X402_SMART_LAYER_ENABLED", "false")]).is_none());
        assert!(from_env_map([("COVENANT_X402_SMART_LAYER_ENABLED", "0")]).is_none());
    }

    #[test]
    fn from_env_some_when_enabled() {
        let v = from_env_map([
            ("COVENANT_X402_SMART_LAYER_ENABLED", "true"),
            ("COVENANT_X402_SMART_LAYER_REALM", "covenant"),
            ("COVENANT_X402_SMART_LAYER_GEMINI_API_KEY", "k"),
            (
                "COVENANT_X402_SMART_LAYER_GEMINI_ENDPOINT",
                "https://example.test/v1",
            ),
        ]);
        assert!(v.is_some());
    }

    /// Enabled but no key: must reject without ever touching the network.
    #[tokio::test]
    async fn enabled_missing_key_rejects_offline() {
        let layer = GeminiSmartLayer::new(
            "covenant",
            "prog",
            "https://api.mainnet-beta.solana.com",
            120_000,
            "must be coherent",
            "",
            "https://unreachable.invalid/should-never-be-called",
        );
        let payload = json!({"prompt": "hi"});
        let err = layer
            .validate(&call(&payload))
            .await
            .expect_err("fail-closed");
        assert!(matches!(err, SmartLayerReject::Unconfigured(_)));
    }

    #[tokio::test]
    async fn enabled_missing_endpoint_rejects_offline() {
        let layer = GeminiSmartLayer::new("covenant", "prog", "conn", 1, "guarantee", "key", "");
        let payload = json!({"prompt": "hi"});
        let err = layer
            .validate(&call(&payload))
            .await
            .expect_err("fail-closed");
        assert!(matches!(err, SmartLayerReject::Unconfigured(_)));
    }
}
