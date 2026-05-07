//! Covenant command-line client for the local daemon.
//!
//! ```text
//!   covenant ping
//!   covenant intent <text>
//!   covenant memory recent [--tier <working|episodic|longterm>] [--limit N]
//!   covenant memory search <query>
//!   covenant memory purge [--tier <T>] (--before-ms <M> | --older-than-ms <D>)
//!   covenant capabilities recent [--limit N]
//!   covenant capabilities grant <action>
//!   covenant capabilities revoke <signature-b58>
//!   covenant receipts recent [--limit N]
//!   covenant verify [--window N]
//!   covenant ignore check <text>
//!   covenant tools list
//!   covenant tools call <name> [--args <json>]
//! ```

#![deny(unsafe_code)]

use anyhow::{bail, Context, Result};
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_types::MemoryTier;
use std::path::PathBuf;
use tokio::net::UnixStream;

fn covenant_home() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("COVENANT_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".covenant"))
}

fn print_usage() {
    eprintln!("covenant — agent-native operating layer CLI");
    eprintln!();
    eprintln!("usage:");
    eprintln!("  covenant intent <text>                  submit an intent and print the result");
    eprintln!("  covenant ping                           check the daemon is responsive");
    eprintln!(
        "  covenant memory recent [--tier T] [-n N]               list recent memory records"
    );
    eprintln!(
        "  covenant memory search <query> [--tier T] [-n N]       semantic search via embeddings"
    );
    eprintln!(
        "  covenant memory purge [--tier T] (--before-ms M | --older-than-ms D)  delete records older than ms epoch / D ms ago"
    );
    eprintln!("  covenant receipts recent [-n N]         list recent settlement receipts");
    eprintln!("  covenant ignore check <text>            test text against .covenantignore rules");
    eprintln!("  covenant tools list                     list registered tools");
    eprintln!("  covenant tools call <name> [--args <json>]   invoke a registered tool");
}

async fn print_memory_response(stream: &mut UnixStream) -> Result<()> {
    match read_frame::<_, Response>(stream).await? {
        Response::Memories { records } => {
            if records.is_empty() {
                println!("(no records)");
            }
            for r in records {
                let tier = match r.tier {
                    MemoryTier::Working => "working",
                    MemoryTier::Episodic => "episodic",
                    MemoryTier::LongTerm => "longterm",
                };
                println!("[{}] {tier}: {}", r.created_at, r.text);
            }
            Ok(())
        }
        Response::Error { message } => bail!("daemon error: {message}"),
        other => bail!("unexpected response: {other:?}"),
    }
}

