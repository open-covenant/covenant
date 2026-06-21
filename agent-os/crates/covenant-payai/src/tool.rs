use std::sync::Arc;

use async_trait::async_trait;
use covenant_identity::LocalIdentity;
use covenant_mcp::{Content, Tool, ToolCallResult, ToolError, ToolSpec};
use serde_json::{json, Value};

use crate::config::PayAiConfig;
use crate::oracle::signed_reputation_for;

const REPUTATION_TOOL: &str = "payai.reputation";
const REPUTATION_DESC: &str =
    "Settlement-grounded reputation for a wallet on the PayAI x402 rail, \
signed by this Covenant daemon. Reads PayAI's public Solana settlements; never touches payments.";

fn reputation_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "wallet": {"type": "string", "description": "Solana wallet (owner) to score"},
            "limit": {"type": "integer", "description": "recent settlements to scan (optional)"}
        },
        "required": ["wallet"],
        "additionalProperties": false
    })
}

/// MCP tool: signed, settlement-grounded reputation for a PayAI wallet.
struct PayaiReputationTool {
    rpc_url: String,
    fee_payer: Option<String>,
    limit: usize,
    identity: Arc<LocalIdentity>,
}

#[async_trait]
impl Tool for PayaiReputationTool {
    fn name(&self) -> &str {
        REPUTATION_TOOL
    }
    fn description(&self) -> &str {
        REPUTATION_DESC
    }
    fn input_schema(&self) -> Value {
        reputation_schema()
    }

    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        if self.rpc_url.trim().is_empty() {
            return Ok(ToolCallResult::error(
                "payai: no RPC url configured (set COVENANT_PAYAI_RPC_URL or COVENANT_SOLANA_RPC_URL)",
            ));
        }
        let wallet = arguments
            .get("wallet")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArguments("missing string field `wallet`".into()))?;
        // Clamp the window: each settlement is an N+1 getTransaction, and
        // getSignaturesForAddress caps at 1000, so a call can't fan out unbounded.
        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(self.limit)
            .clamp(1, 1000);

        match signed_reputation_for(
            &self.rpc_url,
            self.fee_payer.as_deref(),
            wallet,
            limit,
            &self.identity,
        )
        .await
        {
            Ok((rep, signed)) => {
                let tier_label = rep.tier.label();
                let volume_usdc = rep.volume_usdc();
                Ok(ToolCallResult::ok(vec![Content::json(json!({
                    "reputation": rep,
                    "tier_label": tier_label,
                    "volume_usdc": volume_usdc,
                    "signed": signed,
                }))]))
            }
            Err(e) => Ok(ToolCallResult::error(format!(
                "payai reputation read failed: {e}"
            ))),
        }
    }
}

/// Resolve a `payai.*` tool by name. `None` when disabled or unknown.
pub fn payai_tool(
    config: &PayAiConfig,
    name: &str,
    identity: Arc<LocalIdentity>,
) -> Option<Arc<dyn Tool>> {
    if !config.enabled {
        return None;
    }
    match name {
        REPUTATION_TOOL => Some(Arc::new(PayaiReputationTool {
            rpc_url: config.rpc_url.clone(),
            fee_payer: config.fee_payer.clone(),
            limit: config.limit,
            identity,
        }) as Arc<dyn Tool>),
        _ => None,
    }
}

/// Specs for advertised `payai.*` tools (for `tools/list`).
pub fn payai_specs(config: &PayAiConfig) -> Vec<ToolSpec> {
    if !config.enabled {
        return Vec::new();
    }
    vec![ToolSpec {
        name: REPUTATION_TOOL.to_string(),
        description: REPUTATION_DESC.to_string(),
        input_schema: reputation_schema(),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_config_advertises_and_resolves_nothing() {
        let cfg = PayAiConfig::default(); // enabled: false
        assert!(payai_specs(&cfg).is_empty());
        let id = Arc::new(LocalIdentity::generate("daemon@local"));
        assert!(payai_tool(&cfg, REPUTATION_TOOL, id).is_none());
    }

    #[test]
    fn enabled_config_advertises_and_resolves_reputation() {
        let cfg = PayAiConfig {
            enabled: true,
            rpc_url: "https://rpc.example".into(),
            ..Default::default()
        };
        let specs = payai_specs(&cfg);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, REPUTATION_TOOL);

        let id = Arc::new(LocalIdentity::generate("daemon@local"));
        let tool = payai_tool(&cfg, REPUTATION_TOOL, id.clone()).expect("resolves");
        assert_eq!(tool.name(), REPUTATION_TOOL);
        assert!(payai_tool(&cfg, "payai.unknown", id).is_none());
    }

    #[tokio::test]
    async fn missing_wallet_arg_is_invalid() {
        let cfg = PayAiConfig {
            enabled: true,
            rpc_url: "https://rpc.example".into(),
            ..Default::default()
        };
        let id = Arc::new(LocalIdentity::generate("daemon@local"));
        let tool = payai_tool(&cfg, REPUTATION_TOOL, id).unwrap();
        let err = tool.call(json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }
}
