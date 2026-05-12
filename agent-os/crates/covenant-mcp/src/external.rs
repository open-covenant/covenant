//! Bridge from a remote MCP server to the local [`Tool`] trait.
//!
//! After spawning a JSON-RPC client, [`bootstrap_remote_tools`] performs the
//! MCP `initialize` handshake, fetches the server's tool list via
//! `tools/list`, and wraps each spec in a [`RemoteTool`] that forwards
//! `call(...)` over the same transport. The result drops cleanly into the
//! local [`crate::ToolRegistry`].

use crate::transport::{McpClient, McpClientError};
use crate::{Content, Tool, ToolCallResult, ToolError, ToolSpec};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;

/// MCP protocol version we advertise during `initialize`. The server may
/// downgrade; we don't enforce a specific version yet (Phase 4+).
pub const PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error("transport: {0}")]
    Transport(#[from] McpClientError),
    #[error("malformed tools/list response: {0}")]
    BadList(String),
    #[error("duplicate remote tool name after MCP prefixing: {0}")]
    DuplicateToolName(String),
}

#[derive(Debug, Clone, Default)]
pub struct RemoteToolOptions {
    pub tool_prefix: Option<String>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

/// Run `initialize` → `notifications/initialized` → `tools/list` against
/// `client` and return one [`Tool`] per advertised spec, all sharing the
/// same client.
pub async fn bootstrap_remote_tools(
    client: Arc<dyn McpClient>,
) -> Result<Vec<Arc<dyn Tool>>, BootstrapError> {
    bootstrap_remote_tools_with_options(client, RemoteToolOptions::default()).await
}

/// Like [`bootstrap_remote_tools`], but applies Covenant-side naming hygiene
/// and include/exclude filters before the remote specs enter the registry.
pub async fn bootstrap_remote_tools_with_options(
    client: Arc<dyn McpClient>,
    options: RemoteToolOptions,
) -> Result<Vec<Arc<dyn Tool>>, BootstrapError> {
    let init_params = serde_json::json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": { "name": "covenant", "version": env!("CARGO_PKG_VERSION") }
    });
    let _ = client.request("initialize", init_params).await?;
    client
        .notify("notifications/initialized", Value::Null)
        .await?;

    let list = client.request("tools/list", Value::Null).await?;
    let parsed: ToolsListResponse =
        serde_json::from_value(list).map_err(|e| BootstrapError::BadList(format!("{e}")))?;

    let mut out: Vec<Arc<dyn Tool>> = Vec::with_capacity(parsed.tools.len());
    let mut seen = HashSet::new();
    for spec in parsed.tools {
        if !options.allows(&spec.name) {
            continue;
        }
        let upstream_name = spec.name.clone();
        let advertised_name = options.advertised_name(&upstream_name);
        if !seen.insert(advertised_name.clone()) {
            return Err(BootstrapError::DuplicateToolName(advertised_name));
        }
        out.push(Arc::new(RemoteTool {
            client: client.clone(),
            upstream_name,
            spec: ToolSpec {
                name: advertised_name,
                ..spec
            },
        }));
    }
    Ok(out)
}

impl RemoteToolOptions {
    fn allows(&self, name: &str) -> bool {
        if !self.include.is_empty()
            && !self
                .include
                .iter()
                .any(|pattern| matches_filter(pattern, name))
        {
            return false;
        }
        !self
            .exclude
            .iter()
            .any(|pattern| matches_filter(pattern, name))
    }

    fn advertised_name(&self, upstream_name: &str) -> String {
        let Some(prefix) = self
            .tool_prefix
            .as_deref()
            .map(str::trim)
            .filter(|prefix| !prefix.is_empty())
        else {
            return upstream_name.to_string();
        };
        format!("mcp_{}_{}", sanitize_prefix(prefix), upstream_name)
    }
}

fn matches_filter(pattern: &str, name: &str) -> bool {
    let pattern = pattern.trim();
    if pattern == "*" || pattern == name {
        return true;
    }
    pattern
        .strip_suffix('*')
        .is_some_and(|prefix| name.starts_with(prefix))
}

