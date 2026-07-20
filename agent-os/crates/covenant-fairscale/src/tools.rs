//! FairScale MCP tools.
//!
//! One read-only tool, `fairscale.score`, returning a wallet's FairScore as a
//! labeled soft signal, with a [`crate::provenance::ReadProvenance`] block
//! returned alongside the result so whoever holds the response can verify the
//! read after the fact. It never moves funds. The result
//! is tagged third-party/soft so a consumer never mistakes it for a
//! Covenant-verified fact: FairScale's score is off-chain and unsigned, and its
//! agent `work_history` pillar is partly derived from Covenant's own exported
//! data, which makes the composite a routing prior, not a correctness check.

use std::sync::Arc;

use async_trait::async_trait;
use covenant_mcp::{Content, Tool, ToolCallResult, ToolError};
use serde_json::{json, Value};

use crate::client::FairScaleClient;
use crate::config::FairScaleConfig;
use crate::provenance::ReadProvenance;

pub const PROVIDER: &str = "fairscale";
pub const SCORE_TOOL: &str = "fairscale.score";

/// Stamped on every FairScale result, so the score is never mistaken for a
/// Covenant-verified fact.
pub const TRUST_LABEL: &str = "fairscale-attested (third-party REST), soft signal";

/// Forwarded to the trust-gate read; FairScale's documented default is 40.
const TRUST_GATE_MIN_SCORE: u32 = 60;

/// Draw size the advisory credit read is underwritten against, echoed in the
/// output so the returned terms are interpretable.
const CREDIT_PROBE_USD: u64 = 1000;

/// Build the FairScale tool set. Empty when disabled.
pub fn fairscale_tools(client: Arc<FairScaleClient>, cfg: &FairScaleConfig) -> Vec<Arc<dyn Tool>> {
    if !cfg.enabled {
        return Vec::new();
    }
    vec![Arc::new(ScoreTool {
        client,
        include_trust_gate: cfg.include_trust_gate,
        credit_enabled: cfg.credit_enabled,
    }) as Arc<dyn Tool>]
}

struct ScoreTool {
    client: Arc<FairScaleClient>,
    include_trust_gate: bool,
    credit_enabled: bool,
}

#[async_trait]
impl Tool for ScoreTool {
    fn name(&self) -> &str {
        SCORE_TOOL
    }

    fn description(&self) -> &str {
        "Read a wallet's FairScale reputation signal: FairScore (0-100), tier, and optionally the \
         agent trust-gate decision. A third-party soft signal to weigh alongside Covenant's own \
         audit-derived reputation, not a verified fact. Each read carries a Covenant provenance \
         record so the read itself is verifiable. Read-only, no funds move."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pubkey": { "type": "string", "description": "The wallet's Solana pubkey (base58)." }
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
            .ok_or_else(|| {
                ToolError::InvalidArguments("missing or empty string \"pubkey\"".into())
            })?;
        if !is_base58_pubkey(pubkey) {
            return Err(ToolError::InvalidArguments(
                "\"pubkey\" must be a base58 Solana address".into(),
            ));
        }

        let score = match self.client.score(pubkey).await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(pubkey, error = %e, "fairscale score read failed");
                return Ok(ToolCallResult::error(e.to_string()));
            }
        };

        let mut summary = json!({
            "provider": PROVIDER,
            "trust": TRUST_LABEL,
            "pubkey": pubkey,
            "fairscore": score.fairscore(),
            "tier": score.tier(),
        });

        // The trust-gate is a separate agent-host read; a failure degrades to
        // the reputation signal alone, never a tool error.
        if self.include_trust_gate {
            match self.client.trust_gate(pubkey, TRUST_GATE_MIN_SCORE).await {
                Ok(gate) => {
                    summary["trustGate"] = json!({
                        "decision": gate.decision,
                        "recommendation": gate.recommendation,
                        "reasons": gate.reasons,
                        "verification": gate.verification,
                    });
                }
                Err(e) => tracing::debug!(
                    pubkey,
                    error = %e,
                    "fairscale trust-gate read failed; reputation signal only"
                ),
            }
        }

        // The credit read is heavier and advisory only; gated off by default.
        if self.credit_enabled {
            match self.client.credit(pubkey, CREDIT_PROBE_USD).await {
                Ok(credit) => {
                    summary["credit"] = json!({
                        "amountUsd": CREDIT_PROBE_USD,
                        "creditScore": credit.credit_score(),
                        "riskBand": credit.risk_band(),
                        "lendingTerms": credit.lending_terms(),
                    });
                }
                Err(e) => tracing::debug!(
                    pubkey,
                    error = %e,
                    "fairscale credit read failed; reputation signal only"
                ),
            }
        }

        // Binds the /score response only; trustGate/credit are best-effort
        // side reads and carry no anchor.
        let provenance = ReadProvenance::new(pubkey, "/score", &score.raw);
        Ok(ToolCallResult::ok(vec![
            Content::json(summary),
            Content::json(json!({ "provenance": provenance.to_json() })),
            Content::json(json!({ "raw": score.raw })),
        ]))
    }
}

