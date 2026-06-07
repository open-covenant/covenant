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
/// downgrade; a version mismatch is logged, not enforced.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// Well-known hosted Solana MCP endpoint (Streamable HTTP transport). The
/// Solana dev skill installs it with `claude mcp add --transport http`.
pub const SOLANA_MCP_URL: &str = "https://mcp.solana.com/mcp";

/// Build the stdio shim command that bridges a hosted (HTTP) MCP server to
/// the daemon's stdio JSON-RPC transport via the `mcp-remote` adapter.
///
/// Covenant has no in-process HTTP MCP client. Routing a hosted endpoint
/// through a subprocess keeps it behind the same `kill_on_drop` boundary as
/// every other external MCP server, so the remote never reaches the
/// daemon's address space directly.
pub fn hosted_bridge_command(url: &str) -> (String, Vec<String>) {
    (
        "npx".to_string(),
        vec!["-y".to_string(), "mcp-remote".to_string(), url.to_string()],
    )
}

#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error("transport: {0}")]
    Transport(#[from] McpClientError),
    #[error("malformed initialize response: {0}")]
    BadInitialize(String),
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

fn initialize_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
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
    let init_response = client.request("initialize", init_params).await?;
    // The MCP `initialize` result MUST be a JSON object per the spec.
    // A non-object (null, string, array, number) is a misconfigured or
    // incompatible server; refusing here keeps the broken session from
    // poisoning the downstream tools/list call with a useless error.
    let init_obj = init_response.as_object().ok_or_else(|| {
        BootstrapError::BadInitialize(format!(
            "expected JSON object, got {}",
            initialize_kind(&init_response)
        ))
    })?;
    // protocolVersion mismatch is non-fatal in Phase 0 — the spec
    // explicitly allows the server to negotiate a different version —
    // but operators triaging a broken connection deserve a warn so the
    // log shows what the server actually offered.
    if let Some(advertised) = init_obj.get("protocolVersion").and_then(|v| v.as_str()) {
        if advertised != PROTOCOL_VERSION {
            tracing::warn!(
                client_protocol = PROTOCOL_VERSION,
                server_protocol = advertised,
                "mcp: initialize protocolVersion mismatch"
            );
        }
    }
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
    fn hosted_bridge_command_wraps_url_in_mcp_remote_stdio_shim() {
        let (command, args) = hosted_bridge_command(SOLANA_MCP_URL);
        assert_eq!(command, "npx", "hosted bridge launches via npx");
        assert_eq!(
            args,
            vec![
                "-y".to_string(),
                "mcp-remote".to_string(),
                SOLANA_MCP_URL.to_string(),
            ],
            "args are exactly [-y, mcp-remote, <url>] with the URL last",
        );
        assert_eq!(
            args.last().map(String::as_str),
            Some(SOLANA_MCP_URL),
            "the endpoint URL is the final positional arg",
        );

        assert_eq!(
            SOLANA_MCP_URL, "https://mcp.solana.com/mcp",
            "SOLANA_MCP_URL pins the well-known hosted endpoint",
        );
    }

    #[test]
    fn protocol_version_is_mcp_spec_date() {
        assert_eq!(
            PROTOCOL_VERSION, "2024-11-05",
            "PROTOCOL_VERSION is the MCP spec revision the external transport implements",
        );

        assert_eq!(
            PROTOCOL_VERSION.len(),
            10,
            "PROTOCOL_VERSION is a 10-char YYYY-MM-DD date string",
        );

        let parts: Vec<&str> = PROTOCOL_VERSION.split('-').collect();
        assert_eq!(
            parts.len(),
            3,
            "PROTOCOL_VERSION splits on '-' into [year, month, day]",
        );
        for (label, part, expected_len) in [
            ("year", parts[0], 4),
            ("month", parts[1], 2),
            ("day", parts[2], 2),
        ] {
            assert_eq!(
                part.len(),
                expected_len,
                "{label} component is {expected_len} digits; got {part:?}",
            );
            assert!(
                part.chars().all(|c| c.is_ascii_digit()),
                "{label} component is all ASCII digits; got {part:?}",
            );
        }
    }

    #[test]
    fn matches_filter_branches() {
        assert!(matches_filter("*", "anything"), "'*' passes every name");
        assert!(matches_filter("*", ""), "'*' passes the empty name");

        assert!(
            matches_filter("foo", "foo"),
            "an exact literal matches itself"
        );
        assert!(
            !matches_filter("foo", "bar"),
            "an exact literal does not match a different name",
        );
        assert!(
            !matches_filter("foo", "foobar"),
            "an exact literal does not match a longer name",
        );

        assert!(
            matches_filter("foo*", "foo"),
            "the trailing-* glob matches the bare prefix",
        );
        assert!(
            matches_filter("foo*", "foobar"),
            "the trailing-* glob matches anything starting with the prefix",
        );
        assert!(
            !matches_filter("foo*", "fo"),
            "the trailing-* glob does not match a name shorter than the prefix",
        );
        assert!(
            !matches_filter("foo*", "barfoo"),
            "the trailing-* glob is starts_with, not contains",
        );

        assert!(
            matches_filter("  *  ", "x"),
            "'*' is trimmed before matching"
        );
        assert!(
            matches_filter("  foo  ", "foo"),
            "an exact pattern is trimmed before matching",
        );
        assert!(
            matches_filter(" foo* ", "foobar"),
            "a glob pattern is trimmed before matching",
        );
    }

    #[test]
    fn sanitize_prefix_branches() {
        assert_eq!(
            sanitize_prefix("myserver"),
            "myserver",
            "alphanumeric prefixes pass through unchanged",
        );
        assert_eq!(
            sanitize_prefix("123"),
            "123",
            "digit-only prefixes pass through unchanged",
        );
        assert_eq!(
            sanitize_prefix("my-server"),
            "my_server",
            "dash maps to underscore",
        );
        assert_eq!(
            sanitize_prefix("my.server"),
            "my_server",
            "dot maps to underscore",
        );
        assert_eq!(
            sanitize_prefix("  my-server  "),
            "my_server",
            "surrounding whitespace is mapped then trimmed",
        );
        assert_eq!(
            sanitize_prefix(""),
            "remote",
            "an empty prefix falls back to 'remote'",
        );
        assert_eq!(
            sanitize_prefix("..."),
            "remote",
            "an all-punctuation prefix trims to empty and falls back to 'remote'",
        );
        assert_eq!(
            sanitize_prefix("   "),
            "remote",
            "an all-whitespace prefix falls back to 'remote'",
        );
    }

    #[test]
    fn allows_include_exclude_precedence() {
        let default = RemoteToolOptions::default();
        assert!(
            default.allows("anything"),
            "empty include and exclude allow every name",
        );

        let include_only = RemoteToolOptions {
            tool_prefix: None,
            include: vec!["exact".into()],
            exclude: vec![],
        };
        assert!(
            include_only.allows("exact"),
            "a name matching the include pattern is allowed",
        );
        assert!(
            !include_only.allows("other"),
            "a non-empty include acts as an allowlist; unmatched names are denied",
        );

        let exclude_only = RemoteToolOptions {
            tool_prefix: None,
            include: vec![],
            exclude: vec!["banned".into()],
        };
        assert!(
            !exclude_only.allows("banned"),
            "exclude applies even when include is empty",
        );
        assert!(
            exclude_only.allows("other"),
            "with only exclude set, unmatched names are allowed",
        );

        let star_include_with_exclude = RemoteToolOptions {
            tool_prefix: None,
            include: vec!["*".into()],
            exclude: vec!["banned".into()],
        };
        assert!(
            !star_include_with_exclude.allows("banned"),
            "exclude wins over a catch-all include",
        );
        assert!(
            star_include_with_exclude.allows("other"),
            "the catch-all include still allows non-excluded names",
        );

        let glob_include = RemoteToolOptions {
            tool_prefix: None,
            include: vec!["foo*".into()],
            exclude: vec![],
        };
        assert!(
            glob_include.allows("foobar"),
            "a trailing-* include uses the matches_filter glob path",
        );
        assert!(
            !glob_include.allows("bar"),
            "the include glob is starts_with-only",
        );
    }

    #[test]
    fn advertised_name_branches() {
        let none = RemoteToolOptions::default();
        assert_eq!(
            none.advertised_name("foo"),
            "foo",
            "None prefix returns the upstream name verbatim",
        );

        let empty = RemoteToolOptions {
            tool_prefix: Some(String::new()),
            include: vec![],
            exclude: vec![],
        };
        assert_eq!(
            empty.advertised_name("foo"),
            "foo",
            "an empty prefix returns the upstream name verbatim",
        );

        let whitespace = RemoteToolOptions {
            tool_prefix: Some("   ".into()),
            include: vec![],
            exclude: vec![],
        };
        assert_eq!(
            whitespace.advertised_name("foo"),
            "foo",
            "a whitespace-only prefix returns the upstream name verbatim",
        );

        let plain = RemoteToolOptions {
            tool_prefix: Some("myserver".into()),
            include: vec![],
            exclude: vec![],
        };
        assert_eq!(
            plain.advertised_name("foo"),
            "mcp_myserver_foo",
            "a prefix wraps as mcp_<prefix>_<upstream>",
        );

        let dashed = RemoteToolOptions {
            tool_prefix: Some("my-server".into()),
            include: vec![],
            exclude: vec![],
        };
        assert_eq!(
            dashed.advertised_name("foo"),
            "mcp_my_server_foo",
            "the prefix is sanitized (dash to underscore) before wrapping",
        );
    }

    #[test]
    fn parse_tool_call_result_defaults() {
        let r = parse_tool_call_result(serde_json::json!({}))
            .expect("an empty object decodes to an Ok ToolCallResult");
        assert!(
            r.content.is_empty(),
            "missing content defaults to an empty Vec"
        );
        assert!(!r.is_error, "missing is_error defaults to false");

        let r = parse_tool_call_result(serde_json::json!({ "isError": true }))
            .expect("the camelCase isError field decodes");
        assert!(r.is_error, "the isError alias maps onto is_error");

        let r = parse_tool_call_result(serde_json::json!({
            "content": [ { "type": "text", "text": "hello" } ],
            "isError": false
        }))
        .expect("a payload with text content decodes");
        assert_eq!(
            r.content,
            vec![Content::text("hello")],
            "text content round-trips",
        );
        assert!(!r.is_error, "isError=false keeps is_error=false");

        let err = parse_tool_call_result(serde_json::json!("not an object"))
            .expect_err("a non-object payload does not decode");
        match err {
            ToolError::Failed(msg) => assert!(
                msg.starts_with("bad tools/call result: "),
                "the error message carries the documented prefix; got: {msg}",
            ),
            other => panic!("expected ToolError::Failed, got: {other:?}"),
        }
    }

    #[test]
    fn parse_tool_call_result_multi_block_and_snake_case() {
        let multi = parse_tool_call_result(serde_json::json!({
            "content": [
                { "type": "text", "text": "first block" },
                { "type": "json", "value": { "k": "v" } }
            ],
            "isError": false
        }))
        .expect("a multi-block payload with mixed Text+Json content decodes");
        assert_eq!(
            multi.content,
            vec![
                Content::text("first block"),
                Content::json(serde_json::json!({ "k": "v" })),
            ],
            "Vec<Content> preserves order and variant identity",
        );
        assert!(
            !multi.is_error,
            "multi-block ok results keep is_error=false"
        );

        let snake = parse_tool_call_result(serde_json::json!({ "is_error": true }))
            .expect("the canonical snake_case is_error decodes");
        assert!(
            snake.is_error,
            "snake_case is_error=true maps onto is_error=true"
        );
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

    #[test]
    fn bootstrap_error_display() {
        let bad_list = format!("{}", BootstrapError::BadList("missing field tools".into()));
        assert_eq!(
            bad_list, "malformed tools/list response: missing field tools",
            "BootstrapError::BadList Display drifted"
        );

        let duplicate = format!("{}", BootstrapError::DuplicateToolName("fs.read".into()));
        assert_eq!(
            duplicate, "duplicate remote tool name after MCP prefixing: fs.read",
            "BootstrapError::DuplicateToolName Display drifted"
        );

        assert_ne!(
            bad_list, duplicate,
            "BootstrapError::BadList and DuplicateToolName Display must not converge"
        );
    }

    #[test]
    fn bootstrap_error_transport_display() {
        let err = BootstrapError::Transport(McpClientError::Closed);
        let message = format!("{err}");
        assert_eq!(
            message, "transport: transport closed",
            "BootstrapError::Transport Display drifted"
        );
        assert!(
            message.starts_with("transport: "),
            "BootstrapError::Transport carries the 'transport: ' prefix: {message}"
        );
        assert!(
            message.contains("transport closed"),
            "BootstrapError::Transport surfaces the inner McpClientError Display, not Debug: {message}"
        );
        assert_ne!(
            message, "transport: Closed",
            "BootstrapError::Transport must not surface the bare Debug variant name: {message}"
        );
        assert!(
            !message.starts_with("malformed tools/list response:"),
            "BootstrapError::Transport must not share the BadList prefix: {message}"
        );
        assert!(
            !message.starts_with("duplicate remote tool name after MCP prefixing:"),
            "BootstrapError::Transport must not share the DuplicateToolName prefix: {message}"
        );
    }

    #[test]
    fn bootstrap_error_transport_source() {
        use std::error::Error;
        let inner = McpClientError::Closed;
        let expected_display = format!("{inner}");
        let err = BootstrapError::Transport(inner);
        let source = err
            .source()
            .expect("BootstrapError::Transport exposes the inner McpClientError via source()");
        let source_message = format!("{source}");
        assert_eq!(
            source_message, expected_display,
            "source() Display matches the inner McpClientError verbatim"
        );
        assert_eq!(
            source_message, "transport closed",
            "anchors the McpClientError::Closed Display through the wrapper"
        );
    }

    #[tokio::test]
    async fn bootstrap_rejects_non_object_initialize_response() {
        // Misconfigured-server guard: an MCP server that responds to
        // initialize with a non-object payload (string, array, null,
        // number) is incompatible with the spec. Refusing here keeps
        // the broken session from poisoning tools/list with a
        // downstream error that obscures the actual root cause.
        let client: Arc<dyn McpClient> = Arc::new(MockMcpClient::new(|method, _| match method {
            "initialize" => Ok(serde_json::json!("not an object")),
            other => happy_handler(other, &Value::Null),
        }));
        let outcome = bootstrap_remote_tools(client).await;
        match outcome {
            Err(BootstrapError::BadInitialize(message)) => {
                assert!(
                    message.contains("expected JSON object"),
                    "BadInitialize must explain WHAT was rejected: {message}"
                );
                assert!(
                    message.contains("string"),
                    "BadInitialize must surface the offending kind so operators triaging the audit log see what the server actually returned: {message}"
                );
            }
            Err(other) => panic!("expected BadInitialize, got Err({other:?})"),
            Ok(tools) => panic!(
                "expected BadInitialize, got Ok with {} tools — non-object initialize must surface as a Bootstrap rejection, not a silent success",
                tools.len()
            ),
        }
    }

    #[tokio::test]
    async fn bootstrap_accepts_object_initialize_with_mismatched_protocol_version() {
        // Compatibility guard: protocolVersion drift is non-fatal per
        // the MCP spec — the server may negotiate a different version
        // — so bootstrap must succeed. The mismatch lands in the log
        // (visible to operators via tracing) without breaking the
        // session.
        let client: Arc<dyn McpClient> = Arc::new(MockMcpClient::new(|method, _| match method {
            "initialize" => Ok(serde_json::json!({
                "protocolVersion": "1999-01-01",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "fake", "version": "0.0.1" }
            })),
            other => happy_handler(other, &Value::Null),
        }));
        let tools = bootstrap_remote_tools(client).await.expect(
            "version drift must not abort bootstrap — MCP spec allows server-side negotiation",
        );
        assert_eq!(tools.len(), 2);
    }
}