fn sanitize_prefix(prefix: &str) -> String {
    let sanitized: String = prefix
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches('_');
    if trimmed.is_empty() {
        "remote".to_string()
    } else {
        trimmed.to_string()
    }
}

#[derive(Deserialize)]
struct ToolsListResponse {
    tools: Vec<ToolSpec>,
}

pub struct RemoteTool {
    client: Arc<dyn McpClient>,
    upstream_name: String,
    spec: ToolSpec,
}

#[async_trait]
impl Tool for RemoteTool {
    fn name(&self) -> &str {
        &self.spec.name
    }
    fn description(&self) -> &str {
        &self.spec.description
    }
    fn input_schema(&self) -> Value {
        self.spec.input_schema.clone()
    }
    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        let params = serde_json::json!({ "name": self.upstream_name, "arguments": arguments });
        match self.client.request("tools/call", params).await {
            Ok(v) => parse_tool_call_result(v),
            Err(McpClientError::Rpc { code, message }) => {
                Err(ToolError::Failed(format!("rpc {code}: {message}")))
            }
            Err(e) => Err(ToolError::Failed(format!("transport: {e}"))),
        }
    }
}

fn parse_tool_call_result(v: Value) -> Result<ToolCallResult, ToolError> {
    #[derive(Deserialize)]
    struct Wire {
        #[serde(default)]
        content: Vec<Content>,
        #[serde(default, alias = "isError")]
        is_error: bool,
    }
    let w: Wire = serde_json::from_value(v)
        .map_err(|e| ToolError::Failed(format!("bad tools/call result: {e}")))?;
    Ok(ToolCallResult {
        content: w.content,
        is_error: w.is_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockMcpClient;

    #[test]
    fn matches_filter_pins_star_exact_and_prefix_glob_branches() {
        assert!(
            matches_filter("*", "anything"),
            "'*' must pass every tool name; dropping this arm silently blocks every tool when the operator wires '*' in allowed_tools",
        );
        assert!(
            matches_filter("*", ""),
            "'*' must even pass the empty name so the all-pass arm is unconditional",
        );

        assert!(
            matches_filter("foo", "foo"),
            "an exact literal must match itself",
        );
        assert!(
            !matches_filter("foo", "bar"),
            "an exact literal must not match a different name",
        );
        assert!(
            !matches_filter("foo", "foobar"),
            "an exact literal must NOT match a longer name; otherwise the literal silently widens to a prefix",
        );

        assert!(
            matches_filter("foo*", "foo"),
            "the trailing-* glob must match the bare prefix; pattern='foo*' is documented to allow 'foo'",
        );
        assert!(
            matches_filter("foo*", "foobar"),
            "the trailing-* glob must match anything that starts with the prefix",
        );
        assert!(
            !matches_filter("foo*", "fo"),
            "the trailing-* glob must NOT match a name that is shorter than the prefix",
        );
        assert!(
            !matches_filter("foo*", "barfoo"),
            "the trailing-* glob is starts_with, not contains; embedded matches must reject",
        );

        assert!(
            matches_filter("  *  ", "x"),
            "leading/trailing whitespace on '*' must be trimmed before matching",
        );
        assert!(
            matches_filter("  foo  ", "foo"),
            "leading/trailing whitespace on an exact pattern must be trimmed before matching",
        );
        assert!(
            matches_filter(" foo* ", "foobar"),
            "leading/trailing whitespace on a glob pattern must be trimmed before matching",
        );
    }

    #[test]
    fn sanitize_prefix_maps_non_alnum_to_underscore_trims_edges_and_falls_back_to_remote() {
        assert_eq!(
            sanitize_prefix("myserver"),
            "myserver",
            "alphanumeric prefixes must pass through unchanged; otherwise operator-supplied names get rewritten in ways the tool-allowlist can't anticipate",
        );
        assert_eq!(
            sanitize_prefix("123"),
            "123",
            "digit-only prefixes must pass through unchanged so a numeric server label remains a valid tool-name fragment",
        );
        assert_eq!(
            sanitize_prefix("my-server"),
            "my_server",
            "documented character map: dash becomes underscore so the result stays inside the tool-name allowed alphabet",
        );
        assert_eq!(
            sanitize_prefix("my.server"),
            "my_server",
            "documented character map: dot becomes underscore so dotted hostnames don't collide with the mcp_<prefix>_<name> separator convention",
        );
        assert_eq!(
            sanitize_prefix("  my-server  "),
            "my_server",
            "surrounding whitespace must be mapped to underscore then trimmed; otherwise a stray space in operator config leaks into the wire tool name",
        );
        assert_eq!(
            sanitize_prefix(""),
            "remote",
            "an empty prefix must fall back to the literal 'remote' so the generated tool name mcp_remote_<name> never collapses to mcp__<name> and silently merges two unconfigured servers",
        );
        assert_eq!(
            sanitize_prefix("..."),
            "remote",
            "an all-punctuation prefix becomes all-underscore then trims to empty; the fallback must still kick in or the tool-name shape collapses",
        );
        assert_eq!(
            sanitize_prefix("   "),
            "remote",
            "an all-whitespace prefix is equivalent to empty after mapping and trimming; the fallback must apply consistently with the empty-input case",
        );
    }

    #[test]
    fn parse_tool_call_result_pins_default_content_default_is_error_camel_case_alias_and_error_wrap(
    ) {
        let r = parse_tool_call_result(serde_json::json!({})).expect(
            "an empty object must decode to an Ok ToolCallResult so MCP servers that omit content do not error out",
        );
        assert!(
            r.content.is_empty(),
            "missing content field must default to an empty Vec<Content>; otherwise a spec-compliant server that returns only is_error is silently rejected",
        );
        assert!(
            !r.is_error,
            "missing is_error field must default to false; otherwise every spec-compliant tools/call success would be treated as an error",
        );

        let r = parse_tool_call_result(serde_json::json!({ "isError": true }))
            .expect("the camelCase isError wire field must decode without error");
        assert!(
            r.is_error,
            "the isError serde alias must map onto is_error; otherwise every MCP-spec error result silently looks like a success because the wire form is camelCase",
        );

        let r = parse_tool_call_result(serde_json::json!({
            "content": [ { "type": "text", "text": "hello" } ],
            "isError": false
        }))
        .expect("a payload with text content must decode");
        assert_eq!(
            r.content,
            vec![Content::text("hello")],
            "text content must round-trip through parse_tool_call_result; otherwise tool output is silently dropped or mistyped",
        );
        assert!(
            !r.is_error,
            "isError=false must keep is_error=false; the alias must not invert the value",
        );

        let err = parse_tool_call_result(serde_json::json!("not an object"))
            .expect_err("a non-object payload must not decode as a ToolCallResult");
        match err {
            ToolError::Failed(msg) => assert!(
                msg.starts_with("bad tools/call result: "),
                "the error message must begin with the documented prefix so operators can grep for upstream wire failures; got: {msg}",
            ),
            other => panic!(
                "expected ToolError::Failed for a malformed wire payload, got: {other:?}"
            ),
        }
    }

    fn happy_handler(method: &str, params: &Value) -> Result<Value, McpClientError> {
        match method {
            "initialize" => Ok(serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "fake", "version": "0.0.1" }
            })),
            "tools/list" => Ok(serde_json::json!({
                "tools": [
                    {
                        "name": "fs.read",
                        "description": "read a file",
                        "inputSchema": { "type": "object" }
                    },
                    {
                        "name": "fs.write",
                        "description": "write a file",
                        "inputSchema": { "type": "object" }
                    }
                ]
            })),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or("");
                Ok(serde_json::json!({
                    "content": [{ "type": "text", "text": format!("called {name}") }],
                    "isError": false
                }))
            }
            other => Err(McpClientError::Rpc {
                code: -32601,
                message: format!("unknown method {other}"),
            }),
        }
    }

    #[tokio::test]
    async fn bootstrap_lists_remote_tools() {
        let client: Arc<dyn McpClient> = Arc::new(MockMcpClient::new(happy_handler));
        let tools = bootstrap_remote_tools(client.clone()).await.unwrap();
        let names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
        assert_eq!(names, vec!["fs.read".to_string(), "fs.write".to_string()]);
    }

    #[tokio::test]
    async fn bootstrap_fires_initialized_notification() {
        let mock = Arc::new(MockMcpClient::new(happy_handler));
        let _ = bootstrap_remote_tools(mock.clone() as Arc<dyn McpClient>)
            .await
            .unwrap();
        let n = mock.notifications();
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].0, "notifications/initialized");
    }

    #[tokio::test]
    async fn remote_tool_call_forwards_arguments() {
        let client: Arc<dyn McpClient> = Arc::new(MockMcpClient::new(happy_handler));
        let tools = bootstrap_remote_tools(client).await.unwrap();
        let r = tools[0]
            .call(serde_json::json!({ "path": "/tmp" }))
            .await
            .unwrap();
        assert!(!r.is_error);
        match &r.content[0] {
            Content::Text { text } => assert_eq!(text, "called fs.read"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn prefixed_remote_tool_forwards_original_name() {
        let client: Arc<dyn McpClient> = Arc::new(MockMcpClient::new(happy_handler));
        let tools = bootstrap_remote_tools_with_options(
            client,
            RemoteToolOptions {
                tool_prefix: Some("hermes.agent".into()),
                ..RemoteToolOptions::default()
            },
        )
        .await
        .unwrap();
        let names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
        assert_eq!(
            names,
            vec![
                "mcp_hermes_agent_fs.read".to_string(),
                "mcp_hermes_agent_fs.write".to_string()
            ]
        );

        let r = tools[0]
            .call(serde_json::json!({ "path": "/tmp" }))
            .await
            .unwrap();
        match &r.content[0] {
            Content::Text { text } => assert_eq!(text, "called fs.read"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn filters_remote_tools_before_advertising() {
        let client: Arc<dyn McpClient> = Arc::new(MockMcpClient::new(happy_handler));
        let tools = bootstrap_remote_tools_with_options(
            client,
            RemoteToolOptions {
                include: vec!["fs.*".into()],
                exclude: vec!["fs.write".into()],
                ..RemoteToolOptions::default()
            },
        )
        .await
        .unwrap();
        let names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
        assert_eq!(names, vec!["fs.read".to_string()]);
    }

    #[tokio::test]
    async fn duplicate_remote_names_fail_bootstrap() {
        let client: Arc<dyn McpClient> = Arc::new(MockMcpClient::new(|method, _| match method {
            "initialize" => Ok(serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "fake", "version": "0.0.1" }
            })),
            "tools/list" => Ok(serde_json::json!({
                "tools": [
                    {
                        "name": "dup",
                        "description": "first",
                        "inputSchema": { "type": "object" }
                    },
                    {
                        "name": "dup",
                        "description": "second",
                        "inputSchema": { "type": "object" }
                    }
                ]
            })),
            other => Err(McpClientError::Rpc {
                code: -32601,
                message: format!("unknown method {other}"),
            }),
        }));

        match bootstrap_remote_tools(client).await {
            Err(BootstrapError::DuplicateToolName(name)) => assert_eq!(name, "dup"),
            Err(other) => panic!("unexpected: {other:?}"),
            Ok(_) => panic!("duplicate remote tool names should fail bootstrap"),
        }
    }

    #[tokio::test]
    async fn remote_tool_propagates_rpc_errors() {
        let client: Arc<dyn McpClient> = Arc::new(MockMcpClient::new(|method, _| match method {
            "initialize" | "tools/list" => happy_handler(method, &Value::Null),
            "tools/call" => Err(McpClientError::Rpc {
                code: -32602,
                message: "invalid params".into(),
            }),
            _ => unreachable!(),
        }));
        let tools = bootstrap_remote_tools(client).await.unwrap();
        let err = tools[0].call(Value::Null).await.unwrap_err();
        match err {
            ToolError::Failed(msg) => assert!(msg.contains("invalid params")),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
