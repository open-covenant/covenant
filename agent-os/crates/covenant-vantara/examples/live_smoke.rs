//! Live Vantara smoke test against the real explorer. Read-only, no keys,
//! no spend — run by hand:
//!
//!   cargo run -p covenant-vantara --example live_smoke
//!
//! Override the host with COVENANT_VANTARA_BASE_URL. Exercises the three
//! registered tools end-to-end: reads the settlement ledger, lists recent
//! provenance records with their attestations, and — when the feed has a
//! record — attests it by its own output hash to prove the round-trip.

use std::sync::Arc;

use covenant_mcp::{Content, Tool};
use covenant_vantara::{
    vantara_tools, VantaraClient, VantaraConfig, ATTEST_TOOL, JOBS_TOOL, PAYOUTS_TOOL,
};
use serde_json::{json, Value};

#[tokio::main]
async fn main() {
    let base = std::env::var("COVENANT_VANTARA_BASE_URL")
        .unwrap_or_else(|_| covenant_vantara::BASE_URL.to_string());
    // Anchor verification to the provider key published in the MPP doc, the
    // same key the daemon pins to.
    let client = match VantaraClient::new(&base).provider_key_from_mpp().await {
        Ok(key) => {
            println!("anchored to MPP provider key: {key}\n");
            Arc::new(VantaraClient::with_pinned_key(&base, key))
        }
        Err(e) => {
            println!("MPP provider key unresolved ({e}); using the self-describing feed key\n");
            Arc::new(VantaraClient::new(&base))
        }
    };
    let cfg = VantaraConfig {
        enabled: true,
        base_url: base.clone(),
        ..Default::default()
    };
    let tools = vantara_tools(client, &cfg);
    let tool = |name: &str| {
        tools
            .iter()
            .find(|t| t.name() == name)
            .expect("tool registered")
            .clone()
    };

    println!("== vantara explorer: {base} ==\n");

    println!("-- {PAYOUTS_TOOL} (on-chain settlement anchor) --");
    print_result(&*tool(PAYOUTS_TOOL), json!({ "limit": 2 })).await;

    println!("\n-- {JOBS_TOOL} (provenance feed + attestations) --");
    let jobs_res = tool(JOBS_TOOL)
        .call(json!({ "limit": 20 }))
        .await
        .expect("jobs call");
    print_verification_summary(&jobs_res);
    let first_hash = first_output_hash(&jobs_res);
    print_call_result(&jobs_res);

    match first_hash {
        Some(hash) => {
            println!("\n-- {ATTEST_TOOL} outputHash={hash} --");
            print_result(&*tool(ATTEST_TOOL), json!({ "outputHash": hash })).await;
        }
        None => {
            println!("\n(feed currently empty — nothing to attest; it fills as the network runs)")
        }
    }
}

async fn print_result(tool: &dyn Tool, args: Value) {
    let res = tool.call(args).await.expect("tool call");
    print_call_result(&res);
}

/// One line per attestation: the verification result the connector produced
/// for each live record.
fn print_verification_summary(res: &covenant_mcp::ToolCallResult) {
    let Some(Content::Json { value }) = res.content.get(1) else {
        return;
    };
    let Some(atts) = value.get("attestations").and_then(Value::as_array) else {
        return;
    };
    for a in atts {
        let job = a.get("jobId").and_then(Value::as_str).unwrap_or("?");
        let sig = a
            .get("signature")
            .and_then(|s| s.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("?");
        let settled = a
            .get("settlement")
            .and_then(|s| s.get("settled"))
            .and_then(Value::as_bool);
        println!(
            "  {}  signature={sig}  settled={settled:?}",
            &job[..job.len().min(8)]
        );
    }
}

fn print_call_result(res: &covenant_mcp::ToolCallResult) {
    println!("is_error: {}", res.is_error);
    for block in &res.content {
        match block {
            Content::Json { value } => println!("{}", serde_json::to_string_pretty(value).unwrap()),
            Content::Text { text } => println!("{text}"),
        }
    }
}

fn first_output_hash(res: &covenant_mcp::ToolCallResult) -> Option<String> {
    let Content::Json { value } = res.content.first()? else {
        return None;
    };
    value
        .get("jobs")?
        .as_array()?
        .first()?
        .get("outputHash")?
        .as_str()
        .map(str::to_string)
}
