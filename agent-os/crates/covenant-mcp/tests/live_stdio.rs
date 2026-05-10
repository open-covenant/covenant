//! Live integration test: drives `StdioMcpClient` against the in-repo
//! `covenant-mcp-fake-server` binary as a real subprocess. This is the
//! first `live_` test in the workspace per the AGENTS.md mock-vs-live rule
//! — it exercises actual stdin/stdout framing, the `tokio::process::Child`
//! lifecycle, and the JSON-RPC reader loop end-to-end.
//!
//! Marked `#[ignore]` so `cargo test` stays mock-only by default. Run with
//! `cargo test -p covenant-mcp -- --ignored live_`.

use covenant_mcp::external::{bootstrap_remote_tools, BootstrapError};
use covenant_mcp::transport::{McpClient, McpClientError, StdioMcpClient};
use covenant_mcp::Content;
use std::sync::Arc;

#[tokio::test]
#[ignore = "live: spawns a real subprocess; opt-in via --ignored live_"]
async fn live_stdio_mcp_initialize_lists_and_calls() {
    let exe = env!("CARGO_BIN_EXE_covenant-mcp-fake-server").to_string();
    let args = vec!["--string-ids".to_string(), "--stderr-noise".to_string()];
    let client = StdioMcpClient::spawn(&exe, &args)
        .await
        .expect("spawn fake mcp server");
    let client_dyn: Arc<dyn McpClient> = client;

    let tools = bootstrap_remote_tools(client_dyn)
        .await
        .expect("bootstrap remote tools over real stdio");

    let names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
    assert_eq!(names, vec!["ping".to_string()]);
    assert!(tools[0].description().contains("pong"));

    let r = tools[0]
        .call(serde_json::json!({ "text": "hello" }))
        .await
        .expect("tools/call over real stdio");
    assert!(!r.is_error);
    match &r.content[0] {
        Content::Text { text } => assert_eq!(text, "pong: hello"),
        other => panic!("unexpected content: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "live: spawns a real subprocess; opt-in via --ignored live_"]
async fn live_stdio_mcp_handles_split_stdout_response() {
    let exe = env!("CARGO_BIN_EXE_covenant-mcp-fake-server").to_string();
    let args = vec!["--split-response".to_string()];
    let client = StdioMcpClient::spawn(&exe, &args)
        .await
        .expect("spawn fake mcp server");
    let client_dyn: Arc<dyn McpClient> = client;

    let tools = bootstrap_remote_tools(client_dyn)
        .await
        .expect("bootstrap remote tools over split stdio response");
    let r = tools[0]
        .call(serde_json::json!({ "text": "delayed" }))
        .await
        .expect("tools/call over split stdio response");

    match &r.content[0] {
        Content::Text { text } => assert_eq!(text, "pong: delayed"),
        other => panic!("unexpected content: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "live: spawns a real subprocess; opt-in via --ignored live_"]
async fn live_stdio_mcp_handles_large_tool_payload() {
    let exe = env!("CARGO_BIN_EXE_covenant-mcp-fake-server").to_string();
    let args: Vec<String> = Vec::new();
    let client = StdioMcpClient::spawn(&exe, &args)
        .await
        .expect("spawn fake mcp server");
    let client_dyn: Arc<dyn McpClient> = client;

    let tools = bootstrap_remote_tools(client_dyn)
        .await
        .expect("bootstrap remote tools over real stdio");
    let payload = "x".repeat(64 * 1024);
    let r = tools[0]
        .call(serde_json::json!({ "text": payload }))
        .await
        .expect("tools/call with large payload over real stdio");

    match &r.content[0] {
        Content::Text { text } => {
            assert_eq!(text.len(), "pong: ".len() + payload.len());
            assert!(text.starts_with("pong: "));
            assert!(text.ends_with(&payload));
        }
        other => panic!("unexpected content: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "live: spawns a real subprocess; opt-in via --ignored live_"]
async fn live_stdio_mcp_handles_multi_tool_json_and_error_results() {
    let exe = env!("CARGO_BIN_EXE_covenant-mcp-fake-server").to_string();
    let args = vec!["--multi-tool".to_string(), "--string-ids".to_string()];
    let client = StdioMcpClient::spawn(&exe, &args)
        .await
        .expect("spawn fake mcp server");
    let client_dyn: Arc<dyn McpClient> = client;

    let tools = bootstrap_remote_tools(client_dyn)
        .await
        .expect("bootstrap multi-tool remote tools over real stdio");
    let names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
    assert_eq!(
        names,
        vec!["ping".to_string(), "sum".to_string(), "fail".to_string()]
    );

    let sum = tools
        .iter()
        .find(|tool| tool.name() == "sum")
        .expect("sum tool");
    let sum_result = sum
        .call(serde_json::json!({ "a": 2, "b": 5 }))
        .await
        .expect("sum tool call over real stdio");
    assert!(!sum_result.is_error);
    match &sum_result.content[0] {
        Content::Json { value } => assert_eq!(value["sum"], 7),
        other => panic!("unexpected sum content: {other:?}"),
    }

    let fail = tools
        .iter()
        .find(|tool| tool.name() == "fail")
        .expect("fail tool");
    let fail_result = fail
        .call(serde_json::json!({}))
        .await
        .expect("tool-level errors stay in successful JSON-RPC responses");
    assert!(fail_result.is_error);
    match &fail_result.content[0] {
        Content::Text { text } => assert_eq!(text, "forced tool failure"),
        other => panic!("unexpected failure content: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "live: spawns a real subprocess; opt-in via --ignored live_"]
async fn live_stdio_mcp_surfaces_transport_closed_when_server_exits() {
    let exe = env!("CARGO_BIN_EXE_covenant-mcp-fake-server").to_string();
    let args = vec!["--exit-after-initialize".to_string()];
    let client = StdioMcpClient::spawn(&exe, &args)
        .await
        .expect("spawn fake mcp server");
    let client_dyn: Arc<dyn McpClient> = client;

    let err = match bootstrap_remote_tools(client_dyn).await {
        Ok(_) => panic!("bootstrap should fail when server exits after initialize"),
        Err(err) => err,
    };
    let transport = match err {
        BootstrapError::Transport(e) => e,
        other => panic!("unexpected bootstrap error: {other:?}"),
    };
    assert!(matches!(transport, McpClientError::Closed));
}
