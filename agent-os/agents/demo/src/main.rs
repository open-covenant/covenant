//! Demo agent for the public sandbox.
//!
//! Reads one JSON line on stdin (`Intent`), writes one JSON line on stdout
//! (`{"text": ..., "sources": [...]}`). Calls Anthropic Haiku via
//! covenant-llm with a tight system prompt that keeps replies short and
//! grounded in what this sandbox can actually demonstrate.
//!
//! Three guard rails so a public URL can't drain the API key:
//! - Daily request quota, persisted to `$COVENANT_HOME/demo-quota.json`,
//!   resets at UTC day boundary. Cap is `COVENANT_DEMO_DAILY_REQUESTS`
//!   (default 500).
//! - Missing `ANTHROPIC_API_KEY` returns a canned, honest fallback that
//!   still surfaces the rest of Covenant (routing, capability check,
//!   audit log).
//! - Any Anthropic error returns a polite "try again" reply with the
//!   error logged to stderr.

use covenant_llm::{AnthropicProvider, ChatMessage, Provider};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct Intent {
    text: String,
}

#[derive(Debug, Serialize)]
struct AgentResult {
    text: String,
    sources: Vec<String>,
}

const SYSTEM_PROMPT: &str = "You are the demo agent on Covenant's public sandbox. \
Reply briefly — 2 to 4 sentences. You're running as a sandboxed agent inside a Covenant daemon \
that signs and logs every step (capability check, dispatch, your reply). When it fits the user's \
question, mention that they can open the activity log to see this exchange signed. \
You have no internet, no file access, and no memory of prior turns on this sandbox. \
If asked what you can do: say you're a Haiku-backed demo for showing how Covenant routes, signs, \
and audits tasks, and that the real value shows up when the user runs Covenant locally and plugs \
in their own agents.";

const DEFAULT_MODEL: &str = "claude-haiku-4-5-20251001";
const DEFAULT_DAILY_REQUESTS: u32 = 500;

const QUOTA_REACHED_REPLY: &str =
    "This public sandbox has hit its daily request budget. Try again tomorrow, or install \
Covenant locally — opencovenant.org/docs/getting-started — and plug in your own model key.";

const NO_KEY_REPLY: &str = "The demo model isn't configured on this sandbox right now. Covenant \
still routed your task, checked permissions, and logged the dispatch — open the activity log to \
see those steps signed.";

const TRANSIENT_ERROR_REPLY: &str = "Couldn't reach the demo model just now. Your task was still \
routed and signed — try again in a moment.";

fn covenant_home() -> PathBuf {
    if let Ok(p) = std::env::var("COVENANT_HOME") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".covenant")
}

fn today_key() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0)
}

/// Increment today's request count; return Err(()) if today's cap is already met.
/// Best-effort persistence — a write failure logs to stderr but doesn't block the request.
fn try_charge_quota() -> Result<(), ()> {
    let cap: u32 = std::env::var("COVENANT_DEMO_DAILY_REQUESTS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_DAILY_REQUESTS);

    let path = covenant_home().join("demo-quota.json");
    let today = today_key();
    let mut count: u32 = 0;

    if let Ok(s) = fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
            if v.get("day").and_then(|d| d.as_u64()) == Some(today) {
                count = v.get("count").and_then(|c| c.as_u64()).unwrap_or(0) as u32;
            }
        }
    }

    if count >= cap {
        return Err(());
    }

    let next = serde_json::json!({ "day": today, "count": count + 1 });
    if let Err(e) = fs::write(&path, next.to_string()) {
        eprintln!("demo-agent: quota persist failed: {e}");
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    let intent: Intent = serde_json::from_str(buf.trim()).map_err(std::io::Error::other)?;

    if try_charge_quota().is_err() {
        return emit(QUOTA_REACHED_REPLY);
    }

    let key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
    if key.is_empty() {
        return emit(NO_KEY_REPLY);
    }
    let model = std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.into());

    let provider = AnthropicProvider::new(key, model);
    let messages = vec![
        ChatMessage::system(SYSTEM_PROMPT),
        ChatMessage::user(intent.text),
    ];

    match provider.complete(&messages).await {
        Ok(reply) => emit(&reply),
        Err(e) => {
            eprintln!("demo-agent: anthropic call failed: {e}");
            emit(TRANSIENT_ERROR_REPLY)
        }
    }
}

fn emit(text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let result = AgentResult {
        text: text.into(),
        sources: vec![],
    };
    let line = serde_json::to_string(&result).map_err(std::io::Error::other)?;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(line.as_bytes())?;
    stdout.write_all(b"\n")?;
    Ok(())
}
