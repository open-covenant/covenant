//! A memory-using agent: search the caller's memory, then list recent notes.
//!
//! ```bash
//! cargo run -p covenant-sdk --example memory_agent
//! ```
//!
//! Memory is read-only over IPC — the daemon writes it as a side effect of
//! intent execution, so this agent reads what earlier work recorded.

use covenant_sdk::{Client, MemoryTier, SdkError};

#[tokio::main]
async fn main() -> Result<(), SdkError> {
    let mut client = Client::connect_default().await?;

    // Semantic search across the working tier with a relevance floor, so only
    // reasonably close matches come back.
    let hits = client
        .search_memory(
            "deployment checklist",
            Some(MemoryTier::Working),
            5,
            Some(0.2),
        )
        .await?;
    println!("search returned {} record(s)", hits.len());
    for record in &hits {
        println!("- [{:?}] {}", record.tier, record.text);
    }

    // Recent notes, newest first, across every tier.
    let recent = client.recent_memory(None, 10).await?;
    println!("\n{} recent record(s)", recent.len());
    for record in &recent {
        println!("- [{:?}] {}", record.tier, record.text);
    }
    Ok(())
}
