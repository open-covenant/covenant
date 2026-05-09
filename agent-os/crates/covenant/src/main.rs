//! Covenant command-line client for the local daemon.
//!
//! ```text
//!   covenant ping
//!   covenant intent <text>
//!   covenant memory recent [--tier <working|episodic|longterm>] [--limit N]
//!   covenant memory search <query>
//!   covenant memory purge [--tier <T>] (--before-ms <M> | --older-than-ms <D>)
//!   covenant capabilities recent [--limit N]
//!   covenant capabilities grant <action>          (auto-expands `a2a.{send,recv,respond}.<pubkey-prefix>` to full b58)
//!   covenant capabilities revoke <signature-b58>
//!   covenant capabilities purge (--before-ms <M> | --older-than-ms <D>)
//!   covenant receipts recent [--limit N]
//!   covenant chain status
//!   covenant chain flush-receipts [--limit N]
//!   covenant chain receipt-batches [--limit N]
//!   covenant verify [--window N]
//!   covenant ignore check <text>
//!   covenant tools list
//!   covenant tools call <name> [--args <json>]
//!   covenant audit recent [--limit N]
//!   covenant a2a status [--limit N]
//!   covenant a2a compact
//!   covenant peers purge (--before-ms <M> | --older-than-ms <D>)
//!   covenant peers rotate
//!   covenant peers list [--limit N] [--prefix <pubkey-b58-prefix>]
//!   covenant peers revoke <token-prefix> [--force] [--limit-matches <N>]
//!   covenant intents resume <intent-id>
//! ```

#![deny(unsafe_code)]

use anyhow::{bail, Context, Result};
use covenant_ipc::{read_frame, write_frame, Request, Response};
use covenant_peer_auth::{PeerStatusFilter, PeerSummary};
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

