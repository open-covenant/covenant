//! Native Rust tools for the v0 registry. Real value: prove the trait shape
//! works end-to-end (with and without arguments) before wiring external MCP
//! servers.

use crate::{Content, Tool, ToolCallResult, ToolError};
use async_trait::async_trait;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

/// Echoes its `text` argument straight back. Useful as a smoke test for the
/// CLI wiring and as a reference for argument validation.
pub struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Returns the provided `text` argument verbatim."
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": { "type": "string" }
            },
            "required": ["text"],
            "additionalProperties": false
        })
    }
    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        let text = arguments
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments("missing string `text`".into()))?
            .to_string();
        Ok(ToolCallResult::ok(vec![Content::text(text)]))
    }
}

/// No-arg tool that returns the current epoch ms. Proves the schema-empty
/// path and gives the daemon a non-trivial probe target.
pub struct ClockTool;

#[async_trait]
impl Tool for ClockTool {
    fn name(&self) -> &str {
        "clock"
    }
    fn description(&self) -> &str {
        "Returns the current Unix time in milliseconds."
    }
    async fn call(&self, _arguments: Value) -> Result<ToolCallResult, ToolError> {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Ok(ToolCallResult::ok(vec![Content::json(
            serde_json::json!({ "epoch_ms": ms }),
        )]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn echo_returns_text_argument() {
        let t = EchoTool;
        let r = t.call(serde_json::json!({ "text": "ping" })).await.unwrap();
        assert!(!r.is_error);
        match &r.content[0] {
            Content::Text { text } => assert_eq!(text, "ping"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn echo_rejects_missing_text() {
        let t = EchoTool;
        let err = t.call(serde_json::json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn clock_returns_recent_epoch_ms() {
        let t = ClockTool;
        let r = t.call(Value::Null).await.unwrap();
        assert!(!r.is_error);
        let ms = match &r.content[0] {
            Content::Json { value } => value
                .get("epoch_ms")
                .and_then(|v| v.as_u64())
                .expect("epoch_ms u64"),
            other => panic!("unexpected: {other:?}"),
        };
        // A sanity floor: any time after 2024-01-01.
        assert!(ms > 1_704_067_200_000);
    }
}
