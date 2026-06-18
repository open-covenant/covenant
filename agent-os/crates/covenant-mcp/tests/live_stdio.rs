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

#[tokio::test]
#[ignore = "live: spawns a real subprocess; opt-in via --ignored live_"]
async fn live_stdio_mcp_surfaces_rpc_error_for_unknown_tool_then_stays_usable() {
    let exe = env!("CARGO_BIN_EXE_covenant-mcp-fake-server").to_string();
    let args = vec!["--string-ids".to_string()];
    let client = StdioMcpClient::spawn(&exe, &args)
        .await
        .expect("spawn fake mcp server");
    let client_dyn: Arc<dyn McpClient> = client;

    // Keep a handle to issue a raw tools/call for a name the server never
    // advertised; bootstrap takes ownership of one clone to run the handshake.
    let raw = client_dyn.clone();
    let tools = bootstrap_remote_tools(client_dyn)
        .await
        .expect("bootstrap remote tools over real stdio");

    // A tools/call for an unknown tool comes back as a JSON-RPC error response
    // (fake_server.rs emits code -32602), which the real stdio reader must map
    // to McpClientError::Rpc — not Ok, and not a transport-level fault.
    let err = raw
        .request(
            "tools/call",
            serde_json::json!({ "name": "does-not-exist", "arguments": {} }),
        )
        .await
        .expect_err("an unknown tool must surface as a JSON-RPC error, not Ok");
    match err {
        McpClientError::Rpc { code, message } => {
            assert_eq!(
                code, -32602,
                "the Rpc error must preserve the server's JSON-RPC code verbatim \
                 (fake_server.rs emits -32602 for an unknown tool); got {code}",
            );
            assert!(
                message.contains("unknown tool"),
                "the Rpc error must carry the server's diagnostic message; got {message:?}",
            );
        }
        other => panic!("expected McpClientError::Rpc for an unknown tool, got {other:?}"),
    }

    // The error was scoped to the one request: the reader stayed in sync, so a
    // subsequent call to a real tool on the same client still succeeds. A
    // regression that desynchronized framing after an error would fail here.
    let after = tools[0]
        .call(serde_json::json!({ "text": "after-error" }))
        .await
        .expect("transport must stay usable after an rpc error on a prior request");
    assert!(!after.is_error);
    match &after.content[0] {
        Content::Text { text } => assert_eq!(text, "pong: after-error"),
        other => panic!("unexpected content after recovery: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "live: spawns a real subprocess; opt-in via --ignored live_"]
async fn live_stdio_mcp_closes_transport_on_oversized_response_line() {
    let exe = env!("CARGO_BIN_EXE_covenant-mcp-fake-server").to_string();
    let args = vec!["--oversized-response".to_string()];
    let client = StdioMcpClient::spawn(&exe, &args)
        .await
        .expect("spawn fake mcp server");
    let client_dyn: Arc<dyn McpClient> = client;
    let raw = client_dyn.clone();

    // Sanity: the transport is healthy before the flood — a normal tools/call
    // round-trips (the fake server answers `ping` without a prior handshake).
    // This proves the Closed below is caused by the oversized line, not a
    // transport that was already dead.
    let ok = raw
        .request(
            "tools/call",
            serde_json::json!({ "name": "ping", "arguments": { "text": "pre-flood" } }),
        )
        .await
        .expect("a normal tool call must succeed before the flood");
    assert_eq!(ok["content"][0]["text"], "pong: pre-flood");

    // The server answers a `flood` tools/call with a single stdout line larger
    // than the transport's 16 MiB MAX_LINE_BYTES cap and no newline.
    // read_capped_line returns InvalidData, the reader loop breaks, and —
    // because the child is still alive (not exited) — pending requests drain
    // with the clean-close message, mapping to McpClientError::Closed. A
    // regression dropping the take(max + 1) bound would instead buffer the
    // unbounded line: read_until would block on the missing newline and this
    // call would time out. Asserting Closed (and NOT Timeout / ServerCrashed /
    // Rpc) is what makes the per-line cap's bite real.
    let err = raw
        .request(
            "tools/call",
            serde_json::json!({ "name": "flood", "arguments": {} }),
        )
        .await
        .expect_err("an over-cap response line must close the transport, not return Ok");
    assert!(
        matches!(err, McpClientError::Closed),
        "an over-cap response line must surface as McpClientError::Closed (the \
         per-line cap fires and closes the transport while the child is still \
         alive); a Timeout here means the cap did not fire and the reader \
         buffered an unbounded line; got {err:?}",
    );
}
