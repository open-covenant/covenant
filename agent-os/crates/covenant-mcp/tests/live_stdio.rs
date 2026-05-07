//! Live integration test: drives `StdioMcpClient` against the in-repo
//! `covenant-mcp-fake-server` binary as a real subprocess. This is the
//! first `live_` test in the workspace per the AGENTS.md mock-vs-live rule
//! — it exercises actual stdin/stdout framing, the `tokio::process::Child`
//! lifecycle, and the JSON-RPC reader loop end-to-end.
//!
//! Marked `#[ignore]` so `cargo test` stays mock-only by default. Run with
//! `cargo test -p covenant-mcp -- --ignored live_`.

use covenant_mcp::external::bootstrap_remote_tools;
use covenant_mcp::transport::{McpClient, StdioMcpClient};
use covenant_mcp::Content;
use std::sync::Arc;

#[tokio::test]
#[ignore = "live: spawns a real subprocess; opt-in via --ignored live_"]
async fn live_stdio_mcp_initialize_lists_and_calls() {
    let exe = env!("CARGO_BIN_EXE_covenant-mcp-fake-server").to_string();
    let client = StdioMcpClient::spawn(&exe, &[])
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