fn parse_tier(s: &str) -> Result<MemoryTier> {
    match s {
        "working" => Ok(MemoryTier::Working),
        "episodic" => Ok(MemoryTier::Episodic),
        "longterm" | "long-term" | "long_term" => Ok(MemoryTier::LongTerm),
        other => bail!("unknown tier '{other}' (expected working|episodic|longterm)"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print_usage();
        std::process::exit(2);
    }

    let sock = covenant_home()?.join("sock");
    let mut stream = UnixStream::connect(&sock).await.with_context(|| {
        format!(
            "connect to daemon at {} (is covenantd running?)",
            sock.display()
        )
    })?;

    match args[0].as_str() {
        "ping" => {
            write_frame(&mut stream, &Request::Ping).await?;
            match read_frame::<_, Response>(&mut stream).await? {
                Response::Pong => println!("pong"),
                Response::Error { message } => bail!("daemon error: {message}"),
                other => bail!("unexpected response: {other:?}"),
            }
        }
        "intent" => {
            if args.len() < 2 {
                eprintln!("covenant intent: missing intent text");
                std::process::exit(2);
            }
            let text = args[1..].join(" ");
            write_frame(&mut stream, &Request::SubmitIntent { text }).await?;
            match read_frame::<_, Response>(&mut stream).await? {
                Response::IntentResult { text, .. } => println!("{text}"),
                Response::Error { message } => bail!("daemon error: {message}"),
                other => bail!("unexpected response: {other:?}"),
            }
        }
        "memory" => {
            if args.len() < 2 {
                print_usage();
                std::process::exit(2);
            }
            match args[1].as_str() {
                "recent" => {
                    let mut tier: Option<MemoryTier> = None;
                    let mut limit: usize = 10;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--tier" => {
                                i += 1;
                                let v = args.get(i).context("--tier needs a value")?;
                                tier = Some(parse_tier(v)?);
                            }
                            "-n" | "--limit" => {
                                i += 1;
                                let v = args.get(i).context("--limit needs a value")?;
                                limit = v.parse().context("--limit must be an integer")?;
                            }
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(&mut stream, &Request::RecentMemory { tier, limit }).await?;
                    print_memory_response(&mut stream).await?;
                }
                "purge" => {
                    let mut tier: Option<MemoryTier> = None;
                    let mut before_ms: Option<u64> = None;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--tier" => {
                                i += 1;
                                let v = args.get(i).context("--tier needs a value")?;
                                tier = Some(parse_tier(v)?);
                            }
                            "--before-ms" => {
                                i += 1;
                                let v = args.get(i).context("--before-ms needs a value")?;
                                before_ms = Some(
                                    v.parse()
                                        .context("--before-ms must be an integer (epoch ms)")?,
                                );
                            }
                            "--older-than-ms" => {
                                i += 1;
                                let v = args.get(i).context("--older-than-ms needs a value")?;
                                let dur: u64 =
                                    v.parse().context("--older-than-ms must be an integer")?;
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as u64)
                                    .unwrap_or(0);
                                before_ms = Some(now.saturating_sub(dur));
                            }
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let before_ms = before_ms.context("missing --before-ms or --older-than-ms")?;
                    write_frame(&mut stream, &Request::PurgeMemory { tier, before_ms }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::MemoryPurged { purged } => {
                            println!("purged {purged} record(s)");
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "search" => {
                    if args.len() < 3 {
                        eprintln!("covenant memory search: missing <query>");
                        std::process::exit(2);
                    }
                    let mut tier: Option<MemoryTier> = None;
                    let mut limit: usize = 10;
                    let mut query_parts: Vec<String> = Vec::new();
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--tier" => {
                                i += 1;
                                let v = args.get(i).context("--tier needs a value")?;
                                tier = Some(parse_tier(v)?);
                            }
                            "-n" | "--limit" => {
                                i += 1;
                                let v = args.get(i).context("--limit needs a value")?;
                                limit = v.parse().context("--limit must be an integer")?;
                            }
                            other => query_parts.push(other.to_string()),
                        }
                        i += 1;
                    }
                    let query = query_parts.join(" ");
                    if query.is_empty() {
                        bail!("query text is required");
                    }
                    write_frame(&mut stream, &Request::SearchMemory { query, tier, limit }).await?;
                    print_memory_response(&mut stream).await?;
                }
                other => {
                    eprintln!("covenant memory: unknown subcommand '{other}'");
                    print_usage();
                    std::process::exit(2);
                }
            }
        }
        "capabilities" => {
            if args.len() < 2 {
                print_usage();
                std::process::exit(2);
            }
            match args[1].as_str() {
                "recent" => {
                    let mut limit: usize = 10;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "-n" | "--limit" => {
                                i += 1;
                                let v = args.get(i).context("--limit needs a value")?;
                                limit = v.parse().context("--limit must be an integer")?;
                            }
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(&mut stream, &Request::RecentCapabilities { limit }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::Capabilities { capabilities } => {
                            if capabilities.is_empty() {
                                println!("(no capabilities granted)");
                            }
                            for c in capabilities {
                                let exp = match c.capability.expires_at {
                                    Some(ms) => format!("expires {ms}"),
                                    None => "perpetual".into(),
                                };
                                println!(
                                    "{} → {} ({}) [{}]",
                                    c.capability.subject.display,
                                    c.capability.action,
                                    c.capability.granted_by.display,
                                    exp
                                );
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "grant" => {
                    if args.len() < 3 {
                        eprintln!("covenant capabilities grant: missing <action>");
                        std::process::exit(2);
                    }
                    let action = args[2].clone();
                    let mut expires_at: Option<u64> = None;
                    let mut i = 3;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--expires-at" => {
                                i += 1;
                                let v = args.get(i).context("--expires-at needs a value")?;
                                expires_at = Some(
                                    v.parse()
                                        .context("--expires-at must be an integer (epoch ms)")?,
                                );
                            }
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(
                        &mut stream,
                        &Request::GrantCapability {
                            action,
                            scope: None,
                            expires_at,
                        },
                    )
                    .await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::CapabilityGranted {
                            signature_b58,
                            subject_display,
                            action,
                        } => {
                            println!("granted: {subject_display} → {action}");
                            println!("signature: {signature_b58}");
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "revoke" => {
                    if args.len() < 3 {
                        eprintln!("covenant capabilities revoke: missing <signature-b58>");
                        std::process::exit(2);
                    }
                    let signature_b58 = args[2].clone();
                    write_frame(
                        &mut stream,
                        &Request::RevokeCapability {
                            signature_b58: signature_b58.clone(),
                        },
                    )
                    .await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::CapabilityRevoked { removed, .. } => {
                            if removed {
                                println!("revoked: {signature_b58}");
                            } else {
                                println!("(no live capability with that signature)");
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                other => {
                    eprintln!("covenant capabilities: unknown subcommand '{other}'");
                    print_usage();
                    std::process::exit(2);
                }
            }
        }
        "receipts" => {
            if args.len() < 2 || args[1] != "recent" {
                print_usage();
                std::process::exit(2);
            }
            let mut limit: usize = 10;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "-n" | "--limit" => {
                        i += 1;
                        let v = args.get(i).context("--limit needs a value")?;
                        limit = v.parse().context("--limit must be an integer")?;
                    }
                    other => bail!("unknown flag '{other}'"),
                }
                i += 1;
            }
            write_frame(&mut stream, &Request::RecentReceipts { limit }).await?;
            match read_frame::<_, Response>(&mut stream).await? {
                Response::Receipts { receipts } => {
                    if receipts.is_empty() {
                        println!("(no receipts)");
                    }
                    for r in receipts {
                        let resource = match r.resource {
                            covenant_types::ResourceKind::Compute => "compute",
                            covenant_types::ResourceKind::Memory => "memory",
                            covenant_types::ResourceKind::Tool => "tool",
                            covenant_types::ResourceKind::Message => "message",
                            covenant_types::ResourceKind::Registration => "registration",
                        };
                        let onchain = match &r.onchain_sig {
                            Some(s) => s.as_str(),
                            None => "(local-only)",
                        };
                        println!(
                            "[{}] {resource}: {} credits — {onchain}",
                            r.settled_at, r.credits_consumed
                        );
                    }
                }
                Response::Error { message } => bail!("daemon error: {message}"),
                other => bail!("unexpected response: {other:?}"),
            }
        }
        "verify" => {
            let mut window: usize = 100;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--window" | "-w" => {
                        i += 1;
                        let v = args.get(i).context("--window needs a value")?;
                        window = v.parse().context("--window must be an integer")?;
                    }
                    other => bail!("unknown flag '{other}'"),
                }
                i += 1;
            }
            write_frame(&mut stream, &Request::Verify { window }).await?;
            match read_frame::<_, Response>(&mut stream).await? {
                Response::VerifyReport {
                    window,
                    checks,
                    orphans_total,
                } => {
                    println!("verify (last {window} records):");
                    for c in &checks {
                        let mark = if c.passed { "✓" } else { "✗" };
                        println!("  {mark} {} — {}", c.name, c.message);
                    }
                    println!("orphans total: {orphans_total}");
                    if orphans_total > 0 {
                        std::process::exit(1);
                    }
                }
                Response::Error { message } => bail!("daemon error: {message}"),
                other => bail!("unexpected response: {other:?}"),
            }
        }
        "tools" => {
            if args.len() < 2 {
                print_usage();
                std::process::exit(2);
            }
            match args[1].as_str() {
                "list" => {
                    write_frame(&mut stream, &Request::ListTools).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::ToolList { tools } => {
                            if tools.is_empty() {
                                println!("(no tools registered)");
                            }
                            for t in tools {
                                println!("{} — {}", t.name, t.description);
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "call" => {
                    if args.len() < 3 {
                        eprintln!("covenant tools call: missing <name>");
                        std::process::exit(2);
                    }
                    let name = args[2].clone();
                    let mut arguments = serde_json::Value::Null;
                    let mut i = 3;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--args" => {
                                i += 1;
                                let v = args.get(i).context("--args needs a value")?;
                                arguments =
                                    serde_json::from_str(v).context("--args must be valid JSON")?;
                            }
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(
                        &mut stream,
                        &Request::CallTool {
                            name: name.clone(),
                            arguments,
                        },
                    )
                    .await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::ToolResult { content, is_error } => {
                            for c in &content {
                                match c {
                                    covenant_mcp::Content::Text { text } => println!("{text}"),
                                    covenant_mcp::Content::Json { value } => {
                                        println!("{}", serde_json::to_string_pretty(value)?);
                                    }
                                }
                            }
                            if is_error {
                                std::process::exit(1);
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                other => {
                    eprintln!("covenant tools: unknown subcommand '{other}'");
                    print_usage();
                    std::process::exit(2);
                }
            }
        }
        "ignore" => {
            if args.len() < 2 || args[1] != "check" {
                eprintln!("covenant ignore: expected `check <text>`");
                std::process::exit(2);
            }
            if args.len() < 3 {
                eprintln!("covenant ignore check: missing <text>");
                std::process::exit(2);
            }
            let text = args[2..].join(" ");
            write_frame(&mut stream, &Request::IgnoreCheck { text }).await?;
            match read_frame::<_, Response>(&mut stream).await? {
                Response::IgnoreReport {
                    ignored,
                    matched_pattern,
                    rules_loaded,
                } => {
                    if ignored {
                        let pat = matched_pattern.as_deref().unwrap_or("(none)");
                        println!("ignored — matched rule: {pat}");
                    } else {
                        println!("not ignored ({rules_loaded} rule(s) loaded)");
                    }
                    if ignored {
                        std::process::exit(1);
                    }
                }
                Response::Error { message } => bail!("daemon error: {message}"),
                other => bail!("unexpected response: {other:?}"),
            }
        }
        other => {
            eprintln!("covenant: unknown command '{other}'");
            print_usage();
            std::process::exit(2);
        }
    }
    Ok(())
}
