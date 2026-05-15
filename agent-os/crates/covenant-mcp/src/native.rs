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

    #[test]
    fn echo_input_schema_pins_required_text_string_property_and_additional_properties_false() {
        // EchoTool::input_schema overrides Tool::input_schema's
        // empty-object default with the documented MCP-tools/list schema:
        //
        //   {
        //     "type": "object",
        //     "properties": { "text": { "type": "string" } },
        //     "required": ["text"],
        //     "additionalProperties": false
        //   }
        //
        // The schema is what every MCP client (Claude Desktop, downstream
        // wrappers, generated SDK clients) reads to validate operator
        // inputs before sending them to the daemon. echo_returns_text_argument
        // and echo_rejects_missing_text cover the call() runtime path but
        // no test pins the schema shape itself. A refactor that flipped
        // additionalProperties to true would silently let MCP clients send
        // arbitrary extra fields; a refactor that renamed properties.text
        // to message without updating call()'s arguments.get('text') would
        // silently break every operator-facing echo invocation.
        let schema = EchoTool.input_schema();
        let obj = schema
            .as_object()
            .expect("EchoTool::input_schema must emit a JSON object");

        assert_eq!(
            obj.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "EchoTool::input_schema must declare type=object — a refactor \
             that changed the top-level type to anything else (e.g., array) \
             would break every MCP client's argument-validation entry path",
        );

        let properties = obj.get("properties").and_then(|v| v.as_object()).expect(
            "schema.properties must be a JSON object — MCP clients \
                     traverse it to enumerate accepted arguments",
        );
        let text_spec = properties.get("text").and_then(|v| v.as_object()).expect(
            "schema.properties.text must be an object with a type field — \
                 a refactor that renamed properties.text (e.g., to message) \
                 without updating call()'s arguments.get('text') would silently \
                 break every operator-facing echo invocation because the daemon \
                 keeps reading the old key",
        );
        assert_eq!(
            text_spec.get("type").and_then(|v| v.as_str()),
            Some("string"),
            "EchoTool::input_schema.properties.text.type must declare 'string' \
             — call() uses Value::as_str on arguments.get('text'), so a schema \
             that declared anything else (e.g., 'integer' or 'array') would \
             let MCP clients pass values that the daemon path treats as \
             missing-text",
        );

        let required = obj.get("required").and_then(|v| v.as_array()).expect(
            "schema.required must be a JSON array — a refactor that swapped \
                 the array for a string 'text' under a 'simpler schema authoring' \
                 rationale would silently break MCP client validation that reads \
                 required as an array, and operator-facing inputs that omitted \
                 the text field would either fail with a confusing schema error \
                 or succeed unexpectedly through a permissive fallback",
        );
        assert_eq!(
            required.len(),
            1,
            "schema.required must be exactly one element — a refactor that \
             added a second required field (e.g., metadata) without updating \
             call()'s extraction path would silently let every echo call \
             surface a confusing missing-arg error on inputs that historically \
             validated",
        );
        assert_eq!(
            required[0].as_str(),
            Some("text"),
            "schema.required[0] must be the string 'text' — pinning the \
             exact field name so a refactor that renamed properties.text \
             and the required entry together (but not call()'s arguments.get) \
             still surfaces here, and the cross-binding with the call()-side \
             literal stays anchored",
        );

        let additional = obj.get("additionalProperties").unwrap_or_else(|| {
            panic!(
                "schema.additionalProperties must be present — a refactor \
                 that dropped the field under a 'JSON Schema default is \
                 false anyway' rationale would silently let some MCP \
                 clients (whose defaults are true) accept extra fields the \
                 daemon ignores"
            )
        });
        assert_eq!(
            additional,
            &Value::Bool(false),
            "schema.additionalProperties must be the JSON bool false (not the \
             string 'false', not the number 0, not null) — a refactor that \
             flipped this to true to support a future optional field without \
             bumping the schema would silently let MCP clients send arbitrary \
             extra fields, and SDK clients generated from the schema would \
             lose the closed-world invariant the daemon's call() path relies \
             on for argument shape stability",
        );

        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["additionalProperties", "properties", "required", "type"],
            "EchoTool::input_schema must surface EXACTLY four top-level keys \
             (type, properties, required, additionalProperties) — a refactor \
             that added a fifth field (e.g., 'title' or '$schema') without \
             coordinating the MCP wire-form expectations would silently shift \
             the schema shape across every client; pinning the exact key set \
             catches both additions and removals in one assertion",
        );
    }

    #[test]
    fn echo_and_clock_tool_name_and_description_pin_operator_facing_strings() {
        // EchoTool::name and description, plus ClockTool::name and
        // description, are the four operator-facing strings every MCP
        // client (Claude
        // Desktop, downstream SDK wrappers, the Covenant TUI) reads
        // out of tools/list to render the tool catalog. The names
        // double as selectors operators type into commands; the
        // descriptions are the tooltip operators consult to decide
        // when to invoke the tool.
        //
        // registry_lists_tools_sorted_by_name (in lib.rs) pins the
        // names indirectly via the sorted-output assertion
        // ['clock', 'echo'], so a rename of either would surface there.
        // But no test reads .description() and compares the literal
        // string. A refactor that rewrote either description —
        // 'Returns the provided text argument verbatim.' → 'Echo the
        // input back.' for terseness, or 'Returns the current Unix
        // time in milliseconds.' → 'Get current time.' for
        // accessibility — would silently shift the operator-facing
        // copy. echo_returns_text_argument and
        // clock_returns_recent_epoch_ms probe call() behavior, not the
        // description string.

        assert_eq!(
            EchoTool.name(),
            "echo",
            "EchoTool::name must remain 'echo' — the operator-typed \
             selector and the registry sort key. A rename would break \
             every operator command and the sort-order pin in \
             registry_lists_tools_sorted_by_name",
        );
        assert_eq!(
            EchoTool.description(),
            "Returns the provided `text` argument verbatim.",
            "EchoTool::description must remain the literal documented \
             string — the backtick-wrapped 'text' identifier names the \
             argument operators must pass, and a rewrite (e.g., 'Echo \
             the input back.') would silently drop that pointer. The \
             behavior tests probe call() return value, not the \
             description, so they pass under any rewrite",
        );
        assert_eq!(
            ClockTool.name(),
            "clock",
            "ClockTool::name must remain 'clock' — the operator-typed \
             selector and the registry sort key paired with EchoTool::name",
        );
        assert_eq!(
            ClockTool.description(),
            "Returns the current Unix time in milliseconds.",
            "ClockTool::description must remain the literal documented \
             string — the 'milliseconds' unit specifier is load-bearing \
             because operators consuming the epoch_ms return value need \
             to know whether to convert to seconds, minutes, or use the \
             value verbatim. A rewrite that dropped the unit (e.g., \
             'Get current time.' or 'Return Unix time.') would silently \
             let operators consume the value as seconds and parse the \
             returned epoch as a date ~1000x earlier than intended",
        );
    }
}
