//! MCP tools. `blockrun.call` pays a BlockRun endpoint over x402 and returns the
//! response with its receipt attached; `blockrun.verify` re-checks a receipt's
//! digest and verdict without touching the network.

use std::sync::Arc;

use async_trait::async_trait;
use covenant_mcp::{Content, Tool, ToolCallResult, ToolError};
use serde_json::{json, Value};

use crate::client::BlockRunClient;
use crate::config::BlockRunConfig;
use crate::receipt::CallReceipt;

pub const PROVIDER: &str = "blockrun";
pub const CALL_TOOL: &str = "blockrun.call";
pub const VERIFY_TOOL: &str = "blockrun.verify";

pub fn blockrun_tools(client: Arc<BlockRunClient>, cfg: &BlockRunConfig) -> Vec<Arc<dyn Tool>> {
    if !cfg.enabled {
        return Vec::new();
    }
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    if cfg.allows(CALL_TOOL) {
        tools.push(Arc::new(CallTool { client }));
    }
    if cfg.allows(VERIFY_TOOL) {
        tools.push(Arc::new(VerifyTool));
    }
    tools
}

struct CallTool {
    client: Arc<BlockRunClient>,
}

#[async_trait]
impl Tool for CallTool {
    fn name(&self) -> &str {
        CALL_TOOL
    }

    fn description(&self) -> &str {
        "Call a BlockRun endpoint, paying its x402 USDC charge, and return the response with a Covenant receipt binding the requested model, the model served, the input/output hashes, and the settlement."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "endpoint": {"type": "string", "description": "BlockRun path, e.g. /v1/chat/completions"},
                "body": {"type": "object", "description": "JSON request body for the endpoint"}
            },
            "required": ["endpoint", "body"],
            "additionalProperties": false
        })
    }

    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        let endpoint = require_str(&arguments, "endpoint")?;
        // A relative path only. This reaches a payment-signing path, so an
        // absolute URL or an authority-injecting `@` must not redirect the call
        // to an attacker-controlled host.
        if !endpoint.starts_with('/') || endpoint.contains("//") || endpoint.contains('@') {
            return Err(ToolError::InvalidArguments(
                "endpoint must be a path beginning with '/'".into(),
            ));
        }
        let body = arguments
            .get("body")
            .cloned()
            .filter(Value::is_object)
            .ok_or_else(|| ToolError::InvalidArguments("body must be a JSON object".into()))?;

        match self.client.call(&endpoint, body).await {
            Ok(paid) => Ok(ToolCallResult::ok(vec![
                Content::json(paid.response),
                Content::json(json!({ "receipt": paid.receipt.to_json() })),
            ])),
            Err(e) => Ok(ToolCallResult::error(e.to_string())),
        }
    }
}

struct VerifyTool;

#[async_trait]
impl Tool for VerifyTool {
    fn name(&self) -> &str {
        VERIFY_TOOL
    }

    fn description(&self) -> &str {
        "Verify a BlockRun call receipt: recompute its digest and check the stated verdict matches the requested/served model pair. No network, no payment."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "receipt": {"type": "object", "description": "A receipt emitted by blockrun.call"},
                "expectedDigest": {"type": "string", "description": "Optional digest to check the receipt against"}
            },
            "required": ["receipt"],
            "additionalProperties": false
        })
    }

    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        let raw = arguments
            .get("receipt")
            .cloned()
            .ok_or_else(|| ToolError::InvalidArguments("receipt is required".into()))?;
        let receipt: CallReceipt = serde_json::from_value(raw)
            .map_err(|e| ToolError::InvalidArguments(format!("not a receipt: {e}")))?;

        let digest = receipt.digest();
        let expected_verdict = receipt.expected_verdict();
        let verdict_consistent = expected_verdict == receipt.verdict;
        let digest_matches = arguments
            .get("expectedDigest")
            .and_then(Value::as_str)
            .map(|e| e == digest);
        let valid = verdict_consistent && digest_matches.unwrap_or(true);

        Ok(ToolCallResult::ok(vec![Content::json(json!({
            "valid": valid,
            "digest": digest,
            "digestMatches": digest_matches,
            "verdictConsistent": verdict_consistent,
            "statedVerdict": receipt.verdict,
            "expectedVerdict": expected_verdict,
        }))]))
    }
}

