//! Model Context Protocol (MCP) integration for Covenant.
//!
//! Wire types follow the public MCP shapes (`name`, `description`,
//! `inputSchema`, `Content` blocks, `isError`) so the same [`Tool`]
//! trait backs native Rust implementations and external MCP servers
//! reached over stdio JSON-RPC 2.0. The crate exposes the trait, an
//! in-process [`ToolRegistry`], a small set of native tools under
//! [`native`], and the external transport under [`transport`].

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
            let name = t.name().to_string();
            if map.contains_key(&name) {
                tracing::warn!(tool = %name, "duplicate tool name ignored");
                continue;
            }
            map.insert(name, t);
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
    use async_trait::async_trait;

    struct StaticTool {
        name: &'static str,
        text: &'static str,
    }

    #[async_trait]
    impl Tool for StaticTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "static test tool"
        }

        async fn call(&self, _arguments: Value) -> Result<ToolCallResult, ToolError> {
            Ok(ToolCallResult::ok(vec![Content::text(self.text)]))
        }
    }

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
    fn tool_spec_serde_pins_camel_case_round_trip() {
        // ToolSpec is the public MCP wire shape advertised by every
        // ToolRegistry::list_specs response. The inputSchema field
        // name is load-bearing — spec-compliant external MCP servers
        // (and the stdio JSON-RPC transport) key on inputSchema, not
        // input_schema. A dropped rename_all = camelCase attribute
        // would silently switch the wire form to input_schema and
        // break every external transport, while leaving the in-process
        // ToolRegistry path working because Rust types still match.
        let spec = ToolSpec {
            name: "echo".into(),
            description: "d".into(),
            input_schema: serde_json::json!({"type": "object"}),
        };
        let wire = serde_json::json!({
            "name": "echo",
            "description": "d",
            "inputSchema": {"type": "object"},
        });
        assert_eq!(serde_json::to_value(&spec).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<ToolSpec>(wire.clone()).unwrap(),
            spec,
        );

        // The snake_case wire form must be rejected so a dropped
        // rename_all attribute fails loud at the boundary instead of
        // silently swapping the published field name.
        assert!(
            serde_json::from_value::<ToolSpec>(serde_json::json!({
                "name": "echo",
                "description": "d",
                "input_schema": {"type": "object"},
            }))
            .is_err(),
            "snake_case input_schema must be rejected so the rename_all camelCase whitelist stays tight",
        );

        // Missing the required inputSchema field must fail so a future
        // refactor that drops the field forces an explicit migration
        // decision instead of silently accepting an empty Value.
        assert!(
            serde_json::from_value::<ToolSpec>(serde_json::json!({
                "name": "echo",
                "description": "d",
            }))
            .is_err(),
            "missing inputSchema must be rejected so the required field cannot silently disappear",
        );
    }

    #[test]
    fn tool_spec_serde_pins_strict_required_fields_reject_on_omission() {
        // tool_spec_serde_pins_camel_case_round_trip pins the camelCase
        // wire form, the snake_case rejection, and the missing-inputSchema
        // rejection. It does NOT assert a closed three-key wire set and
        // it does NOT reject missing name or description — a stray
        // #[serde(default)] on either of those would silently let a
        // malformed external tools/list payload decode with an
        // empty-string default, and the MCP transport would advertise a
        // half-populated ToolSpec without any boundary signal. Pin all
        // three required keys here so a future refactor that drops any
        // one of them forces an explicit migration decision instead of
        // silently shrinking the wire shape.
        let spec = ToolSpec {
            name: "echo".into(),
            description: "echoes input".into(),
            input_schema: serde_json::json!({"type": "object"}),
        };

        let wire = serde_json::to_value(&spec).unwrap();
        let obj = wire
            .as_object()
            .expect("ToolSpec serialises as a JSON object");
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        let expected: std::collections::BTreeSet<&str> =
            ["name", "description", "inputSchema"].into_iter().collect();
        assert_eq!(
            keys, expected,
            "ToolSpec wire form must be exactly three keys (name, \
             description, inputSchema); an added skip_serializing_if \
             field would silently expand the wire shape and strict \
             external MCP servers may reject the unexpected key, \
             breaking tools/list round-trips through the stdio transport",
        );

        let back: ToolSpec = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, spec,
            "ToolSpec must round-trip through serde_json verbatim — the \
             PartialEq derive is the contract every MCP tools/list \
             consumer leans on",
        );

        for required in ["name", "description", "inputSchema"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<ToolSpec>(serde_json::Value::Object(missing)).is_err(),
                "ToolSpec wire form must reject a payload missing {required:?}; \
                 a stray #[serde(default)] would silently let a malformed \
                 external tools/list payload decode with an empty default \
                 and the LLM agent would route through an unidentifiable tool",
            );
        }
    }

    #[test]
    fn content_variants_serialise_with_type_tag() {
        let t = Content::text("hi");
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"text\":\"hi\""));
    }

    #[test]
    fn content_serde_pins_each_variant_wire_form() {
        let text = Content::text("hi");
        let text_wire = serde_json::json!({ "type": "text", "text": "hi" });
        assert_eq!(serde_json::to_value(&text).unwrap(), text_wire);
        assert_eq!(
            serde_json::from_value::<Content>(text_wire.clone()).unwrap(),
            text
        );

        let json = Content::json(serde_json::json!({ "sum": 7 }));
        let json_wire = serde_json::json!({ "type": "json", "value": { "sum": 7 } });
        assert_eq!(serde_json::to_value(&json).unwrap(), json_wire);
        assert_eq!(
            serde_json::from_value::<Content>(json_wire.clone()).unwrap(),
            json
        );

        assert!(
            serde_json::from_value::<Content>(serde_json::json!({
                "type": "Text",
                "text": "hi",
            }))
            .is_err(),
            "titlecase discriminator must be rejected so the camelCase whitelist stays tight",
        );
        assert!(
            serde_json::from_value::<Content>(serde_json::json!({
                "type": "image",
                "data": "...",
            }))
            .is_err(),
            "unknown variant must be rejected so future MCP additions force an explicit rename",
        );
    }

    #[test]
    fn tool_call_result_is_error_serialises_camel_case() {
        let r = ToolCallResult::error("nope");
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"isError\":true"));
    }

    #[test]
    fn tool_call_result_serde_pins_camel_case_round_trip() {
        // ToolCallResult is the public MCP wire shape returned by every
        // tools/call invocation. The isError field name is load-bearing
        // — spec-compliant external MCP servers (and the stdio JSON-RPC
        // transport) key on isError, not is_error. A dropped rename_all
        // = camelCase attribute would silently switch the wire form to
        // is_error and break every external transport that does NOT go
        // through the external::parse_tool_call_result alias path
        // (parse_tool_call_result_pins_default_content_default_is_error_camel_case_alias_and_error_wrap
        // pins the alias path separately). Pin the full round-trip on
        // the canonical struct so the two surfaces stay distinct.
        let ok = ToolCallResult::ok(vec![Content::text("hi")]);
        let ok_wire = serde_json::json!({
            "content": [{"type": "text", "text": "hi"}],
            "isError": false,
        });
        assert_eq!(serde_json::to_value(&ok).unwrap(), ok_wire);
        assert_eq!(
            serde_json::from_value::<ToolCallResult>(ok_wire.clone()).unwrap(),
            ok,
        );

        let err = ToolCallResult::error("nope");
        let err_wire = serde_json::json!({
            "content": [{"type": "text", "text": "nope"}],
            "isError": true,
        });
        assert_eq!(serde_json::to_value(&err).unwrap(), err_wire);
        assert_eq!(
            serde_json::from_value::<ToolCallResult>(err_wire.clone()).unwrap(),
            err,
        );

        // The snake_case wire form must be rejected on the canonical
        // struct — the is_error alias on the parse_tool_call_result
        // path is a separate, intentional compatibility surface and
        // must not bleed into direct ToolCallResult deserialization.
        assert!(
            serde_json::from_value::<ToolCallResult>(serde_json::json!({
                "content": [],
                "is_error": false,
            }))
            .is_err(),
            "snake_case is_error must be rejected on the canonical struct so a dropped rename_all attribute fails loud (the parse_tool_call_result alias path stays separate)",
        );
    }

    #[test]
    fn tool_call_result_serde_pins_strict_required_fields_reject_on_omission() {
        // tool_call_result_serde_pins_camel_case_round_trip pins the
        // canonical camelCase wire form and the snake_case rejection,
        // but does NOT assert a closed two-key wire set and does NOT
        // reject missing content or isError. A stray #[serde(default)]
        // on either field would silently let a malformed payload decode
        // (content as empty Vec or is_error as false), and the canonical
        // MCP transport path would silently shrink while leaving the
        // parse_tool_call_result alias path
        // (parse_tool_call_result_pins_default_content_default_is_error_camel_case_alias_and_error_wrap
        // in external.rs) untouched — the two surfaces must stay
        // distinct. Pin both required keys on the canonical struct so
        // a future refactor that drops either field's strict-required
        // contract forces an explicit migration decision instead of
        // silently shrinking the wire shape on the canonical path.
        let result = ToolCallResult::ok(vec![Content::text("hi")]);

        let wire = serde_json::to_value(&result).unwrap();
        let obj = wire
            .as_object()
            .expect("ToolCallResult serialises as a JSON object");
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        let expected: std::collections::BTreeSet<&str> =
            ["content", "isError"].into_iter().collect();
        assert_eq!(
            keys, expected,
            "ToolCallResult wire form must be exactly two keys (content, \
             isError); an added skip_serializing_if field would silently \
             expand the canonical wire shape and break consumers \
             destructuring on the documented two-key shape (the \
             parse_tool_call_result alias path is a separate surface)",
        );

        let back: ToolCallResult = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, result,
            "ToolCallResult must round-trip through serde_json verbatim \
             — the PartialEq derive is the contract every MCP tools/call \
             consumer leans on",
        );

        for required in ["content", "isError"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<ToolCallResult>(serde_json::Value::Object(missing))
                    .is_err(),
                "ToolCallResult wire form must reject a payload missing {required:?} \
                 on the canonical path; a stray #[serde(default)] would \
                 silently let a malformed external tools/call payload decode \
                 with an empty content Vec or is_error=false (the alias-path \
                 default contract stays separate)",
            );
        }
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

    #[tokio::test]
    async fn registry_keeps_first_duplicate_tool_name() {
        let reg = ToolRegistry::from_tools(vec![
            Arc::new(StaticTool {
                name: "dup",
                text: "first",
            }),
            Arc::new(StaticTool {
                name: "dup",
                text: "second",
            }),
        ]);

        assert_eq!(reg.len(), 1);
        let r = reg.call("dup", Value::Null).await.unwrap();
        match &r.content[0] {
            Content::Text { text } => assert_eq!(text, "first"),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