async fn authenticate(stream: &mut UnixStream, home: &std::path::Path) -> Result<()> {
    let token_path = home.join("peers").join("operator.token");
    let token_b58 = std::fs::read_to_string(&token_path)
        .with_context(|| {
            format!(
                "read operator token at {} (start covenantd at least once to mint it)",
                token_path.display()
            )
        })?
        .trim()
        .to_string();
    write_frame(stream, &Request::Authenticate { token_b58 }).await?;
    match read_frame::<_, Response>(stream).await? {
        Response::Authenticated { .. } => Ok(()),
        Response::AuthenticationFailed { reason } => bail!("authenticate failed: {reason}"),
        other => bail!("unexpected response to authenticate: {other:?}"),
    }
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
    eprintln!("  covenant chain status                   show Solana protocol configuration");
    eprintln!(
        "  covenant chain flush-receipts [-n N]    batch local receipts into a Solana receipt root"
    );
    eprintln!("  covenant chain receipt-batches [-n N]   list local receipt batches");
    eprintln!("  covenant ignore check <text>            test text against .covenantignore rules");
    eprintln!("  covenant tools list                     list registered tools");
    eprintln!("  covenant tools call <name> [--args <json>]   invoke a registered tool");
    eprintln!(
        "  covenant audit recent [-n N]            list recent audit events as JSON lines (one per row, jq-friendly)"
    );
    eprintln!(
        "  covenant audit purge (--before-ms M | --older-than-ms D)  drop audit events older than ms epoch / D ms ago"
    );
    eprintln!(
        "  covenant capabilities purge (--before-ms M | --older-than-ms D)  drop revoked caps older than ms epoch / D ms ago"
    );
    eprintln!(
        "  covenant a2a status [-n N]            list queued tasks, in-flight leases, and pending results"
    );
    eprintln!(
        "  covenant a2a compact                  drop event-log lines for fully-resolved a2a tasks"
    );
    eprintln!(
        "  covenant peers purge (--before-ms M | --older-than-ms D)  drop revoked peer registrations older than ms epoch / D ms ago"
    );
    eprintln!(
        "  covenant peers rotate                   mint a fresh operator token and revoke the old one"
    );
    eprintln!(
        "  covenant peers list [--limit N] [--prefix B58] [--live-only | --revoked-only]  list registered peers (operator-only) — match audit `peer_pubkey_b58` via --prefix; --live-only and --revoked-only narrow by status"
    );
    eprintln!(
        "  covenant peers revoke <TOKEN-PREFIX> [--force] [--limit-matches N]  revoke a single peer by its token prefix (operator-only); --force overrides the self-revoke guard, --limit-matches caps ambiguous-match render"
    );
    eprintln!(
        "  covenant intents resume <intent-id>     re-dispatch a previously budget-rejected intent"
    );
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

fn parse_limit_args(args: &[String]) -> Result<usize> {
    let mut limit = 10;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" | "--limit" => {
                i += 1;
                let value = args.get(i).context("--limit needs a value")?;
                limit = value.parse().context("--limit must be an integer")?;
            }
            other => bail!("unknown flag '{other}'"),
        }
        i += 1;
    }
    Ok(limit)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print_usage();
        std::process::exit(2);
    }

    let home = covenant_home()?;
    let sock = home.join("sock");
    let mut stream = UnixStream::connect(&sock).await.with_context(|| {
        format!(
            "connect to daemon at {} (is covenantd running?)",
            sock.display()
        )
    })?;
    authenticate(&mut stream, &home).await?;

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
                    let action = match peer_prefix_to_lookup(&action) {
                        Some(prefix) => {
                            write_frame(
                                &mut stream,
                                &Request::ListPeers {
                                    limit: PEER_LOOKUP_LIMIT,
                                    pubkey_prefix: Some(prefix.to_string()),
                                    status_filter: None,
                                },
                            )
                            .await?;
                            let peers = match read_frame::<_, Response>(&mut stream).await? {
                                Response::PeerList { peers, .. } => peers,
                                Response::Error { message } => {
                                    bail!("daemon error during peer lookup: {message}")
                                }
                                other => bail!(
                                    "unexpected response to ListPeers during grant expansion: {other:?}"
                                ),
                            };
                            match expand_a2a_action(&action, &peers) {
                                Ok(ExpandOutcome::Unchanged) => action,
                                Ok(ExpandOutcome::Rewritten { full, .. }) => {
                                    eprintln!("expanding {prefix} → {full}");
                                    full
                                }
                                Err(err) => {
                                    print_expand_error(&err);
                                    std::process::exit(1);
                                }
                            }
                        }
                        None => action,
                    };
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
                "purge" => {
                    let mut before_ms: Option<u64> = None;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
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
                    write_frame(&mut stream, &Request::PurgeCapabilities { before_ms }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::CapabilitiesPurged { purged } => {
                            println!("purged {purged} revoked capability(ies)");
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
                        let onchain = match r.tx_sig.as_ref().or(r.onchain_sig.as_ref()) {
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
        "chain" => {
            if args.len() < 2 {
                print_usage();
                std::process::exit(2);
            }
            match args[1].as_str() {
                "status" => {
                    write_frame(&mut stream, &Request::ChainStatus).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::ChainStatus { status } => {
                            println!("chain: {}", status.chain);
                            println!("cluster: {}", status.cluster);
                            println!(
                                "rpc_url: {}",
                                status.rpc_url.as_deref().unwrap_or("(unset)")
                            );
                            println!("ws_url: {}", status.ws_url.as_deref().unwrap_or("(unset)"));
                            println!(
                                "program_id: {}",
                                status.program_id.as_deref().unwrap_or("(unset)")
                            );
                            println!(
                                "covnt_mint: {}",
                                status.covnt_mint.as_deref().unwrap_or("(unset)")
                            );
                            if status.ready {
                                println!("ready: true");
                            } else {
                                println!("ready: false ({})", status.missing.join(", "));
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "flush-receipts" => {
                    let limit = parse_limit_args(&args[2..])?;
                    write_frame(&mut stream, &Request::FlushReceipts { limit }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::ReceiptBatchFlushed {
                            batch,
                            receipts_updated,
                        } => {
                            println!("batch_id: {}", batch.batch_id);
                            println!("merkle_root: {}", batch.merkle_root);
                            println!("receipt_count: {}", batch.receipt_count);
                            println!("receipts_updated: {receipts_updated}");
                            println!("tx_sig: {}", batch.tx_sig.as_deref().unwrap_or("(pending)"));
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "receipt-batches" => {
                    let limit = parse_limit_args(&args[2..])?;
                    write_frame(&mut stream, &Request::ReceiptBatches { limit }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::ReceiptBatches { batches } => {
                            if batches.is_empty() {
                                println!("(no receipt batches)");
                            }
                            for batch in batches {
                                let tx_sig = batch.tx_sig.as_deref().unwrap_or("(pending)");
                                let slot = batch
                                    .slot
                                    .map(|slot| slot.to_string())
                                    .unwrap_or_else(|| "(pending)".to_string());
                                println!(
                                    "{} {} receipts root={} tx={} slot={}",
                                    batch.batch_id,
                                    batch.receipt_count,
                                    batch.merkle_root,
                                    tx_sig,
                                    slot
                                );
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "register-agent" | "stake" | "buy-credits" => {
                    bail!(
                        "chain {} is prepared by the Solana SDK; daemon signing is not wired yet",
                        args[1]
                    );
                }
                other => bail!("unknown chain subcommand '{other}'"),
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
                    drift,
                    orphans_total,
                } => {
                    println!("verify (last {window} records):");
                    for c in &checks {
                        let mark = if c.passed { "✓" } else { "✗" };
                        println!("  {mark} {} — {}", c.name, c.message);
                    }
                    if !drift.is_empty() {
                        println!("drift:");
                        for item in &drift {
                            let id = item.id.as_deref().unwrap_or("-");
                            println!("  - {} [{}] — {}", item.kind, id, item.message);
                            println!("    repair: {}", item.repair);
                        }
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
        "audit" => {
            if args.len() < 2 {
                eprintln!("covenant audit: expected `recent` or `purge`");
                std::process::exit(2);
            }
            match args[1].as_str() {
                "recent" => {
                    let mut limit: usize = 50;
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
                    write_frame(&mut stream, &Request::RecentAudit { limit }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::AuditEvents { events } => {
                            // One JSON line per event. The on-disk
                            // `audit/events.jsonl` uses the same shape, so
                            // a row read off the wire here matches what
                            // grep/jq would surface against the file.
                            // Universal renderer — every current and
                            // future `AuditKind` variant ships unchanged.
                            if events.is_empty() {
                                println!("(no audit events)");
                            }
                            for e in events {
                                println!("{}", serde_json::to_string(&e)?);
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "purge" => {
                    let mut before_ms: Option<u64> = None;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
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
                    write_frame(&mut stream, &Request::PurgeAudit { before_ms }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::AuditPurged { purged } => {
                            println!("purged {purged} event(s)");
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                other => {
                    eprintln!("covenant audit: unknown subcommand '{other}'");
                    print_usage();
                    std::process::exit(2);
                }
            }
        }
        "a2a" => {
            if args.len() < 2 {
                eprintln!("covenant a2a: expected `status` or `compact`");
                std::process::exit(2);
            }
            match args[1].as_str() {
                "status" => {
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
                    write_frame(&mut stream, &Request::A2AQueue { limit }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::A2AQueue { tasks, results } => {
                            if tasks.is_empty() && results.is_empty() {
                                println!("(a2a queue empty)");
                            }
                            for entry in tasks {
                                println!(
                                    "{}",
                                    serde_json::json!({ "type": "task", "entry": entry })
                                );
                            }
                            for result in results {
                                println!(
                                    "{}",
                                    serde_json::json!({ "type": "result", "result": result })
                                );
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "compact" => {
                    write_frame(&mut stream, &Request::CompactA2A).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::A2ACompacted { dropped } => {
                            println!("dropped {dropped} a2a event(s)");
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                other => {
                    eprintln!("covenant a2a: unknown subcommand '{other}'");
                    print_usage();
                    std::process::exit(2);
                }
            }
        }
        "peers" => {
            if args.len() < 2 {
                eprintln!("covenant peers: expected `purge`, `rotate`, `list`, or `revoke`");
                std::process::exit(2);
            }
            match args[1].as_str() {
                "purge" => {
                    let mut before_ms: Option<u64> = None;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
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
                    write_frame(&mut stream, &Request::PurgePeers { before_ms }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::PeersPurged { purged } => {
                            println!("purged {purged} revoked peer(s)");
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "rotate" => {
                    write_frame(&mut stream, &Request::RotateOperatorToken).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::OperatorTokenRotated { token_b58 } => {
                            // The daemon already wrote the new token to
                            // `$COVENANT_HOME/peers/operator.token` (mode
                            // 0600); print it here so the operator can
                            // copy it into a web UI's
                            // `.env.development.local`. Any existing
                            // shells holding the old token need to
                            // re-read the file.
                            println!("rotated. new token (also written to peers/operator.token):");
                            println!("{token_b58}");
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "list" => {
                    let mut limit: usize = 20;
                    let mut prefix: Option<String> = None;
                    let mut live_only = false;
                    let mut revoked_only = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--limit" => {
                                i += 1;
                                let v = args.get(i).context("--limit needs a value")?;
                                limit = v.parse().context("--limit must be an integer")?;
                            }
                            "--prefix" => {
                                i += 1;
                                let v = args.get(i).context("--prefix needs a value")?;
                                prefix = Some(v.clone());
                            }
                            "--live-only" => live_only = true,
                            "--revoked-only" => revoked_only = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let status_filter = peers_list_status_filter(live_only, revoked_only)?;
                    write_frame(
                        &mut stream,
                        &Request::ListPeers {
                            limit,
                            pubkey_prefix: prefix,
                            status_filter,
                        },
                    )
                    .await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::PeerList {
                            peers,
                            operator_pubkey_b58,
                            truncated,
                        } => {
                            for line in peer_list_lines(&peers, &operator_pubkey_b58, truncated) {
                                println!("{line}");
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "revoke" => {
                    let force = args.iter().any(|a| a == "--force");
                    let mut match_limit: Option<usize> = None;
                    let mut token_prefix: Option<String> = None;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--force" => {}
                            "--limit-matches" => {
                                i += 1;
                                let v = args.get(i).context("--limit-matches needs a value")?;
                                let n: usize = v
                                    .parse()
                                    .context("--limit-matches must be a positive integer")?;
                                if n == 0 {
                                    bail!("--limit-matches must be at least 1");
                                }
                                match_limit = Some(n);
                            }
                            other if !other.starts_with("--") && token_prefix.is_none() => {
                                token_prefix = Some(other.to_string());
                            }
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let token_prefix = token_prefix
                        .context("covenant peers revoke: missing TOKEN-PREFIX argument")?;
                    write_frame(
                        &mut stream,
                        &Request::RevokePeer {
                            token_prefix,
                            force,
                            match_limit,
                        },
                    )
                    .await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::PeerRevoked { outcome } => match outcome {
                            covenant_peer_auth::RevokeOutcome::Revoked(s) => {
                                println!(
                                    "revoked\t{display}\t{pubkey}\t{prefix}…\trevoked@{revoked}",
                                    display = s.agent_id.display,
                                    pubkey = s.agent_id.pubkey_base58(),
                                    prefix = s.token_prefix,
                                    revoked = s.revoked_at.unwrap_or(0),
                                );
                            }
                            covenant_peer_auth::RevokeOutcome::AlreadyRevoked(s) => {
                                println!(
                                    "already revoked at {revoked}: {display}\t{pubkey}\t{prefix}…",
                                    display = s.agent_id.display,
                                    pubkey = s.agent_id.pubkey_base58(),
                                    prefix = s.token_prefix,
                                    revoked = s.revoked_at.unwrap_or(0),
                                );
                            }
                            covenant_peer_auth::RevokeOutcome::NotFound => {
                                eprintln!("no peer matched the supplied prefix");
                                std::process::exit(1);
                            }
                            covenant_peer_auth::RevokeOutcome::Ambiguous { matches, truncated } => {
                                for line in peer_revoke_ambiguous_lines(&matches, truncated) {
                                    eprintln!("{line}");
                                }
                                std::process::exit(1);
                            }
                            covenant_peer_auth::RevokeOutcome::SelfRevokeForbidden(s) => {
                                eprintln!(
                                    "refused to revoke the operator's own bootstrap token: {display}\t{pubkey}\t{prefix}…",
                                    display = s.agent_id.display,
                                    pubkey = s.agent_id.pubkey_base58(),
                                    prefix = s.token_prefix,
                                );
                                eprintln!(
                                    "  use `covenant peers rotate` to retire the current token without bricking auth,"
                                );
                                eprintln!(
                                    "  or pass --force to override (this WILL brick auth; recover by deleting"
                                );
                                eprintln!(
                                    "  $COVENANT_HOME/peers/operator.token and restarting the daemon)."
                                );
                                std::process::exit(1);
                            }
                        },
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                other => bail!("covenant peers: unknown subcommand '{other}'"),
            }
        }
        "intents" => {
            if args.len() < 2 || args[1] != "resume" {
                eprintln!("covenant intents: expected `resume <intent-id>`");
                std::process::exit(2);
            }
            let intent_id_str = args
                .get(2)
                .context("covenant intents resume: missing <intent-id>")?;
            let intent_id: uuid::Uuid = intent_id_str
                .parse()
                .with_context(|| format!("intent-id must be a uuid, got {intent_id_str:?}"))?;
            write_frame(&mut stream, &Request::ResumeIntent { intent_id }).await?;
            match read_frame::<_, Response>(&mut stream).await? {
                Response::IntentResult { text, sources, .. } => {
                    println!("{text}");
                    if !sources.is_empty() {
                        println!();
                        println!("sources:");
                        for s in sources {
                            println!("  - {s}");
                        }
                    }
                }
                Response::Error { message } => bail!("daemon error: {message}"),
                other => bail!("unexpected response: {other:?}"),
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

const PEER_LOOKUP_LIMIT: usize = 16;
const PEER_SCOPED_PREFIXES: &[&str] = &["a2a.send.", "a2a.recv.", "a2a.respond."];

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExpandOutcome {
    Unchanged,
    Rewritten {
        full: String,
        peer_pubkey_b58: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExpandError {
    NoMatch {
        tail: String,
    },
    Ambiguous {
        tail: String,
        matches: Vec<PeerSummary>,
    },
    RevokedOnly {
        tail: String,
        matches: Vec<PeerSummary>,
    },
}

fn peer_prefix_to_lookup(action: &str) -> Option<&str> {
    for prefix in PEER_SCOPED_PREFIXES {
        if let Some(tail) = action.strip_prefix(prefix) {
            if tail.is_empty() || tail.contains('.') || tail.contains('@') {
                return None;
            }
            return Some(tail);
        }
    }
    None
}

fn expand_a2a_action(
    action: &str,
    peers: &[PeerSummary],
) -> std::result::Result<ExpandOutcome, ExpandError> {
    let (prefix, tail) = match PEER_SCOPED_PREFIXES
        .iter()
        .find_map(|p| action.strip_prefix(p).map(|t| (p.trim_end_matches('.'), t)))
    {
        Some(pair) => pair,
        None => return Ok(ExpandOutcome::Unchanged),
    };
    if tail.is_empty() || tail.contains('.') || tail.contains('@') {
        return Ok(ExpandOutcome::Unchanged);
    }

    let mut live: Vec<PeerSummary> = Vec::new();
    let mut revoked: Vec<PeerSummary> = Vec::new();
    for p in peers {
        if !p.agent_id.pubkey_base58().starts_with(tail) {
            continue;
        }
        if p.revoked_at.is_some() {
            revoked.push(p.clone());
        } else {
            live.push(p.clone());
        }
    }

    match live.len() {
        1 => {
            let peer = &live[0];
            let pubkey = peer.agent_id.pubkey_base58();
            let full = format!("{prefix}.{pubkey}");
            Ok(ExpandOutcome::Rewritten {
                full,
                peer_pubkey_b58: pubkey,
            })
        }
        0 if revoked.is_empty() => Err(ExpandError::NoMatch {
            tail: tail.to_string(),
        }),
        0 => Err(ExpandError::RevokedOnly {
            tail: tail.to_string(),
            matches: revoked,
        }),
        _ => Err(ExpandError::Ambiguous {
            tail: tail.to_string(),
            matches: live,
        }),
    }
}

/// Resolve the two `peers list` status flags into a single filter. The
/// pair is mutually exclusive — `--live-only && --revoked-only` would
/// silently empty the result, which is operationally a footgun. Reject
/// at parse time so the operator's mistake fails loudly with no daemon
/// round-trip.
fn peers_list_status_filter(
    live_only: bool,
    revoked_only: bool,
) -> Result<Option<PeerStatusFilter>> {
    match (live_only, revoked_only) {
        (true, true) => bail!("--live-only and --revoked-only are mutually exclusive"),
        (true, false) => Ok(Some(PeerStatusFilter::Live)),
        (false, true) => Ok(Some(PeerStatusFilter::Revoked)),
        (false, false) => Ok(None),
    }
}

fn peer_list_lines(
    peers: &[PeerSummary],
    operator_pubkey_b58: &str,
    truncated: bool,
) -> Vec<String> {
    if peers.is_empty() {
        return vec!["(no matching peers)".into()];
    }
    let mut out: Vec<String> = peers
        .iter()
        .map(|p| {
            let pubkey = p.agent_id.pubkey_base58();
            let self_marker = if pubkey == operator_pubkey_b58 {
                " (self)"
            } else {
                ""
            };
            let status = match p.revoked_at {
                Some(ts) => format!("revoked@{ts}"),
                None => "live".into(),
            };
            format!(
                "{display}{self_marker}\t{pubkey}\t{prefix}…\tregistered@{registered}\t{status}",
                display = p.agent_id.display,
                prefix = p.token_prefix,
                registered = p.registered_at,
            )
        })
        .collect();
    if truncated {
        out.push(format!(
            "(truncated; {n} shown — narrow with --prefix or raise --limit)",
            n = peers.len()
        ));
    }
    out
}

fn peer_revoke_ambiguous_lines(matches: &[PeerSummary], truncated: bool) -> Vec<String> {
    let mut out = Vec::with_capacity(matches.len() + 2);
    out.push(format!(
        "prefix matched {n} peers — narrow the prefix:",
        n = matches.len()
    ));
    for p in matches {
        let status = match p.revoked_at {
            Some(ts) => format!("revoked@{ts}"),
            None => "live".into(),
        };
        out.push(format!(
            "  {display}\t{pubkey}\t{prefix}…\tregistered@{registered}\t{status}",
            display = p.agent_id.display,
            pubkey = p.agent_id.pubkey_base58(),
            prefix = p.token_prefix,
            registered = p.registered_at,
        ));
    }
    if truncated {
        out.push(format!(
            "(truncated; {n} shown — re-run with a longer prefix or raise --limit-matches)",
            n = matches.len()
        ));
    }
    out
}

fn print_expand_error(err: &ExpandError) {
    match err {
        ExpandError::NoMatch { tail } => {
            eprintln!("no peer matched pubkey-prefix `{tail}`");
            eprintln!(
                "  use `covenant peers list --prefix <pubkey-b58-prefix>` to see registered peers"
            );
        }
        ExpandError::Ambiguous { tail, matches } => {
            eprintln!(
                "pubkey-prefix `{tail}` matched {n} live peers — narrow the prefix:",
                n = matches.len()
            );
            for p in matches {
                eprintln!(
                    "  {display}\t{pubkey}\tregistered@{registered}",
                    display = p.agent_id.display,
                    pubkey = p.agent_id.pubkey_base58(),
                    registered = p.registered_at,
                );
            }
        }
        ExpandError::RevokedOnly { tail, matches } => {
            eprintln!(
                "pubkey-prefix `{tail}` matched only revoked peers — granting against a revoked peer is meaningless:"
            );
            for p in matches {
                eprintln!(
                    "  {display}\t{pubkey}\trevoked@{revoked}",
                    display = p.agent_id.display,
                    pubkey = p.agent_id.pubkey_base58(),
                    revoked = p.revoked_at.unwrap_or(0),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_types::AgentId;

    fn make_peer(seed: u8, display: &str, revoked: bool) -> PeerSummary {
        let mut pk = [0u8; 32];
        pk[0] = seed;
        PeerSummary {
            agent_id: AgentId::new(display, pk),
            token_prefix: "tokenp".to_string(),
            registered_at: 1_700_000_000_000,
            revoked_at: if revoked {
                Some(1_700_000_001_000)
            } else {
                None
            },
        }
    }

    #[test]
    fn peer_prefix_to_lookup_returns_none_for_non_a2a_actions() {
        assert_eq!(peer_prefix_to_lookup("tool.call.foo"), None);
        assert_eq!(peer_prefix_to_lookup("audit.purge"), None);
        assert_eq!(peer_prefix_to_lookup("a2a.compact"), None);
    }

    #[test]
    fn peer_prefix_to_lookup_returns_none_for_display_form() {
        assert_eq!(peer_prefix_to_lookup("a2a.send.research@local"), None);
        assert_eq!(peer_prefix_to_lookup("a2a.recv.orch@local"), None);
        assert_eq!(peer_prefix_to_lookup("a2a.respond.user@host"), None);
    }

    #[test]
    fn peer_prefix_to_lookup_returns_some_for_pubkey_form() {
        assert_eq!(peer_prefix_to_lookup("a2a.send.abc"), Some("abc"));
        assert_eq!(peer_prefix_to_lookup("a2a.recv.xyzPQ"), Some("xyzPQ"));
        assert_eq!(peer_prefix_to_lookup("a2a.respond.1"), Some("1"));
    }

    #[test]
    fn peer_prefix_to_lookup_returns_none_for_empty_tail() {
        assert_eq!(peer_prefix_to_lookup("a2a.send."), None);
        assert_eq!(peer_prefix_to_lookup("a2a.respond."), None);
    }

    #[test]
    fn expand_unchanged_when_no_a2a_prefix() {
        let peers = vec![make_peer(7, "x@y", false)];
        assert_eq!(
            expand_a2a_action("tool.call.foo", &peers),
            Ok(ExpandOutcome::Unchanged)
        );
    }

    #[test]
    fn expand_unchanged_when_tail_contains_at_sign() {
        let peers = vec![make_peer(7, "x@y", false)];
        assert_eq!(
            expand_a2a_action("a2a.send.research@local", &peers),
            Ok(ExpandOutcome::Unchanged)
        );
    }

    #[test]
    fn expand_unchanged_for_a2a_compact() {
        assert_eq!(
            expand_a2a_action("a2a.compact", &[]),
            Ok(ExpandOutcome::Unchanged)
        );
    }

    #[test]
    fn expand_rewrites_unique_live_match_for_send() {
        let peer = make_peer(7, "alice@host", false);
        let pubkey = peer.agent_id.pubkey_base58();
        let prefix: String = pubkey.chars().take(3).collect();
        let action = format!("a2a.send.{prefix}");
        let outcome = expand_a2a_action(&action, std::slice::from_ref(&peer)).unwrap();
        assert_eq!(
            outcome,
            ExpandOutcome::Rewritten {
                full: format!("a2a.send.{pubkey}"),
                peer_pubkey_b58: pubkey,
            }
        );
    }

    #[test]
    fn expand_rewrites_unique_live_match_for_recv() {
        let peer = make_peer(11, "bob@host", false);
        let pubkey = peer.agent_id.pubkey_base58();
        let prefix: String = pubkey.chars().take(2).collect();
        let action = format!("a2a.recv.{prefix}");
        let ExpandOutcome::Rewritten { full, .. } =
            expand_a2a_action(&action, std::slice::from_ref(&peer)).unwrap()
        else {
            panic!("expected Rewritten");
        };
        assert_eq!(full, format!("a2a.recv.{pubkey}"));
    }

    #[test]
    fn expand_rewrites_unique_live_match_for_respond() {
        let peer = make_peer(13, "carol@host", false);
        let pubkey = peer.agent_id.pubkey_base58();
        let prefix: String = pubkey.chars().take(4).collect();
        let action = format!("a2a.respond.{prefix}");
        let ExpandOutcome::Rewritten { full, .. } =
            expand_a2a_action(&action, std::slice::from_ref(&peer)).unwrap()
        else {
            panic!("expected Rewritten");
        };
        assert_eq!(full, format!("a2a.respond.{pubkey}"));
    }

    #[test]
    fn expand_errors_no_match_when_zero_peers() {
        let err = expand_a2a_action("a2a.send.abc", &[]).unwrap_err();
        assert_eq!(
            err,
            ExpandError::NoMatch {
                tail: "abc".to_string()
            }
        );
    }

    #[test]
    fn expand_errors_ambiguous_when_multiple_live_matches() {
        // Two peers with leading-zero-byte pubkeys differ only in the trailing
        // byte; bs58 maps each leading zero byte to '1', so both encode to
        // strings starting with many '1's. Tail "1" matches both → Ambiguous.
        let mut pk1 = [0u8; 32];
        pk1[31] = 1;
        let mut pk2 = [0u8; 32];
        pk2[31] = 2;
        let p1 = PeerSummary {
            agent_id: AgentId::new("alice@host", pk1),
            token_prefix: "tokenp".into(),
            registered_at: 0,
            revoked_at: None,
        };
        let p2 = PeerSummary {
            agent_id: AgentId::new("bob@host", pk2),
            token_prefix: "tokenp".into(),
            registered_at: 0,
            revoked_at: None,
        };
        assert!(p1.agent_id.pubkey_base58().starts_with('1'));
        assert!(p2.agent_id.pubkey_base58().starts_with('1'));
        let err = expand_a2a_action("a2a.send.1", &[p1, p2]).unwrap_err();
        match err {
            ExpandError::Ambiguous { matches, tail } => {
                assert_eq!(tail, "1");
                assert_eq!(matches.len(), 2);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn expand_errors_revoked_only_when_unique_match_is_revoked() {
        let peer = make_peer(17, "dave@host", true);
        let pubkey = peer.agent_id.pubkey_base58();
        let prefix: String = pubkey.chars().take(3).collect();
        let action = format!("a2a.send.{prefix}");
        let err = expand_a2a_action(&action, std::slice::from_ref(&peer)).unwrap_err();
        match err {
            ExpandError::RevokedOnly { matches, .. } => {
                assert_eq!(matches.len(), 1);
                assert_eq!(matches[0].agent_id.pubkey_base58(), pubkey);
            }
            other => panic!("expected RevokedOnly, got {other:?}"),
        }
    }

    #[test]
    fn expand_treats_full_44_char_b58_as_lookup_and_succeeds_when_peer_present() {
        let peer = make_peer(23, "eve@host", false);
        let pubkey = peer.agent_id.pubkey_base58();
        let action = format!("a2a.send.{pubkey}");
        let outcome = expand_a2a_action(&action, std::slice::from_ref(&peer)).unwrap();
        assert_eq!(
            outcome,
            ExpandOutcome::Rewritten {
                full: format!("a2a.send.{pubkey}"),
                peer_pubkey_b58: pubkey,
            }
        );
    }

    #[test]
    fn expand_full_length_b58_with_no_match_errors() {
        let registered = make_peer(29, "frank@host", false);
        let phantom = make_peer(31, "ghost@host", false);
        let phantom_pubkey = phantom.agent_id.pubkey_base58();
        let action = format!("a2a.send.{phantom_pubkey}");
        let err = expand_a2a_action(&action, &[registered]).unwrap_err();
        assert_eq!(
            err,
            ExpandError::NoMatch {
                tail: phantom_pubkey
            }
        );
    }

    #[test]
    fn expand_does_not_carry_token_prefix_in_outcome() {
        let peer = make_peer(37, "hank@host", false);
        let pubkey = peer.agent_id.pubkey_base58();
        let prefix: String = pubkey.chars().take(3).collect();
        let action = format!("a2a.send.{prefix}");
        let outcome = expand_a2a_action(&action, std::slice::from_ref(&peer)).unwrap();
        let dump = format!("{outcome:?}");
        assert!(
            !dump.contains(&peer.token_prefix),
            "Rewritten outcome must not carry token_prefix: {dump}"
        );
    }

    #[test]
    fn peer_list_lines_renders_empty_marker_when_no_peers() {
        let out = peer_list_lines(&[], "OPB58", false);
        assert_eq!(out, vec!["(no matching peers)"]);
    }

    #[test]
    fn peer_list_lines_marks_self_row_and_omits_truncation_hint_when_not_truncated() {
        let p = make_peer(7, "alice@host", false);
        let operator = p.agent_id.pubkey_base58();
        let out = peer_list_lines(std::slice::from_ref(&p), &operator, false);
        assert_eq!(out.len(), 1, "exactly one row, no trailing hint");
        assert!(
            out[0].starts_with("alice@host (self)\t"),
            "self marker missing: {}",
            out[0]
        );
        assert!(out[0].contains("\tlive"));
        assert!(!out.iter().any(|l| l.contains("truncated")));
    }

    #[test]
    fn peer_list_lines_appends_truncation_hint_when_truncated() {
        let p = make_peer(7, "alice@host", false);
        let q = make_peer(8, "bob@host", false);
        let out = peer_list_lines(&[p, q], "different-pubkey", true);
        assert_eq!(out.len(), 3, "two rows + one hint line");
        let hint = out.last().unwrap();
        assert!(hint.starts_with("(truncated; 2 shown — "), "hint: {hint}");
        assert!(
            hint.contains("--prefix") && hint.contains("--limit"),
            "hint should suggest narrowing: {hint}"
        );
    }

    #[test]
    fn peer_revoke_ambiguous_lines_omits_hint_when_not_truncated() {
        let p = make_peer(7, "alice@host", false);
        let q = make_peer(8, "bob@host", true);
        let out = peer_revoke_ambiguous_lines(&[p, q], false);
        assert_eq!(out.len(), 3, "header + two rows, no hint");
        assert!(out[0].starts_with("prefix matched 2 peers"));
        assert!(out[1].contains("alice@host"));
        assert!(out[1].contains("\tlive"));
        assert!(out[2].contains("bob@host"));
        assert!(out[2].contains("\trevoked@"));
        assert!(!out.iter().any(|l| l.contains("truncated")));
    }

    #[test]
    fn peer_revoke_ambiguous_lines_appends_truncation_hint_when_truncated() {
        let p = make_peer(7, "alice@host", false);
        let q = make_peer(8, "bob@host", false);
        let out = peer_revoke_ambiguous_lines(&[p, q], true);
        let hint = out.last().unwrap();
        assert!(hint.starts_with("(truncated; 2 shown — "), "hint: {hint}");
        assert!(
            hint.contains("longer prefix") && hint.contains("--limit-matches"),
            "hint should suggest both narrowing options: {hint}"
        );
    }

    #[test]
    fn peers_list_status_filter_resolves_three_branches_and_rejects_both() {
        // No flag → no filter; the wire default that surfaces both halves.
        assert_eq!(peers_list_status_filter(false, false).unwrap(), None);
        assert_eq!(
            peers_list_status_filter(true, false).unwrap(),
            Some(PeerStatusFilter::Live)
        );
        assert_eq!(
            peers_list_status_filter(false, true).unwrap(),
            Some(PeerStatusFilter::Revoked)
        );
        // Both flags set is operationally a footgun (silently empty
        // result against the registry); rejected at parse time.
        let err = peers_list_status_filter(true, true).unwrap_err();
        assert!(
            err.to_string().contains("mutually exclusive"),
            "error mentions mutual exclusion: {err}"
        );
    }
}