fn require_str(args: &Value, key: &str) -> Result<String, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ToolError::InvalidArguments(format!("{key} is required")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receipt::{PaymentInfo, RoutingClaim};

    fn sample_receipt() -> CallReceipt {
        let req = json!({"model": "gpt-4o-mini", "messages": []});
        let resp = json!({"model": "gpt-4o-mini", "choices": []});
        CallReceipt::from_exchange(
            "/v1/chat/completions",
            &req,
            &resp,
            PaymentInfo {
                network: "eip155:8453".into(),
                asset: "0x8335".into(),
                amount: "3000".into(),
                amount_usdc: 0.003,
                pay_to: "0xe903".into(),
                tx: Some("0xabc".into()),
            },
            RoutingClaim::default(),
        )
    }

    #[test]
    fn tools_gate_on_enabled() {
        let cfg = BlockRunConfig::default();
        let client = Arc::new(BlockRunClient::new(
            "http://x",
            Arc::new(covenant_x402::MockSigner),
        ));
        assert!(blockrun_tools(client, &cfg).is_empty());
    }

    #[test]
    fn enabled_registers_both_tools() {
        let cfg = BlockRunConfig {
            enabled: true,
            ..Default::default()
        };
        let client = Arc::new(BlockRunClient::new(
            "http://x",
            Arc::new(covenant_x402::MockSigner),
        ));
        let names: Vec<_> = blockrun_tools(client, &cfg)
            .iter()
            .map(|t| t.name().to_string())
            .collect();
        assert_eq!(names, vec![CALL_TOOL, VERIFY_TOOL]);
    }

    #[tokio::test]
    async fn call_rejects_a_non_path_endpoint() {
        // Validated before any network call, so no server is needed.
        let client = Arc::new(BlockRunClient::new(
            "http://127.0.0.1:1",
            Arc::new(covenant_x402::MockSigner),
        ));
        let tool = CallTool { client };
        for bad in [
            "v1/chat",
            "https://evil.com/x",
            "/@evil.com/x",
            "//evil.com",
        ] {
            let out = tool.call(json!({"endpoint": bad, "body": {}})).await;
            assert!(
                matches!(out, Err(ToolError::InvalidArguments(_))),
                "expected rejection for {bad:?}"
            );
        }
    }

    #[tokio::test]
    async fn verify_accepts_a_clean_receipt() {
        let r = sample_receipt();
        let digest = r.digest();
        let out = VerifyTool
            .call(json!({"receipt": r.to_json(), "expectedDigest": digest}))
            .await
            .unwrap();
        let v = match &out.content[0] {
            Content::Json { value } => value.clone(),
            _ => panic!("expected json"),
        };
        assert_eq!(v["valid"], json!(true));
        assert_eq!(v["verdictConsistent"], json!(true));
        assert_eq!(v["digestMatches"], json!(true));
    }

    #[tokio::test]
    async fn verify_catches_a_tampered_verdict() {
        let mut r = sample_receipt();
        r.verdict = "delivered".into();
        r.model_served = "cheaper-model".into(); // now inconsistent with the stated verdict
        let out = VerifyTool
            .call(json!({"receipt": r.to_json()}))
            .await
            .unwrap();
        let v = match &out.content[0] {
            Content::Json { value } => value.clone(),
            _ => panic!("expected json"),
        };
        assert_eq!(v["verdictConsistent"], json!(false));
        assert_eq!(v["expectedVerdict"], json!("substituted"));
    }

    #[tokio::test]
    async fn verify_rejects_non_receipt() {
        let out = VerifyTool.call(json!({"receipt": {"foo": 1}})).await;
        assert!(matches!(out, Err(ToolError::InvalidArguments(_))));
    }
}