/// A Solana address is base58 (Bitcoin alphabet, no `0` `O` `I` `l`) and 32-44
/// chars encoded. Validate at the tool boundary so an agent-supplied string
/// can't smuggle `/`, `?`, `#`, or `..` into the request URL.
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
        let c = Arc::new(FairScaleClient::new("http://x", "http://x"));
        assert!(fairscale_tools(c, &FairScaleConfig::default()).is_empty());
    }

    #[test]
    fn enabled_config_registers_score_tool() {
        let cfg = FairScaleConfig {
            enabled: true,
            ..Default::default()
        };
        let tools = fairscale_tools(Arc::new(FairScaleClient::new("http://x", "http://x")), &cfg);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), SCORE_TOOL);
    }

    #[tokio::test]
    async fn score_tool_returns_labeled_signal_with_provenance() {
        let server = MockServer::start().await;
        let pubkey = "EnteGjokMnFqTDcZSBitXDQEctMCnqV33HbPKw2LnDCg";
        Mock::given(method("GET"))
            .and(path("/score"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "wallet": pubkey, "fairscore": 65.3, "tier": "gold"
            })))
            .mount(&server)
            .await;
        let cfg = FairScaleConfig {
            enabled: true,
            reputation_base_url: server.uri(),
            agent_base_url: server.uri(),
            ..Default::default()
        };
        let client = Arc::new(FairScaleClient::new(server.uri(), server.uri()));
        let tools = fairscale_tools(client, &cfg);
        let res = tools[0].call(json!({ "pubkey": pubkey })).await.unwrap();
        assert!(!res.is_error);

        let summary = match &res.content[0] {
            Content::Json { value } => value.clone(),
            other => panic!("expected json, got {other:?}"),
        };
        assert_eq!(summary["fairscore"], 65.3);
        assert_eq!(summary["tier"], "gold");
        assert_eq!(summary["trust"], TRUST_LABEL);

        let prov = match &res.content[1] {
            Content::Json { value } => value.clone(),
            other => panic!("expected json, got {other:?}"),
        };
        assert_eq!(prov["provenance"]["provider"], PROVIDER);
        assert_eq!(prov["provenance"]["wallet"], pubkey);
        assert_eq!(
            prov["provenance"]["responseSha256"].as_str().unwrap().len(),
            64
        );
    }

    #[tokio::test]
    async fn trust_gate_surfaces_reasons_and_degrades_on_failure() {
        let server = MockServer::start().await;
        let pubkey = "EnteGjokMnFqTDcZSBitXDQEctMCnqV33HbPKw2LnDCg";
        Mock::given(method("GET"))
            .and(path("/score"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "wallet": pubkey, "fairscore": 24.0, "tier": "bronze"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/trust-gate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "decision": "deny", "recommendation": "unverified",
                "reasons": ["score_below_threshold: 24 < 60"],
                "verification": { "sati": true }
            })))
            .mount(&server)
            .await;
        let cfg = FairScaleConfig {
            enabled: true,
            include_trust_gate: true,
            ..Default::default()
        };
        let tools = fairscale_tools(
            Arc::new(FairScaleClient::new(server.uri(), server.uri())),
            &cfg,
        );
        let res = tools[0].call(json!({ "pubkey": pubkey })).await.unwrap();
        let summary = match &res.content[0] {
            Content::Json { value } => value.clone(),
            other => panic!("expected json, got {other:?}"),
        };
        assert_eq!(summary["trustGate"]["decision"], "deny");
        assert_eq!(
            summary["trustGate"]["reasons"][0],
            "score_below_threshold: 24 < 60"
        );

        // Agent host unreachable: the read degrades to the score alone.
        let bare = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/score"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "fairscore": 24.0 })))
            .mount(&bare)
            .await;
        let tools = fairscale_tools(Arc::new(FairScaleClient::new(bare.uri(), bare.uri())), &cfg);
        let res = tools[0].call(json!({ "pubkey": pubkey })).await.unwrap();
        assert!(!res.is_error);
        let summary = match &res.content[0] {
            Content::Json { value } => value.clone(),
            other => panic!("expected json, got {other:?}"),
        };
        assert!(summary.get("trustGate").is_none());
    }

    #[tokio::test]
    async fn score_tool_rejects_non_base58_pubkey() {
        let cfg = FairScaleConfig {
            enabled: true,
            ..Default::default()
        };
        let tools = fairscale_tools(Arc::new(FairScaleClient::new("http://x", "http://x")), &cfg);
        let len_ok_with_slash = format!("{}/", "1".repeat(43));
        for bad in [
            "../../admin",
            "x?wallet=evil",
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

    #[tokio::test]
    async fn score_tool_requires_pubkey() {
        let cfg = FairScaleConfig {
            enabled: true,
            ..Default::default()
        };
        let tools = fairscale_tools(Arc::new(FairScaleClient::new("http://x", "http://x")), &cfg);
        let err = tools[0].call(json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }
}
