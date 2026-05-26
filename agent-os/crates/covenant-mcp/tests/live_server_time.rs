//! Live integration test: drives `StdioMcpClient` against the vendored upstream
//! `mcp-server-time` reference server (see `agent-os/vendor/mcp-server-time/`)
//! as a real subprocess, exercising Covenant's external MCP stdio transport
//! against an officially-maintained third-party server.
//!
//! Default-off twice over: `#[ignore]` keeps it out of `cargo test`, and it
//! additionally returns early unless `COVENANT_LIVE_MCP_SERVER_TIME` holds the
//! server launch command. A default `cargo test` neither builds nor runs the
//! fixture. Run it against the vendored source with:
//!
//! ```text
//! COVENANT_LIVE_MCP_SERVER_TIME="uv run --project agent-os/vendor/mcp-server-time/a1e5a9a9/time python -m mcp_server_time" \
//!   cargo test -p covenant-mcp -- --ignored live_server_time
//! ```
//!
//! The server reads only the system clock and the IANA timezone database; the
//! test performs no network access.

use covenant_mcp::external::bootstrap_remote_tools;
use covenant_mcp::transport::{McpClient, StdioMcpClient};
use covenant_mcp::Content;
use std::sync::Arc;

const LAUNCH_ENV: &str = "COVENANT_LIVE_MCP_SERVER_TIME";

/// Dependency-free structural check that `s` opens with an ISO-8601
/// `YYYY-MM-DDTHH:MM:SS` instant (offset/fractional suffixes are allowed to
/// follow). Avoids pulling a date crate into the test surface.
fn looks_like_iso8601(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 19 {
        return false;
    }
    let digit = |i: usize| b[i].is_ascii_digit();
    (0..4).all(digit)
        && b[4] == b'-'
        && digit(5)
        && digit(6)
        && b[7] == b'-'
        && digit(8)
        && digit(9)
        && b[10] == b'T'
        && digit(11)
        && digit(12)
        && b[13] == b':'
        && digit(14)
        && digit(15)
        && b[16] == b':'
        && digit(17)
        && digit(18)
}

#[tokio::test]
#[ignore = "live: spawns the vendored mcp-server-time subprocess; opt-in via COVENANT_LIVE_MCP_SERVER_TIME + --ignored live_"]
async fn live_server_time_get_current_time_returns_iso8601() {
    let Ok(launch) = std::env::var(LAUNCH_ENV) else {
        eprintln!("{LAUNCH_ENV} unset; skipping vendored mcp-server-time live test");
        return;
    };
    let mut parts = launch.split_whitespace();
    let exe = parts
        .next()
        .unwrap_or_else(|| panic!("{LAUNCH_ENV} is empty; expected a server launch command"));
    let args: Vec<String> = parts.map(str::to_string).collect();

    let client = StdioMcpClient::spawn(exe, &args)
        .await
        .expect("spawn vendored mcp-server-time");
    let client_dyn: Arc<dyn McpClient> = client;

    let tools = bootstrap_remote_tools(client_dyn)
        .await
        .expect("bootstrap mcp-server-time tools over real stdio");

    let get_current_time = tools
        .iter()
        .find(|tool| tool.name() == "get_current_time")
        .expect("server-time exposes get_current_time");

    let result = get_current_time
        .call(serde_json::json!({ "timezone": "UTC" }))
        .await
        .expect("get_current_time call over real stdio");
    assert!(!result.is_error, "get_current_time returned an error result");

    let payload = result
        .content
        .first()
        .expect("get_current_time returned content");
    let datetime = match payload {
        Content::Text { text } => {
            let value: serde_json::Value =
                serde_json::from_str(text).expect("get_current_time text content parses as JSON");
            value["datetime"]
                .as_str()
                .expect("datetime field is a string")
                .to_string()
        }
        Content::Json { value } => value["datetime"]
            .as_str()
            .expect("datetime field is a string")
            .to_string(),
    };
    assert!(
        looks_like_iso8601(&datetime),
        "expected an ISO-8601 datetime from get_current_time, got {datetime:?}"
    );
}
