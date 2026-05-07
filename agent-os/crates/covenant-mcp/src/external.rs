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
}

/// Run `initialize` → `notifications/initialized` → `tools/list` against
/// `client` and return one [`Tool`] per advertised spec, all sharing the
/// same client.
pub async fn bootstrap_remote_tools(
    client: Arc<dyn McpClient>,
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
    for spec in parsed.tools {
        out.push(Arc::new(RemoteTool {
            client: client.clone(),
            spec,
        }));
    }
    Ok(out)
}

#[derive(Deserialize)]
struct ToolsListResponse {
    tools: Vec<ToolSpec>,
}

pub struct RemoteTool {
    client: Arc<dyn McpClient>,
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
        let params = serde_json::json!({ "name": self.spec.name, "arguments": arguments });
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
