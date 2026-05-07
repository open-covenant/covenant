//! MCP-aligned tool abstraction for the covenant runtime.
//!
//! Wire types follow the public Model Context Protocol shapes (`name`,
//! `description`, `inputSchema`, `Content` blocks, `isError`) so the same
//! `Tool` trait can later back native Rust impls *and* external MCP servers
//! over stdio/HTTP. Sprint 22 ships the trait + registry + two native tools.
//! External MCP transport (stdio JSON-RPC 2.0) is the next sprint.

#![deny(unsafe_code)]

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

pub mod config;
pub mod external;
pub mod native;
pub mod transport;

/// Public spec for a tool — what the registry advertises via `tools/list`.
/// Field naming matches the MCP wire format (camelCase).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's `arguments` object. An empty object means
    /// the tool takes no arguments.
    pub input_schema: Value,
}

/// One block of tool output. The MCP spec admits more variants (image,
/// resource); v0 ships text + json only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Content {
    Text { text: String },
    Json { value: Value },
}

impl Content {
    pub fn text(t: impl Into<String>) -> Self {
        Content::Text { text: t.into() }
    }
    pub fn json(v: Value) -> Self {
        Content::Json { value: v }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallResult {
    pub content: Vec<Content>,
    pub is_error: bool,
}

impl ToolCallResult {
    pub fn ok(content: Vec<Content>) -> Self {
        Self {
            content,
            is_error: false,
        }
    }
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: vec![Content::text(message)],
            is_error: true,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("tool not found: {0}")]
    NotFound(String),
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
    #[error("tool failed: {0}")]
    Failed(String),
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON Schema for `arguments`. Default is "no arguments allowed".
    fn input_schema(&self) -> Value {
        serde_json::json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }
    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError>;

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_schema: self.input_schema(),
        }
    }
}

/// Registry of named tools. Insertion-ordered listing isn't part of the MCP
/// contract — we sort by name for deterministic output, which keeps tests
/// and audit hashes stable.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    inner: Arc<BTreeMap<String, Arc<dyn Tool>>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_tools(tools: Vec<Arc<dyn Tool>>) -> Self {
        let mut map = BTreeMap::new();
        for t in tools {
            map.insert(t.name().to_string(), t);
        }
        Self {
            inner: Arc::new(map),
        }
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn list_specs(&self) -> Vec<ToolSpec> {
        self.inner.values().map(|t| t.spec()).collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.inner.keys().cloned().collect()
    }

    pub async fn call(&self, name: &str, arguments: Value) -> Result<ToolCallResult, ToolError> {
        let tool = self
            .inner
            .get(name)
            .ok_or_else(|| ToolError::NotFound(name.to_string()))?;
        tool.call(arguments).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::EchoTool;

    fn registry_with_echo() -> ToolRegistry {
        ToolRegistry::from_tools(vec![Arc::new(EchoTool)])
    }

    #[test]
    fn tool_spec_uses_camel_case_on_the_wire() {
        let spec = ToolSpec {
            name: "echo".into(),
            description: "d".into(),
            input_schema: serde_json::json!({}),
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains("\"inputSchema\""));
        assert!(!json.contains("input_schema"));
    }

    #[test]
    fn content_variants_serialise_with_type_tag() {
        let t = Content::text("hi");
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"text\":\"hi\""));
    }

    #[test]
    fn tool_call_result_is_error_serialises_camel_case() {
        let r = ToolCallResult::error("nope");
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"isError\":true"));
    }

    #[tokio::test]
    async fn registry_lists_tools_sorted_by_name() {
        let reg = ToolRegistry::from_tools(vec![Arc::new(EchoTool), Arc::new(native::ClockTool)]);
        let names: Vec<String> = reg.list_specs().into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["clock".to_string(), "echo".to_string()]);
    }

    #[tokio::test]
    async fn registry_call_returns_not_found_for_unknown() {
        let reg = registry_with_echo();
        let err = reg.call("does-not-exist", Value::Null).await.unwrap_err();
        assert!(matches!(err, ToolError::NotFound(_)));
    }

    #[tokio::test]
    async fn registry_dispatches_to_named_tool() {
        let reg = registry_with_echo();
        let r = reg
            .call("echo", serde_json::json!({ "text": "hello" }))
            .await
            .unwrap();
        assert!(!r.is_error);
        match &r.content[0] {
            Content::Text { text } => assert_eq!(text, "hello"),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
