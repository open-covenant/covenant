//! Krexa MCP tools.
//!
//! One read-only tool, `krexa.score`, returning an agent's Krexit score
//! and underwriting as a labeled soft signal. It never moves funds and
//! never touches the gated credit path. The result is explicitly tagged
//! so a consumer treats it as a third-party REST signal, not a Covenant
//! attestation: the score is gameable (Krexa's SNS boost), its data is
//! shaky, and its "attestation" is a hash, not a signature.

use std::sync::Arc;

use async_trait::async_trait;
use covenant_mcp::{Content, Tool, ToolCallResult, ToolError};
use serde_json::{json, Value};

use crate::client::KrexaClient;
use crate::config::KrexaConfig;

pub const PROVIDER: &str = "krexa";
pub const SCORE_TOOL: &str = "krexa.score";

/// The trust label stamped on every Krexa result, so the score is never
/// mistaken for a Covenant-verified fact.
pub const TRUST_LABEL: &str = "krexa-attested (third-party REST), soft signal";

/// Build the Krexa tool set. Empty when disabled.
pub fn krexa_tools(client: Arc<KrexaClient>, cfg: &KrexaConfig) -> Vec<Arc<dyn Tool>> {
    if !cfg.enabled {
        return Vec::new();
    }
    vec![Arc::new(ScoreTool { client }) as Arc<dyn Tool>]
}

struct ScoreTool {
    client: Arc<KrexaClient>,
}

#[async_trait]
impl Tool for ScoreTool {
    fn name(&self) -> &str {
        SCORE_TOOL
    }

    fn description(&self) -> &str {
        "Read an agent's Krexa credit/risk signal: Krexit score (200-850), risk band, and \
         underwriting opinion. A third-party soft signal to weigh alongside Covenant's own \
         audit-derived reputation, not a verified fact. Read-only, no funds move."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pubkey": { "type": "string", "description": "The agent's Solana pubkey (base58)." }
            },
            "required": ["pubkey"],
            "additionalProperties": false
        })
    }

    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        let pubkey = arguments
            .get("pubkey")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::InvalidArguments("missing string \"pubkey\"".into()))?;
        if !is_base58_pubkey(pubkey) {
            return Err(ToolError::InvalidArguments(
                "\"pubkey\" must be a base58 Solana address".into(),
            ));
        }

        match self.client.score(pubkey).await {
            Ok(s) => Ok(ToolCallResult::ok(vec![
                Content::json(json!({
                    "provider": PROVIDER,
                    "trust": TRUST_LABEL,
                    "pubkey": pubkey,
                    "score": s.score(),
                    "creditLevel": s.credit_level(),
                    "riskBand": s.risk_band(),
                    "registered": s.registered(),
                    "snsBoostApplied": s.sns_boost_applied(),
                    "recommendation": s.recommendation(),
                    "attestationHash": s.attestation_hash(),
                })),
                Content::json(json!({ "raw": s.raw })),
            ])),
            Err(e) => {
                tracing::debug!(pubkey, error = %e, "krexa score read failed");
                Ok(ToolCallResult::error(e.to_string()))
            }
        }
    }
}

/// A Solana address is base58 (Bitcoin alphabet, no `0` `O` `I` `l`) and
/// 32-44 chars once encoded. Validate at the tool boundary so an
/// agent-supplied string can't smuggle `/`, `?`, `#`, or `..` into the
/// request path.
fn is_base58_pubkey(s: &str) -> bool {
    (32..=44).contains(&s.len())
        && s.bytes().all(|b| {
            matches!(b, b'1'..=b'9' | b'A'..=b'H' | b'J'..=b'N' | b'P'..=b'Z' | b'a'..=b'k' | b'm'..=b'z')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn disabled_config_registers_no_tools() {
        let c = Arc::new(KrexaClient::new("http://x"));
        assert!(krexa_tools(c, &KrexaConfig::default()).is_empty());
    }

    #[test]
    fn enabled_config_registers_score_tool() {
        let cfg = KrexaConfig {
            enabled: true,
            ..Default::default()
        };
        let tools = krexa_tools(Arc::new(KrexaClient::new("http://x")), &cfg);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), SCORE_TOOL);
    }

    #[tokio::test]
    async fn score_tool_returns_labeled_signal() {
        let server = MockServer::start().await;
        let pubkey = "EnteGjokMnFqTDcZSBitXDQEctMCnqV33HbPKw2LnDCg";
        Mock::given(method("GET"))
            .and(path(format!("/solana/score/{pubkey}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "score": 700, "creditLevel": 3,
                "snsBoost": { "applied": true, "boost": 15 },
                "krexit": {
                    "riskBand": "prime",
                    "underwriting": { "opinion": "Approve" },
                    "attestation": { "type": "krexa_v1", "payload_hash": "abc" }
                }
            })))
            .mount(&server)
            .await;
        let cfg = KrexaConfig {
            enabled: true,
            base_url: server.uri(),
            credit_enabled: false,
        };
        let tools = krexa_tools(Arc::new(KrexaClient::new(server.uri())), &cfg);
        let res = tools[0].call(json!({ "pubkey": pubkey })).await.unwrap();
        assert!(!res.is_error);
        let body = match &res.content[0] {
            Content::Json { value } => value.clone(),
            other => panic!("expected json, got {other:?}"),
        };
        assert_eq!(body["score"], 700);
        assert_eq!(body["snsBoostApplied"], true);
        assert_eq!(body["attestationHash"], "abc");
        assert_eq!(body["trust"], TRUST_LABEL);
    }

    #[tokio::test]
    async fn score_tool_requires_pubkey() {
        let cfg = KrexaConfig {
            enabled: true,
            ..Default::default()
        };
        let tools = krexa_tools(Arc::new(KrexaClient::new("http://x")), &cfg);
        let err = tools[0].call(json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn score_tool_rejects_non_base58_pubkey() {
        let cfg = KrexaConfig {
            enabled: true,
            ..Default::default()
        };
        let tools = krexa_tools(Arc::new(KrexaClient::new("http://x")), &cfg);
        // Path traversal, query injection, wrong length, and an in-range
        // string carrying a path separator are all refused before any
        // request leaves the host.
        let len_ok_with_slash = format!("{}/", "1".repeat(43));
        for bad in [
            "../../admin",
            "x?foo=bar",
            "short",
            len_ok_with_slash.as_str(),
        ] {
            let err = tools[0].call(json!({ "pubkey": bad })).await.unwrap_err();
            assert!(
                matches!(err, ToolError::InvalidArguments(_)),
                "expected reject for {bad:?}"
            );
        }
    }
}
