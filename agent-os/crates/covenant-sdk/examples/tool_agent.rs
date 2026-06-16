//! A tool-using agent: connect, discover the daemon's tools, and call one.
//!
//! ```bash
//! cargo run -p covenant-sdk --example tool_agent
//! ```
//!
//! Start `covenantd` first so the socket and operator token exist.

use covenant_sdk::{Client, Content, SdkError};

#[tokio::main]
async fn main() -> Result<(), SdkError> {
    let mut client = Client::connect_default().await?;
    println!("connected as {}", client.identity());

    let tools = client.list_tools().await?;
    if tools.is_empty() {
        println!("the daemon's router advertises no tools");
        return Ok(());
    }
    for tool in &tools {
        println!("- {}: {}", tool.name, tool.description);
    }

    // Call the first advertised tool with empty arguments. A real agent picks
    // the tool and builds arguments from its task; the daemon enforces the
    // capability and returns Err here if the call is not permitted.
    let target = &tools[0];
    println!("\ncalling {}", target.name);
    let outcome = client
        .call_tool(&target.name, serde_json::json!({}))
        .await?;

    // `is_error` is a tool-level failure (bad arguments, upstream error), which
    // is distinct from a daemon-level rejection that would surface as Err above.
    if outcome.is_error {
        eprintln!("{} reported a tool error", target.name);
    }
    for block in &outcome.content {
        match block {
            Content::Text { text } => println!("{text}"),
            Content::Json { value } => println!("{value}"),
        }
    }
    Ok(())
}
