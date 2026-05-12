//! Covenant command-line client for the local daemon.
//!
//! ```text
//!   covenant ping [--json]
//!   covenant intent [--json] <text>
//!   covenant memory recent [--tier <working|episodic|longterm>] [--limit N] [--json]
//!   covenant memory search <query> [--tier <working|episodic|longterm>] [--limit N] [--min-relevance F] [--json]
//!   covenant memory purge [--tier <T>] (--before-ms <M> | --older-than-ms <D>) [--json]
//!   covenant memory compact --reason <text> [--apply] [--detach-stale-parents] [--delete-working-before-ms <M>|--delete-working-older-than-ms <D>] [--delete-episodic-before-ms <M>|--delete-episodic-older-than-ms <D>] [--mark-longterm-stale-before-ms <M>|--mark-longterm-stale-older-than-ms <D>] [--json]
//!   covenant memory plan-compaction --reason <text> [--detach-stale-parents] [--delete-working-before-ms <M>|--delete-working-older-than-ms <D>] [--delete-episodic-before-ms <M>|--delete-episodic-older-than-ms <D>] [--mark-longterm-stale-before-ms <M>|--mark-longterm-stale-older-than-ms <D>] [--json]
//!   covenant memory plan-receipt-backfill [--limit N] [--json]
//!   covenant memory repair detach-parent <id> --reason <text> [--expected-parent <uuid>] [--apply]
//!   covenant memory repair delete <id> --reason <text> [--apply]
//!   covenant memory repair backfill-provenance <id> --reason <text> --provenance <json> [--apply]
//!   covenant capabilities recent [--limit N] [--json]
//!   covenant capabilities grant <action> [--scope <json>] [--expires-at <ms>] [--json]
//!   covenant capabilities revoke <signature-b58> [--json]
//!   covenant capabilities purge (--before-ms <M> | --older-than-ms <D>) [--json]
//!   covenant receipts recent [--limit N] [--since-ms <epoch_ms>] [--json]
//!   covenant chain status [--json]
//!   covenant chain flush-receipts [--limit N] [--json]
//!   covenant chain receipt-batches [--limit N] [--json]
//!   covenant verify [--window N] [--json]
//!   covenant ignore check [--json] <text>
//!   covenant tools list [--json]
//!   covenant tools call <name> [--args <json>] [--json]
//!   covenant audit recent [--limit N] [--since-ms <epoch_ms>] [--json]
//!   covenant audit verify [--json]
//!   covenant audit purge (--before-ms <M> | --older-than-ms <D>) [--json]
//!   covenant a2a status [--limit N] [--min-lease-age-ms N] [--deadline-within-ms N] [--state queued|in_flight] [--json]
//!   covenant a2a requeue <task-id> --reason <text> --duplicate-risk <idempotent|operator-accepted> [--lease-id <uuid>]
//!   covenant a2a force-error <task-id> --reason <text> --message <text> [--lease-id <uuid>]
//!   covenant a2a retry-stale [--enable] [--min-lease-age-ms N] [--max-attempts N] [--max-requeues N] [--scan-limit N] [--json]
//!   covenant a2a compact [--json]
//!   covenant peers purge (--before-ms <M> | --older-than-ms <D>) [--json]
//!   covenant peers rotate [--json]
//!   covenant peers list [--limit N] [--prefix <pubkey-b58-prefix>] [--json]
//!   covenant peers revoke <token-prefix> [--force] [--limit-matches <N>] [--json]
//!   covenant intents resume <intent-id> [--json]
//!   covenant intents resume latest [--json]
//! ```

#![deny(unsafe_code)]

use anyhow::{bail, Context, Result};
use covenant_a2a::{
    A2AAutoRetryPolicy, A2AAutoRetryReport, A2ADuplicateRisk, A2ARepairCommand, A2ARepairRequest,
    A2ATaskQueueEntry, A2ATaskQueueState, A2ATaskResult,
};
use covenant_audit::{AuditEvent, AuditIntegrityReport, AuditKind};
use covenant_ipc::{
    read_frame, write_frame, ChainStatus, ReceiptBatchSummary, Request, Response, VerifyCheck,
    VerifyDrift,
};
use covenant_mcp::ToolSpec;
use covenant_peer_auth::{PeerStatusFilter, PeerSummary, RevokeOutcome};
use covenant_permissions::SignedCapability;
use covenant_types::{
    MemoryCompactionOutcome, MemoryCompactionPolicy, MemoryCompactionRequest, MemoryRecord,
    MemoryRepairCommand, MemoryRepairMode, MemoryRepairRequest, MemoryTier, ResourceKind,
    SettlementReceipt,
};
use std::collections::HashSet;
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
    eprintln!("  covenant intent [--json] <text>         submit an intent and print the result");
    eprintln!("  covenant ping [--json]                  check the daemon is responsive");
    eprintln!(
        "  covenant memory recent [--tier T] [-n N] [--json]      list recent memory records"
    );
    eprintln!(
        "  covenant memory search <query> [--tier T] [-n N] [--min-relevance F] [--json]  semantic search via embeddings; --min-relevance F drops records whose cosine score is below F (range [0.0, 1.0]) before the limit is applied"
    );
    eprintln!(
        "  covenant memory purge [--tier T] (--before-ms M | --older-than-ms D) [--json]  delete records older than ms epoch / D ms ago"
    );
    eprintln!(
        "  covenant memory compact --reason TEXT [--apply] [--detach-stale-parents] [--delete-working-before-ms M|--delete-working-older-than-ms D] [--delete-episodic-before-ms M|--delete-episodic-older-than-ms D] [--mark-longterm-stale-before-ms M|--mark-longterm-stale-older-than-ms D] [--json]"
    );
    eprintln!(
        "  covenant memory plan-compaction --reason TEXT [--detach-stale-parents] [--delete-working-before-ms M|--delete-working-older-than-ms D] [--delete-episodic-before-ms M|--delete-episodic-older-than-ms D] [--mark-longterm-stale-before-ms M|--mark-longterm-stale-older-than-ms D] [--json]"
    );
    eprintln!("  covenant memory plan-receipt-backfill [-n N] [--json]  dry-run legacy memory receipt correlation plan");
    eprintln!(
        "  covenant memory repair detach-parent <id> --reason TEXT [--expected-parent UUID] [--apply]"
    );
    eprintln!("  covenant memory repair delete <id> --reason TEXT [--apply]");
    eprintln!(
        "  covenant memory repair backfill-provenance <id> --reason TEXT --provenance JSON [--apply]"
    );
    eprintln!(
        "  covenant receipts recent [-n N] [--since-ms <epoch_ms>] [--json]  list recent settlement receipts"
    );
    eprintln!("  covenant chain status [--json]          show Solana protocol configuration");
    eprintln!(
        "  covenant chain flush-receipts [-n N] [--json]  batch local receipts into a Solana receipt root"
    );
    eprintln!("  covenant chain receipt-batches [-n N] [--json]  list local receipt batches");
    eprintln!("  covenant verify [-w N] [--json]      cross-check audit log vs other state");
    eprintln!("  covenant ignore check [--json] <text>   test text against .covenantignore rules");
    eprintln!("  covenant tools list [--json]            list registered tools");
    eprintln!("  covenant tools call <name> [--args <json>] [--json]   invoke a registered tool");
    eprintln!(
        "  covenant audit recent [-n N] [--since-ms <epoch_ms>] [--json]   list recent audit events as JSONL or one JSON envelope; --since-ms drops events older than the given epoch ms before --limit is applied"
    );
    eprintln!("  covenant audit verify [--json]         verify local audit hash-chain sidecar");
    eprintln!(
        "  covenant audit purge (--before-ms M | --older-than-ms D) [--json]  drop audit events older than ms epoch / D ms ago"
    );
    eprintln!(
        "  covenant capabilities recent [-n N] [--json]  list recent active capability tokens"
    );
    eprintln!("  covenant capabilities grant <action> [--scope JSON] [--expires-at M] [--json]");
    eprintln!("  covenant capabilities revoke <signature-b58> [--json]");
    eprintln!(
        "  covenant capabilities purge (--before-ms M | --older-than-ms D) [--json]  drop revoked caps older than ms epoch / D ms ago"
    );
    eprintln!(
        "  covenant a2a status [-n N] [--min-lease-age-ms N] [--deadline-within-ms N] [--state queued|in_flight] [--json]  list queued tasks, in-flight leases, and pending results; --deadline-within-ms N keeps only tasks whose deadline_ms is set and within at most N ms from now; --state narrows to one queue state"
    );
    eprintln!(
        "  covenant a2a requeue <task-id> --reason TEXT --duplicate-risk idempotent|operator-accepted [--lease-id UUID]"
    );
    eprintln!(
        "  covenant a2a force-error <task-id> --reason TEXT --message TEXT [--lease-id UUID]"
    );
    eprintln!(
        "  covenant a2a retry-stale [--enable] [--min-lease-age-ms N] [--max-attempts N] [--max-requeues N] [--scan-limit N] [--json]"
    );
    eprintln!(
        "  covenant a2a compact [--json]          drop event-log lines for fully-resolved a2a tasks"
    );
    eprintln!(
        "  covenant peers purge (--before-ms M | --older-than-ms D) [--json]  drop revoked peer registrations older than ms epoch / D ms ago"
    );
    eprintln!(
        "  covenant peers rotate [--json]          mint a fresh operator token and revoke the old one"
    );
    eprintln!(
        "  covenant peers list [--limit N] [--prefix B58] [--live-only | --revoked-only] [--json]  list registered peers (operator-only) — match audit `peer_pubkey_b58` via --prefix; add --json for stable machine output"
    );
    eprintln!(
        "  covenant peers revoke <TOKEN-PREFIX> [--force] [--limit-matches N] [--json]  revoke a single peer by its token prefix (operator-only); --json emits one stable machine-readable outcome"
    );
    eprintln!(
        "  covenant intents resume <intent-id> [--json]     re-dispatch a previously budget-rejected intent"
    );
    eprintln!(
        "  covenant intents resume latest [--json]          re-dispatch the most recent budget-rejected intent"
    );
}

struct MemoryReadJsonArgs {
    mode: &'static str,
    tier: Option<MemoryTier>,
    limit: usize,
    query: Option<String>,
    min_relevance: Option<f32>,
}

async fn resolve_intents_resume_intent_id(
    stream: &mut UnixStream,
    want_latest: bool,
    explicit_id: Option<&str>,
) -> Result<uuid::Uuid> {
    if want_latest {
        if explicit_id.is_some() {
            bail!("covenant intents resume: pass either <intent-id> or latest, not both");
        }
        let limit = 200;
        write_frame(
            stream,
            &Request::RecentAudit {
                limit,
                since_ms: None,
            },
        )
        .await?;
        let events = match read_frame::<_, Response>(stream).await? {
            Response::AuditEvents { events } => events,
            Response::Error { message } => bail!("daemon error: {message}"),
            other => bail!("unexpected response: {other:?}"),
        };
        let mut latest: Option<(u64, uuid::Uuid)> = None;
        for e in events {
            let id = match e.kind {
                AuditKind::BudgetExhausted { intent_id, .. } => intent_id,
                _ => continue,
            };
            match latest {
                Some((ts, _)) if ts >= e.timestamp_ms => {}
                _ => latest = Some((e.timestamp_ms, id)),
            }
        }
        return latest.map(|(_, id)| id).context(
            "no BudgetExhausted audit row found in recent audit feed (try `covenant audit recent`)",
        );
    }

    let intent_id_str =
        explicit_id.context("covenant intents resume: missing <intent-id> or latest")?;
    intent_id_str
        .parse()
        .with_context(|| format!("intent-id must be a uuid, got {intent_id_str:?}"))
}

async fn print_memory_response(
    stream: &mut UnixStream,
    json: Option<MemoryReadJsonArgs>,
) -> Result<()> {
    match read_frame::<_, Response>(stream).await? {
        Response::Memories { records } => {
            if let Some(args) = json {
                println!(
                    "{}",
                    serde_json::to_string(&memory_read_json(
                        args.mode,
                        args.tier,
                        args.limit,
                        args.query.as_deref(),
                        args.min_relevance,
                        &records
                    ))?
                );
                return Ok(());
            }
            if records.is_empty() {
                println!("(no records)");
            }
            for r in records {
                let tier = memory_tier_slug(r.tier);
                println!("[{}] {tier}: {}", r.created_at, r.text);
            }
            Ok(())
        }
        Response::Error { message } => bail!("daemon error: {message}"),
        other => bail!("unexpected response: {other:?}"),
    }
}

fn memory_tier_slug(tier: MemoryTier) -> &'static str {
    match tier {
        MemoryTier::Working => "working",
        MemoryTier::Episodic => "episodic",
        MemoryTier::LongTerm => "longterm",
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

fn parse_duplicate_risk(value: &str) -> Result<A2ADuplicateRisk> {
    match value {
        "idempotent" => Ok(A2ADuplicateRisk::Idempotent),
        "operator-accepted" | "operator_accepted" => Ok(A2ADuplicateRisk::OperatorAccepted),
        other => bail!("unknown duplicate risk '{other}' (expected idempotent|operator-accepted)"),
    }
}

fn parse_a2a_queue_state(value: &str) -> Result<A2ATaskQueueState> {
    match value {
        "queued" => Ok(A2ATaskQueueState::Queued),
        "in_flight" | "in-flight" => Ok(A2ATaskQueueState::InFlight),
        other => bail!("unknown a2a state '{other}' (expected queued|in_flight)"),
    }
}

fn parse_uuid(value: &str, name: &str) -> Result<uuid::Uuid> {
    value
        .parse()
        .with_context(|| format!("{name} must be a UUID"))
}

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn print_a2a_repair_response(response: Response) -> Result<()> {
    match response {
        Response::A2ARepaired { outcome } => {
            println!("{}", serde_json::to_string(&outcome)?);
            Ok(())
        }
        Response::Error { message } => bail!("daemon error: {message}"),
        other => bail!("unexpected response: {other:?}"),
    }
}

fn print_memory_repair_response(response: Response) -> Result<()> {
    match response {
        Response::MemoryRepaired { outcome } => {
            println!("{}", serde_json::to_string(&outcome)?);
            Ok(())
        }
        Response::Error { message } => bail!("daemon error: {message}"),
        other => bail!("unexpected response: {other:?}"),
    }
}

fn print_memory_compaction_response(response: Response, as_json: bool) -> Result<()> {
    match response {
        Response::MemoryCompacted { outcome } => {
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string(&memory_compaction_json(&outcome))?
                );
            } else {
                println!("{}", serde_json::to_string(&outcome)?);
            }
            Ok(())
        }
        Response::Error { message } => bail!("daemon error: {message}"),
        other => bail!("unexpected response: {other:?}"),
    }
}

fn print_memory_compaction_plan_response(response: Response, as_json: bool) -> Result<()> {
    match response {
        Response::MemoryCompacted { outcome } => {
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string(&memory_compaction_plan_json(&outcome))?
                );
            } else {
                println!(
                    "would change: {} (delete {}, mark stale {}, detach parent {})",
                    outcome.would_change,
                    outcome.deleted.len(),
                    outcome.stale_marked.len(),
                    outcome.parents_detached.len()
                );
                println!("receipt backfill: none in dry-run plan");
            }
        }
        Response::Error { message } => bail!("daemon error: {message}"),
        other => bail!("unexpected response: {other:?}"),
    }
    Ok(())
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
            let mut as_json = false;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--json" => as_json = true,
                    other => bail!("unknown flag '{other}'"),
                }
                i += 1;
            }
            write_frame(&mut stream, &Request::Ping).await?;
            match read_frame::<_, Response>(&mut stream).await? {
                Response::Pong => {
                    if as_json {
                        println!("{}", serde_json::to_string(&ping_json())?);
                    } else {
                        println!("pong");
                    }
                }
                Response::Error { message } => bail!("daemon error: {message}"),
                other => bail!("unexpected response: {other:?}"),
            }
        }
        "intent" => {
            if args.len() < 2 {
                eprintln!("covenant intent: missing intent text");
                std::process::exit(2);
            }
            // Strip `--json` only from leading positions. Stop scanning
            // for flags after the first non-flag token so `covenant
            // intent search --json command help` keeps the literal text
            // `search --json command help` instead of silently dropping
            // the `--json` in the middle.
            let mut as_json = false;
            let mut text_parts: Vec<String> = Vec::new();
            let mut consuming_flags = true;
            for arg in args.iter().skip(1) {
                if consuming_flags && arg == "--json" {
                    as_json = true;
                } else {
                    consuming_flags = false;
                    text_parts.push(arg.clone());
                }
            }
            if text_parts.is_empty() {
                eprintln!("covenant intent: missing intent text");
                std::process::exit(2);
            }
            let request_text = text_parts.join(" ");
            write_frame(&mut stream, &Request::SubmitIntent { text: request_text }).await?;
            match read_frame::<_, Response>(&mut stream).await? {
                Response::IntentResult {
                    intent_id,
                    status,
                    text,
                    sources,
                    settlement,
                } => {
                    if as_json {
                        println!(
                            "{}",
                            serde_json::to_string(&intent_result_json(
                                intent_id,
                                &status,
                                &text,
                                &sources,
                                settlement.as_ref(),
                            ))?
                        );
                    } else {
                        println!("{text}");
                    }
                }
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
                    let mut as_json = false;
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
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(&mut stream, &Request::RecentMemory { tier, limit }).await?;
                    print_memory_response(
                        &mut stream,
                        as_json.then_some(MemoryReadJsonArgs {
                            mode: "recent",
                            tier,
                            limit,
                            query: None,
                            min_relevance: None,
                        }),
                    )
                    .await?;
                }
                "purge" => {
                    let mut tier: Option<MemoryTier> = None;
                    let mut before_ms: Option<u64> = None;
                    let mut as_json = false;
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
                                before_ms = Some(epoch_ms().saturating_sub(dur));
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let before_ms = before_ms.context("missing --before-ms or --older-than-ms")?;
                    write_frame(&mut stream, &Request::PurgeMemory { tier, before_ms }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::MemoryPurged { purged } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&memory_purge_json(
                                        tier, before_ms, purged
                                    ))?
                                );
                            } else {
                                println!("purged {purged} record(s)");
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "compact" | "plan-compaction" => {
                    let plan_only = args[1] == "plan-compaction";
                    let mut policy = MemoryCompactionPolicy::default();
                    let mut reason = None;
                    let mut apply = false;
                    let mut as_json = plan_only;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--apply" if plan_only => {
                                bail!("memory plan-compaction is read-only and does not accept --apply")
                            }
                            "--apply" => apply = true,
                            "--json" => as_json = true,
                            "--reason" => {
                                i += 1;
                                reason =
                                    Some(args.get(i).context("--reason needs a value")?.clone());
                            }
                            "--detach-stale-parents" => policy.detach_stale_parents = true,
                            "--delete-working-before-ms" => {
                                i += 1;
                                let v = args
                                    .get(i)
                                    .context("--delete-working-before-ms needs a value")?;
                                policy.delete_working_before_ms = Some(
                                    v.parse()
                                        .context("--delete-working-before-ms must be an integer")?,
                                );
                            }
                            "--delete-working-older-than-ms" => {
                                i += 1;
                                let v = args
                                    .get(i)
                                    .context("--delete-working-older-than-ms needs a value")?;
                                let dur: u64 = v
                                    .parse()
                                    .context("--delete-working-older-than-ms must be an integer")?;
                                policy.delete_working_before_ms =
                                    Some(epoch_ms().saturating_sub(dur));
                            }
                            "--delete-episodic-before-ms" => {
                                i += 1;
                                let v = args
                                    .get(i)
                                    .context("--delete-episodic-before-ms needs a value")?;
                                policy.delete_episodic_before_ms =
                                    Some(v.parse().context(
                                        "--delete-episodic-before-ms must be an integer",
                                    )?);
                            }
                            "--delete-episodic-older-than-ms" => {
                                i += 1;
                                let v = args
                                    .get(i)
                                    .context("--delete-episodic-older-than-ms needs a value")?;
                                let dur: u64 = v.parse().context(
                                    "--delete-episodic-older-than-ms must be an integer",
                                )?;
                                policy.delete_episodic_before_ms =
                                    Some(epoch_ms().saturating_sub(dur));
                            }
                            "--mark-longterm-stale-before-ms" => {
                                i += 1;
                                let v = args
                                    .get(i)
                                    .context("--mark-longterm-stale-before-ms needs a value")?;
                                policy.mark_longterm_stale_before_ms = Some(v.parse().context(
                                    "--mark-longterm-stale-before-ms must be an integer",
                                )?);
                            }
                            "--mark-longterm-stale-older-than-ms" => {
                                i += 1;
                                let v = args
                                    .get(i)
                                    .context("--mark-longterm-stale-older-than-ms needs a value")?;
                                let dur: u64 = v.parse().context(
                                    "--mark-longterm-stale-older-than-ms must be an integer",
                                )?;
                                policy.mark_longterm_stale_before_ms =
                                    Some(epoch_ms().saturating_sub(dur));
                            }
                            "--marked-at-ms" => {
                                i += 1;
                                let v = args.get(i).context("--marked-at-ms needs a value")?;
                                policy.marked_at_ms =
                                    Some(v.parse().context("--marked-at-ms must be an integer")?);
                            }
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let request = MemoryCompactionRequest {
                        mode: if apply {
                            MemoryRepairMode::Apply
                        } else {
                            MemoryRepairMode::DryRun
                        },
                        policy,
                        reason: reason.context("missing --reason")?,
                    };
                    write_frame(&mut stream, &Request::CompactMemory { request }).await?;
                    let response = read_frame::<_, Response>(&mut stream).await?;
                    if plan_only {
                        print_memory_compaction_plan_response(response, as_json)?;
                    } else {
                        print_memory_compaction_response(response, as_json)?;
                    }
                }
                "plan-receipt-backfill" => {
                    let mut limit: usize = 100;
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--apply" => {
                                bail!(
                                    "memory plan-receipt-backfill is read-only and does not accept --apply"
                                )
                            }
                            "-n" | "--limit" => {
                                i += 1;
                                let v = args.get(i).context("--limit needs a value")?;
                                limit = v.parse().context("--limit must be an integer")?;
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }

                    write_frame(&mut stream, &Request::RecentMemory { tier: None, limit }).await?;
                    let memories = match read_frame::<_, Response>(&mut stream).await? {
                        Response::Memories { records } => records,
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    };

                    write_frame(
                        &mut stream,
                        &Request::RecentReceipts {
                            limit,
                            since_ms: None,
                        },
                    )
                    .await?;
                    let receipts = match read_frame::<_, Response>(&mut stream).await? {
                        Response::Receipts { receipts } => receipts,
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    };

                    let plan = memory_receipt_backfill_plan_json(limit, &memories, &receipts);
                    if as_json {
                        println!("{}", serde_json::to_string(&plan)?);
                    } else {
                        let records = plan["records"].as_array().map(Vec::len).unwrap_or(0);
                        let unmatched_receipts = plan["unmatched_legacy_receipts"]
                            .as_array()
                            .map(Vec::len)
                            .unwrap_or(0);
                        let unmatched_memory = plan["unmatched_memory_records"]
                            .as_array()
                            .map(Vec::len)
                            .unwrap_or(0);
                        println!(
                            "receipt backfill plan: {records} candidate(s), {unmatched_receipts} unmatched legacy receipt(s), {unmatched_memory} unmatched memory record(s)"
                        );
                        println!("mutation: unsupported; this command only emits a dry-run plan");
                    }
                }
                "repair" => {
                    if args.len() < 4 {
                        bail!(
                            "covenant memory repair: expected detach-parent|delete|backfill-provenance <id>"
                        );
                    }
                    let id = parse_uuid(&args[3], "memory-id")?;
                    let mut reason = None;
                    let mut apply = false;
                    let mut expected_parent = None;
                    let mut provenance = None;
                    let mut i = 4;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--apply" => apply = true,
                            "--reason" => {
                                i += 1;
                                reason =
                                    Some(args.get(i).context("--reason needs a value")?.clone());
                            }
                            "--expected-parent" => {
                                i += 1;
                                let v = args.get(i).context("--expected-parent needs a value")?;
                                expected_parent = Some(parse_uuid(v, "--expected-parent")?);
                            }
                            "--provenance" => {
                                i += 1;
                                let v = args.get(i).context("--provenance needs a value")?;
                                provenance = Some(
                                    serde_json::from_str(v)
                                        .context("--provenance must be valid JSON")?,
                                );
                            }
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let command = match args[2].as_str() {
                        "detach-parent" => {
                            if provenance.is_some() {
                                bail!("detach-parent does not accept --provenance");
                            }
                            MemoryRepairCommand::DetachParent {
                                id,
                                expected_parent,
                            }
                        }
                        "delete" => {
                            if expected_parent.is_some() || provenance.is_some() {
                                bail!("delete accepts only --reason and --apply");
                            }
                            MemoryRepairCommand::DeleteRecord { id }
                        }
                        "backfill-provenance" => {
                            if expected_parent.is_some() {
                                bail!("backfill-provenance does not accept --expected-parent");
                            }
                            MemoryRepairCommand::BackfillProvenance {
                                id,
                                provenance: provenance.context("missing --provenance JSON")?,
                            }
                        }
                        other => bail!(
                            "unknown memory repair action '{other}' (expected detach-parent|delete|backfill-provenance)"
                        ),
                    };
                    let request = MemoryRepairRequest {
                        mode: if apply {
                            MemoryRepairMode::Apply
                        } else {
                            MemoryRepairMode::DryRun
                        },
                        command,
                        reason: reason.context("missing --reason")?,
                    };
                    write_frame(&mut stream, &Request::RepairMemory { request }).await?;
                    let response = read_frame::<_, Response>(&mut stream).await?;
                    print_memory_repair_response(response)?;
                }
                "search" => {
                    if args.len() < 3 {
                        eprintln!("covenant memory search: missing <query>");
                        std::process::exit(2);
                    }
                    let mut tier: Option<MemoryTier> = None;
                    let mut limit: usize = 10;
                    let mut min_relevance: Option<f32> = None;
                    let mut as_json = false;
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
                            "--min-relevance" => {
                                i += 1;
                                let v = args.get(i).context("--min-relevance needs a value")?;
                                let parsed: f32 = v
                                    .parse()
                                    .context("--min-relevance must be a float in [0.0, 1.0]")?;
                                if !parsed.is_finite() || !(0.0..=1.0).contains(&parsed) {
                                    bail!(
                                        "--min-relevance must be a finite float in [0.0, 1.0]; got {v:?}"
                                    );
                                }
                                min_relevance = Some(parsed);
                            }
                            "--json" => as_json = true,
                            other => query_parts.push(other.to_string()),
                        }
                        i += 1;
                    }
                    let query = query_parts.join(" ");
                    if query.is_empty() {
                        bail!("query text is required");
                    }
                    write_frame(
                        &mut stream,
                        &Request::SearchMemory {
                            query: query.clone(),
                            tier,
                            limit,
                            min_relevance,
                        },
                    )
                    .await?;
                    print_memory_response(
                        &mut stream,
                        as_json.then_some(MemoryReadJsonArgs {
                            mode: "search",
                            tier,
                            limit,
                            query: Some(query),
                            min_relevance,
                        }),
                    )
                    .await?;
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
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "-n" | "--limit" => {
                                i += 1;
                                let v = args.get(i).context("--limit needs a value")?;
                                limit = v.parse().context("--limit must be an integer")?;
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(&mut stream, &Request::RecentCapabilities { limit }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::Capabilities { capabilities } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&capability_list_json(
                                        limit,
                                        &capabilities
                                    ))?
                                );
                            } else if capabilities.is_empty() {
                                println!("(no capabilities granted)");
                            } else {
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
                    let mut scope: Option<serde_json::Value> = None;
                    let mut expires_at: Option<u64> = None;
                    let mut as_json = false;
                    let mut i = 3;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--scope" => {
                                i += 1;
                                let v = args.get(i).context("--scope needs a JSON value")?;
                                scope =
                                    Some(serde_json::from_str(v).context("--scope must be JSON")?);
                            }
                            "--expires-at" => {
                                i += 1;
                                let v = args.get(i).context("--expires-at needs a value")?;
                                expires_at = Some(
                                    v.parse()
                                        .context("--expires-at must be an integer (epoch ms)")?,
                                );
                            }
                            "--json" => as_json = true,
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
                            action: action.clone(),
                            scope: scope.clone(),
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
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&capability_grant_json(
                                        &subject_display,
                                        &action,
                                        &signature_b58,
                                        scope.as_ref(),
                                        expires_at,
                                    ))?
                                );
                            } else {
                                println!("granted: {subject_display} → {action}");
                                println!("signature: {signature_b58}");
                            }
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
                    let mut as_json = false;
                    let mut i = 3;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(
                        &mut stream,
                        &Request::RevokeCapability {
                            signature_b58: signature_b58.clone(),
                        },
                    )
                    .await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::CapabilityRevoked {
                            signature_b58,
                            removed,
                        } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&capability_revoke_json(
                                        &signature_b58,
                                        removed,
                                    ))?
                                );
                            } else if removed {
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
                    let mut as_json = false;
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
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let before_ms = before_ms.context("missing --before-ms or --older-than-ms")?;
                    write_frame(&mut stream, &Request::PurgeCapabilities { before_ms }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::CapabilitiesPurged { purged } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&capabilities_purge_json(
                                        before_ms, purged
                                    ))?
                                );
                            } else {
                                println!("purged {purged} revoked capability(ies)");
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
            let mut as_json = false;
            let mut since_ms: Option<u64> = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "-n" | "--limit" => {
                        i += 1;
                        let v = args.get(i).context("--limit needs a value")?;
                        limit = v.parse().context("--limit must be an integer")?;
                    }
                    "--since-ms" => {
                        i += 1;
                        let v = args.get(i).context("--since-ms needs a value")?;
                        since_ms = Some(v.parse().context("--since-ms must be an integer")?);
                    }
                    "--json" => as_json = true,
                    other => bail!("unknown flag '{other}'"),
                }
                i += 1;
            }
            write_frame(&mut stream, &Request::RecentReceipts { limit, since_ms }).await?;
            match read_frame::<_, Response>(&mut stream).await? {
                Response::Receipts { receipts } => {
                    if as_json {
                        println!(
                            "{}",
                            serde_json::to_string(&receipt_list_json(limit, since_ms, &receipts))?
                        );
                    } else if receipts.is_empty() {
                        println!("(no receipts)");
                    } else {
                        for r in receipts {
                            let resource = resource_name(r.resource);
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
                    let mut as_json = false;
                    for arg in &args[2..] {
                        match arg.as_str() {
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                    }
                    write_frame(&mut stream, &Request::ChainStatus).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::ChainStatus { status } => {
                            if as_json {
                                println!("{}", serde_json::to_string(&chain_status_json(&status))?);
                            } else {
                                println!("chain: {}", status.chain);
                                println!("cluster: {}", status.cluster);
                                println!(
                                    "rpc_url: {}",
                                    status.rpc_url.as_deref().unwrap_or("(unset)")
                                );
                                println!(
                                    "ws_url: {}",
                                    status.ws_url.as_deref().unwrap_or("(unset)")
                                );
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
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "flush-receipts" => {
                    let mut limit = 10;
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "-n" | "--limit" => {
                                i += 1;
                                let v = args.get(i).context("--limit needs a value")?;
                                limit = v.parse().context("--limit must be an integer")?;
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(&mut stream, &Request::FlushReceipts { limit }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::ReceiptBatchFlushed {
                            batch,
                            receipts_updated,
                        } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&flush_receipts_json(
                                        limit,
                                        &batch,
                                        receipts_updated
                                    ))?
                                );
                            } else {
                                println!("batch_id: {}", batch.batch_id);
                                println!("merkle_root: {}", batch.merkle_root);
                                println!("receipt_count: {}", batch.receipt_count);
                                println!("receipts_updated: {receipts_updated}");
                                println!(
                                    "tx_sig: {}",
                                    batch.tx_sig.as_deref().unwrap_or("(pending)")
                                );
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "receipt-batches" => {
                    let mut limit = 10;
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "-n" | "--limit" => {
                                i += 1;
                                let v = args.get(i).context("--limit needs a value")?;
                                limit = v.parse().context("--limit must be an integer")?;
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(&mut stream, &Request::ReceiptBatches { limit }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::ReceiptBatches { batches } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&receipt_batch_list_json(
                                        limit, &batches
                                    ))?
                                );
                            } else if batches.is_empty() {
                                println!("(no receipt batches)");
                            } else {
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
            let mut as_json = false;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--window" | "-w" => {
                        i += 1;
                        let v = args.get(i).context("--window needs a value")?;
                        window = v.parse().context("--window must be an integer")?;
                    }
                    "--json" => as_json = true,
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
                    if as_json {
                        println!(
                            "{}",
                            serde_json::to_string(&verify_report_json(
                                window,
                                &checks,
                                &drift,
                                orphans_total
                            ))?
                        );
                    } else {
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
                    }
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
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(&mut stream, &Request::ListTools).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::ToolList { tools } => {
                            if as_json {
                                println!("{}", serde_json::to_string(&tool_list_json(&tools))?);
                            } else if tools.is_empty() {
                                println!("(no tools registered)");
                            } else {
                                for t in tools {
                                    println!("{} — {}", t.name, t.description);
                                }
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
                    let mut as_json = false;
                    let mut i = 3;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--args" => {
                                i += 1;
                                let v = args.get(i).context("--args needs a value")?;
                                arguments =
                                    serde_json::from_str(v).context("--args must be valid JSON")?;
                            }
                            "--json" => as_json = true,
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
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&tool_result_json(
                                        &name, &content, is_error,
                                    ))?
                                );
                            } else {
                                for c in &content {
                                    match c {
                                        covenant_mcp::Content::Text { text } => println!("{text}"),
                                        covenant_mcp::Content::Json { value } => {
                                            println!("{}", serde_json::to_string_pretty(value)?);
                                        }
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
                eprintln!("covenant audit: expected `recent`, `verify`, or `purge`");
                std::process::exit(2);
            }
            match args[1].as_str() {
                "recent" => {
                    let mut limit: usize = 50;
                    let mut since_ms: Option<u64> = None;
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "-n" | "--limit" => {
                                i += 1;
                                let v = args.get(i).context("--limit needs a value")?;
                                limit = v.parse().context("--limit must be an integer")?;
                            }
                            "--since-ms" => {
                                i += 1;
                                let v = args.get(i).context("--since-ms needs a value")?;
                                since_ms =
                                    Some(v.parse().context("--since-ms must be an integer")?);
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(
                        &mut stream,
                        &Request::RecentAudit { limit, since_ms },
                    )
                    .await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::AuditEvents { events } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&audit_recent_json(
                                        limit, since_ms, &events
                                    ))?
                                );
                            } else {
                                // Default JSONL mirrors `audit/events.jsonl`, so tail/grep/jq
                                // users see the same row shape as the durable log.
                                if events.is_empty() {
                                    println!("(no audit events)");
                                }
                                for e in events {
                                    println!("{}", serde_json::to_string(&e)?);
                                }
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "verify" => {
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(&mut stream, &Request::VerifyAuditIntegrity).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::AuditIntegrity { report } => {
                            if as_json {
                                println!("{}", serde_json::to_string(&audit_verify_json(&report))?);
                            } else {
                                println!("{}", serde_json::to_string(&report)?);
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "purge" => {
                    let mut before_ms: Option<u64> = None;
                    let mut as_json = false;
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
                                before_ms = Some(epoch_ms().saturating_sub(dur));
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let before_ms = before_ms.context("missing --before-ms or --older-than-ms")?;
                    write_frame(&mut stream, &Request::PurgeAudit { before_ms }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::AuditPurged { purged } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&audit_purge_json(before_ms, purged))?
                                );
                            } else {
                                println!("purged {purged} event(s)");
                            }
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
                eprintln!(
                    "covenant a2a: expected `status`, `requeue`, `force-error`, `retry-stale`, or `compact`"
                );
                std::process::exit(2);
            }
            match args[1].as_str() {
                "status" => {
                    let mut limit: usize = 10;
                    let mut min_lease_age_ms: Option<u64> = None;
                    let mut deadline_within_ms: Option<u64> = None;
                    let mut state_filter: Option<A2ATaskQueueState> = None;
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "-n" | "--limit" => {
                                i += 1;
                                let v = args.get(i).context("--limit needs a value")?;
                                limit = v.parse().context("--limit must be an integer")?;
                            }
                            "--min-lease-age-ms" => {
                                i += 1;
                                let v = args.get(i).context("--min-lease-age-ms needs a value")?;
                                min_lease_age_ms = Some(
                                    v.parse().context("--min-lease-age-ms must be an integer")?,
                                );
                            }
                            "--deadline-within-ms" => {
                                i += 1;
                                let v =
                                    args.get(i).context("--deadline-within-ms needs a value")?;
                                deadline_within_ms = Some(
                                    v.parse()
                                        .context("--deadline-within-ms must be an integer")?,
                                );
                            }
                            "--state" => {
                                i += 1;
                                let v = args.get(i).context("--state needs a value")?;
                                state_filter = Some(parse_a2a_queue_state(v)?);
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(
                        &mut stream,
                        &Request::A2AQueue {
                            limit,
                            min_lease_age_ms,
                            deadline_within_ms,
                            state_filter,
                        },
                    )
                    .await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::A2AQueue { tasks, results } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&a2a_status_json(
                                        limit,
                                        min_lease_age_ms,
                                        deadline_within_ms,
                                        state_filter,
                                        &tasks,
                                        &results
                                    ))?
                                );
                            } else {
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
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "requeue" => {
                    if args.len() < 3 {
                        bail!(
                            "covenant a2a requeue: missing <task-id> --reason TEXT --duplicate-risk idempotent|operator-accepted"
                        );
                    }
                    let task_id = parse_uuid(&args[2], "task-id")?;
                    let mut lease_id = None;
                    let mut reason = None;
                    let mut duplicate_risk = None;
                    let mut i = 3;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--lease-id" => {
                                i += 1;
                                let v = args.get(i).context("--lease-id needs a value")?;
                                lease_id = Some(parse_uuid(v, "--lease-id")?);
                            }
                            "--reason" => {
                                i += 1;
                                reason =
                                    Some(args.get(i).context("--reason needs a value")?.clone());
                            }
                            "--duplicate-risk" => {
                                i += 1;
                                let v = args.get(i).context("--duplicate-risk needs a value")?;
                                duplicate_risk = Some(parse_duplicate_risk(v)?);
                            }
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let request = A2ARepairRequest {
                        task_id,
                        command: A2ARepairCommand::Requeue {
                            lease_id,
                            duplicate_risk: duplicate_risk
                                .context("missing --duplicate-risk idempotent|operator-accepted")?,
                        },
                        reason: reason.context("missing --reason")?,
                    };
                    write_frame(&mut stream, &Request::RepairA2ATask { request }).await?;
                    let response = read_frame::<_, Response>(&mut stream).await?;
                    print_a2a_repair_response(response)?;
                }
                "force-error" => {
                    if args.len() < 3 {
                        bail!(
                            "covenant a2a force-error: missing <task-id> --reason TEXT --message TEXT"
                        );
                    }
                    let task_id = parse_uuid(&args[2], "task-id")?;
                    let mut lease_id = None;
                    let mut reason = None;
                    let mut message = None;
                    let mut i = 3;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--lease-id" => {
                                i += 1;
                                let v = args.get(i).context("--lease-id needs a value")?;
                                lease_id = Some(parse_uuid(v, "--lease-id")?);
                            }
                            "--reason" => {
                                i += 1;
                                reason =
                                    Some(args.get(i).context("--reason needs a value")?.clone());
                            }
                            "--message" => {
                                i += 1;
                                message =
                                    Some(args.get(i).context("--message needs a value")?.clone());
                            }
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let request = A2ARepairRequest {
                        task_id,
                        command: A2ARepairCommand::ForceError {
                            lease_id,
                            message: message.context("missing --message")?,
                        },
                        reason: reason.context("missing --reason")?,
                    };
                    write_frame(&mut stream, &Request::RepairA2ATask { request }).await?;
                    let response = read_frame::<_, Response>(&mut stream).await?;
                    print_a2a_repair_response(response)?;
                }
                "retry-stale" => {
                    let mut policy = A2AAutoRetryPolicy::default();
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--enable" => policy.enabled = true,
                            "--min-lease-age-ms" => {
                                i += 1;
                                let v = args.get(i).context("--min-lease-age-ms needs a value")?;
                                policy.min_lease_age_ms =
                                    v.parse().context("--min-lease-age-ms must be an integer")?;
                            }
                            "--max-attempts" => {
                                i += 1;
                                let v = args.get(i).context("--max-attempts needs a value")?;
                                policy.max_attempts =
                                    v.parse().context("--max-attempts must be an integer")?;
                            }
                            "--max-requeues" => {
                                i += 1;
                                let v = args.get(i).context("--max-requeues needs a value")?;
                                policy.max_requeues =
                                    v.parse().context("--max-requeues must be an integer")?;
                            }
                            "--scan-limit" => {
                                i += 1;
                                let v = args.get(i).context("--scan-limit needs a value")?;
                                policy.scan_limit =
                                    v.parse().context("--scan-limit must be an integer")?;
                            }
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(&mut stream, &Request::RetryA2AStale { policy }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::A2AAutoRetried { report } => {
                            if as_json {
                                println!("{}", serde_json::to_string(&a2a_retry_json(&report))?);
                            } else {
                                println!(
                                    "considered {} task(s), requeued {}, skipped {}",
                                    report.considered,
                                    report.requeued.len(),
                                    report.skipped.len()
                                );
                                if !report.policy.enabled {
                                    println!("automatic retry disabled; pass --enable to mutate");
                                }
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "compact" => {
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    write_frame(&mut stream, &Request::CompactA2A).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::A2ACompacted { dropped } => {
                            if as_json {
                                println!("{}", serde_json::to_string(&a2a_compact_json(dropped))?);
                            } else {
                                println!("dropped {dropped} a2a event(s)");
                            }
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
                    let mut as_json = false;
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
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let before_ms = before_ms.context("missing --before-ms or --older-than-ms")?;
                    write_frame(&mut stream, &Request::PurgePeers { before_ms }).await?;
                    match read_frame::<_, Response>(&mut stream).await? {
                        Response::PeersPurged { purged } => {
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&peers_purge_json(before_ms, purged))?
                                );
                            } else {
                                println!("purged {purged} revoked peer(s)");
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                "rotate" => {
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
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
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&peers_rotate_json(&token_b58))?
                                );
                            } else {
                                println!(
                                    "rotated. new token (also written to peers/operator.token):"
                                );
                                println!("{token_b58}");
                            }
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
                    let mut as_json = false;
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
                            "--json" => as_json = true,
                            other => bail!("unknown flag '{other}'"),
                        }
                        i += 1;
                    }
                    let status_filter = peers_list_status_filter(live_only, revoked_only)?;
                    write_frame(
                        &mut stream,
                        &Request::ListPeers {
                            limit,
                            pubkey_prefix: prefix.clone(),
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
                            if as_json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&peer_list_json(
                                        limit,
                                        prefix.as_deref(),
                                        &peers,
                                        &operator_pubkey_b58,
                                        truncated,
                                    ))?
                                );
                            } else {
                                for line in peer_list_lines(&peers, &operator_pubkey_b58, truncated)
                                {
                                    println!("{line}");
                                }
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
                    let mut as_json = false;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--force" => {}
                            "--json" => as_json = true,
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
                        Response::PeerRevoked { outcome } => {
                            if as_json {
                                println!("{}", serde_json::to_string(&peer_revoke_json(&outcome))?);
                                if peer_revoke_is_failure(&outcome) {
                                    std::process::exit(1);
                                }
                                return Ok(());
                            }
                            match outcome {
                                RevokeOutcome::Revoked(s) => {
                                    println!(
                                        "revoked\t{display}\t{pubkey}\t{prefix}…\trevoked@{revoked}",
                                        display = s.agent_id.display,
                                        pubkey = s.agent_id.pubkey_base58(),
                                        prefix = s.token_prefix,
                                        revoked = s.revoked_at.unwrap_or(0),
                                    );
                                }
                                RevokeOutcome::AlreadyRevoked(s) => {
                                    println!(
                                        "already revoked at {revoked}: {display}\t{pubkey}\t{prefix}…",
                                        display = s.agent_id.display,
                                        pubkey = s.agent_id.pubkey_base58(),
                                        prefix = s.token_prefix,
                                        revoked = s.revoked_at.unwrap_or(0),
                                    );
                                }
                                RevokeOutcome::NotFound => {
                                    eprintln!("no peer matched the supplied prefix");
                                    std::process::exit(1);
                                }
                                RevokeOutcome::Ambiguous { matches, truncated } => {
                                    for line in peer_revoke_ambiguous_lines(&matches, truncated) {
                                        eprintln!("{line}");
                                    }
                                    std::process::exit(1);
                                }
                                RevokeOutcome::SelfRevokeForbidden(s) => {
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
                            }
                        }
                        Response::Error { message } => bail!("daemon error: {message}"),
                        other => bail!("unexpected response: {other:?}"),
                    }
                }
                other => bail!("covenant peers: unknown subcommand '{other}'"),
            }
        }
        "intents" => {
            if args.len() < 2 || args[1] != "resume" {
                eprintln!("covenant intents: expected `resume [--json] <intent-id>|latest`");
                std::process::exit(2);
            }
            let mut explicit_id: Option<String> = None;
            let mut want_latest = false;
            let mut as_json = false;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--latest" | "latest" => want_latest = true,
                    "--json" => as_json = true,
                    other if !other.starts_with("--") && explicit_id.is_none() => {
                        explicit_id = Some(other.to_string());
                    }
                    other => bail!("unknown flag '{other}'"),
                }
                i += 1;
            }

            let mode = if want_latest { "latest" } else { "explicit" };

            let intent_id = match resolve_intents_resume_intent_id(
                &mut stream,
                want_latest,
                explicit_id.as_deref(),
            )
            .await
            {
                Ok(intent_id) => intent_id,
                Err(err) => {
                    if as_json {
                        let message = err.to_string();
                        let code = intents_resume_error_code(&message);
                        println!(
                            "{}",
                            serde_json::to_string(&intents_resume_error_json(
                                mode, None, code, &message
                            ))?
                        );
                        std::process::exit(1);
                    }
                    return Err(err);
                }
            };

            write_frame(&mut stream, &Request::ResumeIntent { intent_id }).await?;
            match read_frame::<_, Response>(&mut stream).await? {
                Response::IntentResult {
                    intent_id,
                    status,
                    text,
                    sources,
                    settlement,
                } => {
                    if as_json {
                        println!(
                            "{}",
                            serde_json::to_string(&intents_resume_ok_json(
                                mode,
                                intent_id,
                                &status,
                                &text,
                                &sources,
                                &settlement
                            ))?
                        );
                        return Ok(());
                    }
                    println!("{text}");
                    if !sources.is_empty() {
                        println!();
                        println!("sources:");
                        for s in sources {
                            println!("  - {s}");
                        }
                    }
                }
                Response::Error { message } => {
                    if as_json {
                        println!(
                            "{}",
                            serde_json::to_string(&intents_resume_error_json(
                                mode,
                                Some(intent_id),
                                "daemon_error",
                                &message
                            ))?
                        );
                        std::process::exit(1);
                    }
                    bail!("daemon error: {message}")
                }
                other => {
                    if as_json {
                        println!(
                            "{}",
                            serde_json::to_string(&intents_resume_error_json(
                                mode,
                                Some(intent_id),
                                "unexpected_response",
                                &format!("{other:?}")
                            ))?
                        );
                        std::process::exit(1);
                    }
                    bail!("unexpected response: {other:?}")
                }
            }
        }
        "ignore" => {
            if args.len() < 2 || args[1] != "check" {
                eprintln!("covenant ignore: expected `check <text>`");
                std::process::exit(2);
            }
            let mut as_json = false;
            let mut text_parts = Vec::new();
            for arg in args.iter().skip(2) {
                if arg == "--json" {
                    as_json = true;
                } else {
                    text_parts.push(arg.as_str());
                }
            }
            if text_parts.is_empty() {
                eprintln!("covenant ignore check: missing <text>");
                std::process::exit(2);
            }
            let text = text_parts.join(" ");
            write_frame(&mut stream, &Request::IgnoreCheck { text }).await?;
            match read_frame::<_, Response>(&mut stream).await? {
                Response::IgnoreReport {
                    ignored,
                    matched_pattern,
                    rules_loaded,
                } => {
                    if as_json {
                        println!(
                            "{}",
                            serde_json::to_string(&ignore_report_json(
                                ignored,
                                matched_pattern.as_deref(),
                                rules_loaded
                            ))?
                        );
                    } else if ignored {
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

fn peer_list_json(
    limit: usize,
    filter_pubkey_prefix: Option<&str>,
    peers: &[PeerSummary],
    operator_pubkey_b58: &str,
    truncated: bool,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "peer_list",
        "limit": limit,
        "filter_pubkey_prefix": filter_pubkey_prefix,
        "matched_count": peers.len(),
        "peers": peers,
        "operator_pubkey_b58": operator_pubkey_b58,
        "truncated": truncated,
    })
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

fn resource_name(resource: ResourceKind) -> &'static str {
    match resource {
        ResourceKind::Compute => "compute",
        ResourceKind::Memory => "memory",
        ResourceKind::Tool => "tool",
        ResourceKind::Message => "message",
        ResourceKind::Registration => "registration",
    }
}

fn receipt_list_json(
    limit: usize,
    since_ms: Option<u64>,
    receipts: &[SettlementReceipt],
) -> serde_json::Value {
    serde_json::json!({
        "kind": "receipt_list",
        "limit": limit,
        "since_ms": since_ms,
        "receipts": receipts,
    })
}

fn intent_result_json(
    intent_id: uuid::Uuid,
    status: &str,
    text: &str,
    sources: &[String],
    settlement: Option<&SettlementReceipt>,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "intent_result",
        "intent_id": intent_id,
        "status": status,
        "text": text,
        "sources": sources,
        "settlement": settlement,
    })
}

fn ping_json() -> serde_json::Value {
    serde_json::json!({
        "kind": "daemon_ping",
        "status": "ok",
    })
}

fn capability_list_json(limit: usize, capabilities: &[SignedCapability]) -> serde_json::Value {
    serde_json::json!({
        "kind": "capability_list",
        "limit": limit,
        "capabilities": capabilities,
    })
}

fn capability_grant_json(
    subject_display: &str,
    action: &str,
    signature_b58: &str,
    scope: Option<&serde_json::Value>,
    expires_at: Option<u64>,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "capability_granted",
        "subject_display": subject_display,
        "action": action,
        "signature_b58": signature_b58,
        "scope": scope,
        "expires_at": expires_at,
    })
}

fn capability_revoke_json(signature_b58: &str, removed: bool) -> serde_json::Value {
    serde_json::json!({
        "kind": "capability_revoked",
        "signature_b58": signature_b58,
        "removed": removed,
    })
}

fn capabilities_purge_json(before_ms: u64, purged: u64) -> serde_json::Value {
    serde_json::json!({
        "kind": "capabilities_purged",
        "before_ms": before_ms,
        "purged": purged,
    })
}

fn peers_purge_json(before_ms: u64, purged: u64) -> serde_json::Value {
    serde_json::json!({
        "kind": "peers_purged",
        "before_ms": before_ms,
        "purged": purged,
    })
}

fn peers_rotate_json(token_b58: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "peer_token_rotated",
        "token_b58": token_b58,
    })
}

fn a2a_compact_json(dropped: u64) -> serde_json::Value {
    serde_json::json!({
        "kind": "a2a_compacted",
        "dropped": dropped,
    })
}

fn audit_purge_json(before_ms: u64, purged: u64) -> serde_json::Value {
    serde_json::json!({
        "kind": "audit_purged",
        "before_ms": before_ms,
        "purged": purged,
    })
}

fn audit_recent_json(
    limit: usize,
    since_ms: Option<u64>,
    events: &[AuditEvent],
) -> serde_json::Value {
    serde_json::json!({
        "kind": "audit_recent",
        "limit": limit,
        "since_ms": since_ms,
        "events": events,
    })
}

fn audit_verify_json(report: &AuditIntegrityReport) -> serde_json::Value {
    serde_json::json!({
        "kind": "audit_integrity",
        "report": report,
    })
}

fn memory_purge_json(tier: Option<MemoryTier>, before_ms: u64, purged: u64) -> serde_json::Value {
    serde_json::json!({
        "kind": "memory_purged",
        "tier": tier.map(memory_tier_slug),
        "before_ms": before_ms,
        "purged": purged,
    })
}

fn memory_compaction_json(outcome: &MemoryCompactionOutcome) -> serde_json::Value {
    serde_json::json!({
        "kind": "memory_compacted",
        "outcome": outcome,
    })
}

fn memory_compaction_plan_json(outcome: &MemoryCompactionOutcome) -> serde_json::Value {
    serde_json::json!({
        "kind": "memory_compaction_plan",
        "outcome": outcome,
        "expected_receipt_changes": {
            "mode": "none",
            "records": [],
            "reason": "dry-run compaction planning does not mutate memory or settlement receipts"
        }
    })
}

fn memory_receipt_backfill_plan_json(
    limit: usize,
    memories: &[MemoryRecord],
    receipts: &[SettlementReceipt],
) -> serde_json::Value {
    let memory_receipts = receipts
        .iter()
        .filter(|receipt| receipt.resource == ResourceKind::Memory)
        .collect::<Vec<_>>();
    let correlated = memory_receipts
        .iter()
        .filter_map(|receipt| receipt.memory_record_id)
        .collect::<HashSet<_>>();
    let mut used_memory = HashSet::new();
    let mut records = Vec::new();
    let mut unmatched_legacy_receipts = Vec::new();

    let legacy_receipts = memory_receipts
        .iter()
        .copied()
        .filter(|receipt| receipt.memory_record_id.is_none())
        .collect::<Vec<_>>();

    for receipt in &legacy_receipts {
        let candidate = memories.iter().find(|memory| {
            memory.owner.pubkey == receipt.payer.pubkey
                && !correlated.contains(&memory.id)
                && !used_memory.contains(&memory.id)
        });

        if let Some(memory) = candidate {
            used_memory.insert(memory.id);
            records.push(serde_json::json!({
                "receipt_id": receipt.id,
                "memory_record_id": memory.id,
                "payer_display": receipt.payer.display,
                "payer_pubkey": receipt.payer.pubkey_base58(),
                "memory_owner_display": memory.owner.display,
                "memory_owner_pubkey": memory.owner.pubkey_base58(),
                "credits_consumed": receipt.credits_consumed,
                "status": "candidate",
                "reason": "legacy memory receipt has no memory_record_id and the same owner has an uncorrelated memory record"
            }));
        } else {
            unmatched_legacy_receipts.push(serde_json::json!({
                "receipt_id": receipt.id,
                "payer_display": receipt.payer.display,
                "payer_pubkey": receipt.payer.pubkey_base58(),
                "credits_consumed": receipt.credits_consumed,
                "reason": "no uncorrelated memory record for receipt payer in the requested window"
            }));
        }
    }

    let unmatched_memory_records = memories
        .iter()
        .filter(|memory| !correlated.contains(&memory.id) && !used_memory.contains(&memory.id))
        .map(|memory| {
            serde_json::json!({
                "memory_record_id": memory.id,
                "owner_display": memory.owner.display,
                "owner_pubkey": memory.owner.pubkey_base58(),
                "tier": memory_tier_slug(memory.tier),
                "reason": "no legacy receipt candidate for memory owner in the requested window"
            })
        })
        .collect::<Vec<_>>();

    serde_json::json!({
        "kind": "memory_receipt_backfill_plan",
        "mode": "dry_run",
        "limit": limit,
        "mutation_supported": false,
        "records": records,
        "unmatched_legacy_receipts": unmatched_legacy_receipts,
        "unmatched_memory_records": unmatched_memory_records,
        "refusal": {
            "apply_supported": false,
            "reason": "receipt backfill mutation is not implemented; review this plan before adding an explicit mutation path with audit evidence"
        }
    })
}

fn memory_read_json(
    mode: &str,
    tier: Option<MemoryTier>,
    limit: usize,
    query: Option<&str>,
    min_relevance: Option<f32>,
    records: &[MemoryRecord],
) -> serde_json::Value {
    serde_json::json!({
        "kind": "memory_read",
        "mode": mode,
        "tier": tier.map(memory_tier_slug),
        "limit": limit,
        "query": query,
        "min_relevance": min_relevance,
        "records": records,
    })
}

fn ignore_report_json(
    ignored: bool,
    matched_pattern: Option<&str>,
    rules_loaded: usize,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "ignore_report",
        "ignored": ignored,
        "matched_pattern": matched_pattern,
        "rules_loaded": rules_loaded,
    })
}

fn tool_list_json(tools: &[ToolSpec]) -> serde_json::Value {
    serde_json::json!({
        "kind": "tool_list",
        "tools": tools,
    })
}

fn tool_result_json(
    name: &str,
    content: &[covenant_mcp::Content],
    is_error: bool,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "tool_result",
        "name": name,
        "content": content,
        "is_error": is_error,
    })
}

fn receipt_batch_list_json(limit: usize, batches: &[ReceiptBatchSummary]) -> serde_json::Value {
    serde_json::json!({
        "kind": "receipt_batch_list",
        "limit": limit,
        "batches": batches,
    })
}

fn chain_status_json(status: &ChainStatus) -> serde_json::Value {
    serde_json::json!({
        "kind": "chain_status",
        "status": status,
    })
}

fn verify_report_json(
    window: usize,
    checks: &[VerifyCheck],
    drift: &[VerifyDrift],
    orphans_total: u64,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "verify_report",
        "window": window,
        "checks": checks,
        "drift": drift,
        "orphans_total": orphans_total,
    })
}

fn flush_receipts_json(
    limit: usize,
    batch: &ReceiptBatchSummary,
    receipts_updated: u64,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "receipt_batch_flushed",
        "limit": limit,
        "receipts_updated": receipts_updated,
        "batch": batch,
    })
}

fn a2a_status_json(
    limit: usize,
    min_lease_age_ms: Option<u64>,
    deadline_within_ms: Option<u64>,
    state_filter: Option<A2ATaskQueueState>,
    tasks: &[A2ATaskQueueEntry],
    results: &[A2ATaskResult],
) -> serde_json::Value {
    serde_json::json!({
        "kind": "a2a_status",
        "limit": limit,
        "min_lease_age_ms": min_lease_age_ms,
        "deadline_within_ms": deadline_within_ms,
        "state_filter": state_filter,
        "tasks": tasks,
        "results": results,
    })
}

fn a2a_retry_json(report: &A2AAutoRetryReport) -> serde_json::Value {
    serde_json::json!({
        "kind": "a2a_auto_retry",
        "report": report,
    })
}

fn peer_revoke_json(outcome: &RevokeOutcome) -> serde_json::Value {
    serde_json::json!({
        "kind": "peer_revoke",
        "outcome": outcome,
    })
}

fn intents_resume_error_code(message: &str) -> &'static str {
    if message.contains("no BudgetExhausted audit row found") {
        return "no_budget_exhausted_row";
    }
    if message.contains("missing <intent-id>") {
        return "missing_intent_id";
    }
    if message.contains("intent-id must be a uuid") || message.contains("intent-id must be a UUID")
    {
        return "invalid_intent_id";
    }
    if message.contains("pass either <intent-id> or latest") {
        return "conflicting_flags";
    }
    "error"
}

fn intents_resume_ok_json(
    mode: &str,
    intent_id: uuid::Uuid,
    status: &str,
    text: &str,
    sources: &[String],
    settlement: &Option<SettlementReceipt>,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "intents_resume",
        "ok": true,
        "mode": mode,
        "intent_id": intent_id,
        "status": status,
        "text": text,
        "sources": sources,
        "settlement": settlement,
    })
}

fn intents_resume_error_json(
    mode: &str,
    intent_id: Option<uuid::Uuid>,
    code: &str,
    message: &str,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "intents_resume",
        "ok": false,
        "mode": mode,
        "intent_id": intent_id,
        "error": {
            "code": code,
            "message": message,
        }
    })
}

fn peer_revoke_is_failure(outcome: &RevokeOutcome) -> bool {
    matches!(
        outcome,
        RevokeOutcome::NotFound
            | RevokeOutcome::Ambiguous { .. }
            | RevokeOutcome::SelfRevokeForbidden(_)
    )
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
    use covenant_a2a::{A2ATask, A2ATaskQueueState};
    use covenant_types::{AgentId, Capability};

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
    fn a2a_duplicate_risk_accepts_cli_spellings() {
        assert_eq!(
            parse_duplicate_risk("idempotent").unwrap(),
            A2ADuplicateRisk::Idempotent
        );
        assert_eq!(
            parse_duplicate_risk("operator-accepted").unwrap(),
            A2ADuplicateRisk::OperatorAccepted
        );
        assert_eq!(
            parse_duplicate_risk("operator_accepted").unwrap(),
            A2ADuplicateRisk::OperatorAccepted
        );
        assert!(parse_duplicate_risk("unsafe").is_err());
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
    fn peer_list_json_echoes_prefix_and_match_count() {
        let p = make_peer(7, "alice@host", false);
        let q = make_peer(8, "bob@host", true);
        let value = peer_list_json(20, Some("ABcde"), &[p.clone(), q.clone()], "OPB58", false);
        assert_eq!(value["kind"], "peer_list");
        assert_eq!(value["limit"], 20);
        assert_eq!(value["filter_pubkey_prefix"], "ABcde");
        assert_eq!(value["matched_count"], 2);
        assert_eq!(value["operator_pubkey_b58"], "OPB58");
        assert_eq!(value["truncated"], false);
        assert_eq!(value["peers"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn peer_list_json_omits_prefix_when_inactive() {
        let p = make_peer(7, "alice@host", false);
        let value = peer_list_json(20, None, &[p], "OPB58", false);
        assert!(
            value["filter_pubkey_prefix"].is_null(),
            "filter_pubkey_prefix must be null when --prefix is not supplied so machine consumers see a stable absent-filter shape: {value:?}",
        );
        assert_eq!(value["matched_count"], 1);
    }

    #[test]
    fn peer_list_json_reports_zero_match_count_for_empty_response() {
        let value = peer_list_json(20, Some("nomatch"), &[], "OPB58", false);
        assert_eq!(value["matched_count"], 0);
        assert_eq!(value["filter_pubkey_prefix"], "nomatch");
        assert_eq!(value["peers"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn peer_list_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &[
            "filter_pubkey_prefix",
            "kind",
            "limit",
            "matched_count",
            "operator_pubkey_b58",
            "peers",
            "truncated",
        ];

        fn assert_shape(value: &serde_json::Value) {
            let object = value.as_object().expect("peer_list_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "peer_list_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("peer_list"));
            assert!(
                value["limit"].is_u64(),
                "limit must serialize as a non-negative integer, not a string: {value}",
            );
            assert!(
                value["filter_pubkey_prefix"].is_string()
                    || value["filter_pubkey_prefix"].is_null(),
                "filter_pubkey_prefix must be string or null (never integer / array): {value}",
            );
            assert!(
                value["matched_count"].is_u64(),
                "matched_count must serialize as a non-negative integer, not a string-of-integer: {value}",
            );
            assert!(
                value["peers"].is_array(),
                "peers must be an array: {value}",
            );
            assert!(
                value["operator_pubkey_b58"].is_string(),
                "operator_pubkey_b58 must be a string: {value}",
            );
            assert!(
                value["truncated"].is_boolean(),
                "truncated must be a boolean, not 0/1: {value}",
            );
        }

        let populated = peer_list_json(
            20,
            Some("ABcde"),
            &[
                make_peer(7, "alice@host", false),
                make_peer(8, "bob@host", true),
            ],
            "OPB58",
            true,
        );
        assert_shape(&populated);

        let empty = peer_list_json(20, None, &[], "OPB58", false);
        assert_shape(&empty);
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
    fn peer_revoke_json_renders_stable_ambiguous_shape() {
        let p = make_peer(7, "alice@host", false);
        let value = peer_revoke_json(&RevokeOutcome::Ambiguous {
            matches: vec![p.clone()],
            truncated: true,
        });
        assert_eq!(value["kind"], "peer_revoke");
        assert_eq!(value["outcome"]["type"], "ambiguous");
        assert_eq!(value["outcome"]["truncated"], true);
        assert_eq!(
            value["outcome"]["matches"][0]["token_prefix"],
            p.token_prefix
        );
        let text = serde_json::to_string(&value).unwrap();
        assert!(!text.contains("PeerToken"), "{text}");
    }

    #[test]
    fn intents_resume_json_renders_stable_error_shape() {
        let intent_id = uuid::Uuid::nil();
        let value = intents_resume_error_json(
            "latest",
            Some(intent_id),
            "daemon_error",
            "budget exhausted; try again later",
        );
        assert_eq!(value["kind"], "intents_resume");
        assert_eq!(value["ok"], false);
        assert_eq!(value["mode"], "latest");
        assert_eq!(value["intent_id"], intent_id.to_string());
        assert_eq!(value["error"]["code"], "daemon_error");
        assert!(value["error"]["message"].as_str().is_some());
    }

    #[test]
    fn intents_resume_json_renders_stable_ok_shape() {
        let intent_id = uuid::Uuid::nil();
        let value = intents_resume_ok_json(
            "explicit",
            intent_id,
            "ok",
            "resumed intent text",
            &["a".into(), "b".into()],
            &None,
        );
        assert_eq!(value["kind"], "intents_resume");
        assert_eq!(value["ok"], true);
        assert_eq!(value["mode"], "explicit");
        assert_eq!(value["intent_id"], intent_id.to_string());
        assert_eq!(value["status"], "ok");
        assert_eq!(value["sources"][0], "a");
        assert!(value["settlement"].is_null());
    }

    #[test]
    fn receipt_list_json_renders_stable_shape() {
        let payer = AgentId::new("payer@local", [3u8; 32]);
        let receipt = SettlementReceipt {
            id: uuid::Uuid::nil(),
            payer: payer.clone(),
            resource: ResourceKind::Memory,
            memory_record_id: Some(uuid::Uuid::nil()),
            credits_consumed: 42,
            settled_at: 1_700_000_000_000,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        };
        let receipts = [receipt];
        let value = receipt_list_json(10, Some(1_699_999_999_000), &receipts);
        assert_eq!(value["kind"], "receipt_list");
        assert_eq!(value["limit"], 10);
        assert_eq!(value["since_ms"], 1_699_999_999_000u64);
        assert_eq!(value["receipts"][0]["payer"]["display"], "payer@local");
        assert_eq!(
            value["receipts"][0]["payer"]["pubkey"],
            payer.pubkey_base58()
        );
        assert_eq!(value["receipts"][0]["resource"], "memory");
        assert_eq!(
            value["receipts"][0]["memory_record_id"],
            uuid::Uuid::nil().to_string()
        );
        assert_eq!(value["receipts"][0]["credits_consumed"], 42);
        assert!(value["receipts"][0]["tx_sig"].is_null());

        let unfiltered = receipt_list_json(10, None, &receipts);
        assert!(unfiltered["since_ms"].is_null());
    }

    #[test]
    fn receipt_list_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["kind", "limit", "receipts", "since_ms"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("receipt_list_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "receipt_list_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("receipt_list"));
            assert!(
                value["limit"].is_u64(),
                "limit must serialize as a non-negative integer, not a string: {value}",
            );
            assert!(
                value["since_ms"].is_u64() || value["since_ms"].is_null(),
                "since_ms must be u64-or-null (never a string-of-integer or other type): {value}",
            );
            assert!(
                value["receipts"].is_array(),
                "receipts must be an array: {value}",
            );
        }

        let receipt = SettlementReceipt {
            id: uuid::Uuid::nil(),
            payer: AgentId::new("payer@local", [4u8; 32]),
            resource: ResourceKind::Memory,
            memory_record_id: Some(uuid::Uuid::nil()),
            credits_consumed: 1,
            settled_at: 1_700_000_000_000,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        };
        let receipts = [receipt];

        assert_shape(&receipt_list_json(10, Some(1_699_999_999_000), &receipts));
        assert_shape(&receipt_list_json(10, None, &receipts));
        assert_shape(&receipt_list_json(10, None, &[]));
    }

    #[test]
    fn intent_result_json_renders_stable_shape() {
        let value = intent_result_json(
            uuid::Uuid::nil(),
            "ok",
            "phase 0 echo",
            &["research".into()],
            None,
        );

        assert_eq!(value["kind"], "intent_result");
        assert_eq!(value["intent_id"], uuid::Uuid::nil().to_string());
        assert_eq!(value["status"], "ok");
        assert_eq!(value["text"], "phase 0 echo");
        assert_eq!(value["sources"][0], "research");
        assert!(value["settlement"].is_null());
    }

    #[test]
    fn intent_result_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &[
            "intent_id",
            "kind",
            "settlement",
            "sources",
            "status",
            "text",
        ];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("intent_result_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "intent_result_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("intent_result"));
            assert!(
                value["intent_id"].is_string(),
                "intent_id must be a string (uuid serialization), not bytes/array: {value}",
            );
            assert!(
                value["status"].is_string(),
                "status must be a string: {value}",
            );
            assert!(value["text"].is_string(), "text must be a string: {value}");
            assert!(
                value["sources"].is_array(),
                "sources must be an array: {value}",
            );
            assert!(
                value["settlement"].is_object() || value["settlement"].is_null(),
                "settlement must be object-or-null (never integer / array): {value}",
            );
        }

        let settlement = SettlementReceipt {
            id: uuid::Uuid::nil(),
            payer: AgentId::new("payer@local", [4u8; 32]),
            resource: ResourceKind::Memory,
            memory_record_id: Some(uuid::Uuid::nil()),
            credits_consumed: 1,
            settled_at: 1_700_000_000_000,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        };

        assert_shape(&intent_result_json(
            uuid::Uuid::nil(),
            "ok",
            "phase 0 echo",
            &["research".into()],
            Some(&settlement),
        ));
        assert_shape(&intent_result_json(uuid::Uuid::nil(), "ok", "", &[], None));
    }

    #[test]
    fn ping_json_renders_stable_shape() {
        let value = ping_json();
        assert_eq!(value["kind"], "daemon_ping");
        assert_eq!(value["status"], "ok");
    }

    #[test]
    fn capability_list_json_renders_stable_shape() {
        let subject = AgentId::new("subject@local", [1u8; 32]);
        let granted_by = AgentId::new("issuer@local", [2u8; 32]);
        let signed = SignedCapability {
            capability: Capability {
                subject: subject.clone(),
                action: "tool.call.echo".into(),
                scope: serde_json::json!({"version": 1}),
                granted_by: granted_by.clone(),
                expires_at: None,
            },
            signature: [9u8; 64],
        };

        let value = capability_list_json(5, &[signed]);
        assert_eq!(value["kind"], "capability_list");
        assert_eq!(value["limit"], 5);
        assert_eq!(
            value["capabilities"][0]["capability"]["action"],
            "tool.call.echo"
        );
        assert_eq!(
            value["capabilities"][0]["capability"]["subject"]["pubkey"],
            subject.pubkey_base58()
        );
        assert_eq!(
            value["capabilities"][0]["capability"]["granted_by"]["display"],
            granted_by.display
        );
        assert_eq!(
            value["capabilities"][0]["capability"]["scope"]["version"],
            1
        );
        assert!(value["capabilities"][0]["capability"]["expires_at"].is_null());
        assert!(value["capabilities"][0]["signature"]
            .as_str()
            .is_some_and(|s| !s.is_empty()));
    }

    #[test]
    fn capability_list_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["capabilities", "kind", "limit"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("capability_list_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "capability_list_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("capability_list"));
            assert!(
                value["limit"].is_u64(),
                "limit must serialize as a non-negative integer, not a string: {value}",
            );
            assert!(
                value["capabilities"].is_array(),
                "capabilities must be an array: {value}",
            );
        }

        let signed = SignedCapability {
            capability: Capability {
                subject: AgentId::new("subject@local", [1u8; 32]),
                action: "tool.call.echo".into(),
                scope: serde_json::json!({"version": 1}),
                granted_by: AgentId::new("issuer@local", [2u8; 32]),
                expires_at: None,
            },
            signature: [9u8; 64],
        };

        assert_shape(&capability_list_json(5, &[signed]));
        assert_shape(&capability_list_json(5, &[]));
    }

    #[test]
    fn capability_grant_json_renders_stable_shape() {
        let scope = serde_json::json!({"version": 1, "tools": ["echo"]});
        let value = capability_grant_json(
            "operator@local",
            "tool.call.echo",
            "sigb58",
            Some(&scope),
            Some(1_700_000_000_000),
        );

        assert_eq!(value["kind"], "capability_granted");
        assert_eq!(value["subject_display"], "operator@local");
        assert_eq!(value["action"], "tool.call.echo");
        assert_eq!(value["signature_b58"], "sigb58");
        assert_eq!(value["scope"]["version"], 1);
        assert_eq!(value["expires_at"], 1_700_000_000_000u64);

        let unscoped = capability_grant_json("operator@local", "memory.read", "sigb58", None, None);
        assert!(unscoped["scope"].is_null());
        assert!(unscoped["expires_at"].is_null());
    }

    #[test]
    fn capability_grant_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &[
            "action",
            "expires_at",
            "kind",
            "scope",
            "signature_b58",
            "subject_display",
        ];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("capability_grant_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "capability_grant_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("capability_granted"));
            assert!(
                value["subject_display"].is_string(),
                "subject_display must be a string: {value}",
            );
            assert!(
                value["action"].is_string(),
                "action must be a string: {value}",
            );
            assert!(
                value["signature_b58"].is_string(),
                "signature_b58 must be a string: {value}",
            );
            assert!(
                value["scope"].is_object() || value["scope"].is_null(),
                "scope must be a structured object or null, never a string blob: {value}",
            );
            assert!(
                value["expires_at"].is_u64() || value["expires_at"].is_null(),
                "expires_at must be a non-negative integer or null, not a string: {value}",
            );
        }

        let scope = serde_json::json!({"version": 1, "tools": ["echo"]});
        assert_shape(&capability_grant_json(
            "operator@local",
            "tool.call.echo",
            "sigb58",
            Some(&scope),
            Some(1_700_000_000_000),
        ));
        assert_shape(&capability_grant_json(
            "operator@local",
            "memory.read",
            "sigb58",
            None,
            None,
        ));
    }

    #[test]
    fn capability_revoke_json_renders_stable_shape() {
        let removed = capability_revoke_json("sigb58", true);
        assert_eq!(removed["kind"], "capability_revoked");
        assert_eq!(removed["signature_b58"], "sigb58");
        assert_eq!(removed["removed"], true);

        let absent = capability_revoke_json("sigb58", false);
        assert_eq!(absent["kind"], "capability_revoked");
        assert_eq!(absent["signature_b58"], "sigb58");
        assert_eq!(absent["removed"], false);
    }

    #[test]
    fn capability_revoke_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["kind", "removed", "signature_b58"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("capability_revoke_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "capability_revoke_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("capability_revoked"));
            assert!(
                value["signature_b58"].is_string(),
                "signature_b58 must be a string: {value}",
            );
            assert!(
                value["removed"].is_boolean(),
                "removed must be a JSON boolean, not 0/1 or string: {value}",
            );
        }

        assert_shape(&capability_revoke_json("sigb58", true));
        assert_shape(&capability_revoke_json("sigb58", false));
    }

    #[test]
    fn capabilities_purge_json_renders_stable_shape() {
        let value = capabilities_purge_json(1_700_000_000_000, 3);
        assert_eq!(value["kind"], "capabilities_purged");
        assert_eq!(value["before_ms"], 1_700_000_000_000u64);
        assert_eq!(value["purged"], 3);
    }

    #[test]
    fn peers_purge_json_renders_stable_shape() {
        let value = peers_purge_json(1_700_000_000_000, 3);
        assert_eq!(value["kind"], "peers_purged");
        assert_eq!(value["before_ms"], 1_700_000_000_000u64);
        assert_eq!(value["purged"], 3);
    }

    #[test]
    fn peers_rotate_json_renders_stable_shape() {
        let value = peers_rotate_json("tokenb58");
        assert_eq!(value["kind"], "peer_token_rotated");
        assert_eq!(value["token_b58"], "tokenb58");
    }

    #[test]
    fn a2a_compact_json_renders_stable_shape() {
        let value = a2a_compact_json(3);
        assert_eq!(value["kind"], "a2a_compacted");
        assert_eq!(value["dropped"], 3);
    }

    #[test]
    fn a2a_retry_json_renders_stable_shape() {
        let mut report = A2AAutoRetryReport::new(A2AAutoRetryPolicy {
            enabled: true,
            min_lease_age_ms: 300_000,
            max_attempts: 3,
            max_requeues: 1,
            scan_limit: 100,
        });
        report.considered = 2;
        report.requeued.push(covenant_a2a::A2AAutoRetryRequeued {
            task_id: uuid::Uuid::nil(),
            lease_id: uuid::Uuid::nil(),
            attempt: 1,
            idempotency_key: "task:key".into(),
        });
        report.skipped.push(covenant_a2a::A2AAutoRetrySkipped {
            task_id: uuid::Uuid::nil(),
            reason: covenant_a2a::A2AAutoRetrySkipReason::UnsafeDuplicateSafety,
            attempt: 1,
            lease_age_ms: Some(300_000),
        });

        let value = a2a_retry_json(&report);
        assert_eq!(value["kind"], "a2a_auto_retry");
        assert_eq!(value["report"]["policy"]["enabled"], true);
        assert_eq!(value["report"]["considered"], 2);
        assert_eq!(
            value["report"]["requeued"][0]["idempotency_key"],
            "task:key"
        );
        assert_eq!(
            value["report"]["skipped"][0]["reason"],
            "unsafe_duplicate_safety"
        );
    }

    #[test]
    fn audit_purge_json_renders_stable_shape() {
        let value = audit_purge_json(1_700_000_000_000, 3);
        assert_eq!(value["kind"], "audit_purged");
        assert_eq!(value["before_ms"], 1_700_000_000_000u64);
        assert_eq!(value["purged"], 3);
    }

    #[test]
    fn audit_recent_json_renders_stable_shape() {
        let event = AuditEvent {
            id: uuid::Uuid::nil(),
            timestamp_ms: 1_700_000_000_000,
            issuer: covenant_types::AgentId::new("operator@covenant", [7; 32]),
            kind: AuditKind::CapabilityGranted {
                subject_display: "operator@covenant".into(),
                action: "tool.call.echo".into(),
                granted_by_display: "operator@covenant".into(),
                signature_b58: "sigb58".into(),
            },
        };

        let value = audit_recent_json(5, Some(1_699_999_999_000), &[event]);
        assert_eq!(value["kind"], "audit_recent");
        assert_eq!(value["limit"], 5);
        assert_eq!(value["since_ms"], 1_699_999_999_000u64);
        assert_eq!(value["events"][0]["timestamp_ms"], 1_700_000_000_000u64);
        assert_eq!(value["events"][0]["kind"]["type"], "capability_granted");
        assert_eq!(value["events"][0]["kind"]["action"], "tool.call.echo");

        let empty = audit_recent_json(5, None, &[]);
        assert_eq!(empty["events"].as_array().unwrap().len(), 0);
        assert!(empty["since_ms"].is_null());
    }

    #[test]
    fn audit_recent_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["events", "kind", "limit", "since_ms"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("audit_recent_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "audit_recent_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("audit_recent"));
            assert!(
                value["limit"].is_u64(),
                "limit must serialize as a non-negative integer, not a string: {value}",
            );
            assert!(
                value["since_ms"].is_u64() || value["since_ms"].is_null(),
                "since_ms must be u64-or-null (never a string-of-integer or other type): {value}",
            );
            assert!(
                value["events"].is_array(),
                "events must be an array: {value}",
            );
        }

        let event = AuditEvent {
            id: uuid::Uuid::nil(),
            timestamp_ms: 1_700_000_000_000,
            issuer: covenant_types::AgentId::new("operator@covenant", [7; 32]),
            kind: AuditKind::CapabilityGranted {
                subject_display: "operator@covenant".into(),
                action: "tool.call.echo".into(),
                granted_by_display: "operator@covenant".into(),
                signature_b58: "sigb58".into(),
            },
        };
        let events = [event];

        assert_shape(&audit_recent_json(5, Some(1_699_999_999_000), &events));
        assert_shape(&audit_recent_json(5, Some(1_699_999_999_000), &[]));
        assert_shape(&audit_recent_json(5, None, &[]));
    }

    #[test]
    fn audit_verify_json_renders_stable_shape() {
        let report = AuditIntegrityReport {
            events: 2,
            anchors: 2,
            valid: true,
            root_hash_hex: "ab".repeat(32),
            failures: vec![],
        };

        let value = audit_verify_json(&report);
        assert_eq!(value["kind"], "audit_integrity");
        assert_eq!(value["report"]["events"], 2);
        assert_eq!(value["report"]["anchors"], 2);
        assert_eq!(value["report"]["valid"], true);
        assert_eq!(
            value["report"]["root_hash_hex"]
                .as_str()
                .unwrap_or_default()
                .len(),
            64
        );
        assert_eq!(
            value["report"]["failures"].as_array().map(Vec::len),
            Some(0)
        );
    }

    #[test]
    fn audit_verify_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["kind", "report"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("audit_verify_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "audit_verify_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("audit_integrity"));
            assert!(
                value["report"].is_object(),
                "report must be a structured object, not a string blob: {value}",
            );
        }

        let valid = AuditIntegrityReport {
            events: 2,
            anchors: 2,
            valid: true,
            root_hash_hex: "ab".repeat(32),
            failures: vec![],
        };
        let invalid = AuditIntegrityReport {
            events: 5,
            anchors: 4,
            valid: false,
            root_hash_hex: "cd".repeat(32),
            failures: vec!["chain hash mismatch at event 3".into()],
        };

        assert_shape(&audit_verify_json(&valid));
        assert_shape(&audit_verify_json(&invalid));
    }

    #[test]
    fn memory_purge_json_renders_stable_shape() {
        let value = memory_purge_json(Some(MemoryTier::Working), 1_700_000_000_000, 3);
        assert_eq!(value["kind"], "memory_purged");
        assert_eq!(value["tier"], "working");
        assert_eq!(value["before_ms"], 1_700_000_000_000u64);
        assert_eq!(value["purged"], 3);

        let all_tiers = memory_purge_json(None, 1_700_000_000_000, 0);
        assert!(all_tiers["tier"].is_null());
    }

    #[test]
    fn memory_compaction_json_renders_stable_shape() {
        let outcome = MemoryCompactionOutcome {
            mode: MemoryRepairMode::DryRun,
            would_change: true,
            changed: false,
            deleted: vec![],
            stale_marked: vec![],
            parents_detached: vec![],
        };

        let value = memory_compaction_json(&outcome);
        assert_eq!(value["kind"], "memory_compacted");
        assert_eq!(value["outcome"]["mode"], "dry_run");
        assert_eq!(value["outcome"]["would_change"], true);
        assert_eq!(value["outcome"]["changed"], false);
        assert_eq!(
            value["outcome"]["deleted"].as_array().map(Vec::len),
            Some(0)
        );
        assert_eq!(
            value["outcome"]["parents_detached"]
                .as_array()
                .map(Vec::len),
            Some(0)
        );
    }

    #[test]
    fn memory_compaction_plan_json_renders_stable_shape() {
        let outcome = MemoryCompactionOutcome {
            mode: MemoryRepairMode::DryRun,
            would_change: true,
            changed: false,
            deleted: vec![uuid::Uuid::nil()],
            stale_marked: vec![],
            parents_detached: vec![],
        };

        let value = memory_compaction_plan_json(&outcome);
        assert_eq!(value["kind"], "memory_compaction_plan");
        assert_eq!(value["outcome"]["mode"], "dry_run");
        assert_eq!(value["outcome"]["changed"], false);
        assert_eq!(value["expected_receipt_changes"]["mode"], "none");
        assert_eq!(
            value["expected_receipt_changes"]["records"]
                .as_array()
                .map(Vec::len),
            Some(0)
        );
    }

    #[test]
    fn memory_receipt_backfill_plan_json_pairs_legacy_receipts_by_owner() {
        let owner = AgentId::new("owner@local", [4u8; 32]);
        let memory_id = uuid::Uuid::from_u128(10);
        let memory = MemoryRecord {
            id: memory_id,
            tier: MemoryTier::Working,
            owner: owner.clone(),
            text: "legacy memory".into(),
            embedding: Vec::new(),
            metadata: serde_json::json!({}),
            created_at: 1,
            parent: None,
        };
        let receipt_id = uuid::Uuid::from_u128(20);
        let receipt = SettlementReceipt {
            id: receipt_id,
            payer: owner.clone(),
            resource: ResourceKind::Memory,
            memory_record_id: None,
            credits_consumed: 3,
            settled_at: 2,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        };

        let value = memory_receipt_backfill_plan_json(100, &[memory], &[receipt]);
        assert_eq!(value["kind"], "memory_receipt_backfill_plan");
        assert_eq!(value["mode"], "dry_run");
        assert_eq!(value["mutation_supported"], false);
        assert_eq!(value["records"].as_array().map(Vec::len), Some(1));
        assert_eq!(value["records"][0]["receipt_id"], receipt_id.to_string());
        assert_eq!(
            value["records"][0]["memory_record_id"],
            memory_id.to_string()
        );
        assert_eq!(value["records"][0]["status"], "candidate");
        assert_eq!(
            value["unmatched_legacy_receipts"].as_array().map(Vec::len),
            Some(0)
        );
        assert_eq!(
            value["unmatched_memory_records"].as_array().map(Vec::len),
            Some(0)
        );
        assert_eq!(value["refusal"]["apply_supported"], false);
    }

    #[test]
    fn memory_receipt_backfill_plan_json_lists_unmatched_rows() {
        let memory_owner = AgentId::new("memory@local", [5u8; 32]);
        let payer = AgentId::new("payer@local", [6u8; 32]);
        let memory = MemoryRecord {
            id: uuid::Uuid::from_u128(30),
            tier: MemoryTier::LongTerm,
            owner: memory_owner,
            text: "unmatched memory".into(),
            embedding: Vec::new(),
            metadata: serde_json::json!({}),
            created_at: 1,
            parent: None,
        };
        let receipt = SettlementReceipt {
            id: uuid::Uuid::from_u128(40),
            payer,
            resource: ResourceKind::Memory,
            memory_record_id: None,
            credits_consumed: 1,
            settled_at: 2,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        };

        let value = memory_receipt_backfill_plan_json(10, &[memory], &[receipt]);
        assert_eq!(value["records"].as_array().map(Vec::len), Some(0));
        assert_eq!(
            value["unmatched_legacy_receipts"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(
            value["unmatched_memory_records"].as_array().map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn memory_read_json_renders_stable_shape() {
        let owner = AgentId::new("owner@local", [4u8; 32]);
        let record = MemoryRecord {
            id: uuid::Uuid::nil(),
            tier: MemoryTier::Working,
            owner: owner.clone(),
            text: "memory read fixture".into(),
            embedding: vec![0.1, 0.2],
            metadata: serde_json::json!({"source": "test"}),
            created_at: 1_700_000_000_000,
            parent: None,
        };

        let value = memory_read_json(
            "search",
            Some(MemoryTier::Working),
            5,
            Some("memory read"),
            Some(0.6),
            &[record],
        );

        assert_eq!(value["kind"], "memory_read");
        assert_eq!(value["mode"], "search");
        assert_eq!(value["tier"], "working");
        assert_eq!(value["limit"], 5);
        assert_eq!(value["query"], "memory read");
        assert!(
            (value["min_relevance"].as_f64().unwrap() - 0.6).abs() < 1e-6,
            "min_relevance must round-trip the supplied threshold",
        );
        assert_eq!(value["records"][0]["text"], "memory read fixture");
        assert_eq!(value["records"][0]["tier"], "working");
        assert_eq!(value["records"][0]["owner"]["display"], owner.display);

        let recent = memory_read_json("recent", None, 3, None, None, &[]);
        assert!(recent["tier"].is_null());
        assert!(recent["query"].is_null());
        assert!(recent["min_relevance"].is_null());
        assert_eq!(recent["records"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn memory_read_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &[
            "kind",
            "limit",
            "min_relevance",
            "mode",
            "query",
            "records",
            "tier",
        ];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("memory_read_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "memory_read_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("memory_read"));
            assert!(value["mode"].is_string(), "mode must be a string: {value}");
            assert!(
                value["limit"].is_u64(),
                "limit must serialize as a non-negative integer, not a string: {value}",
            );
            assert!(
                value["tier"].is_string() || value["tier"].is_null(),
                "tier must be a string or null, never a structured object: {value}",
            );
            assert!(
                value["query"].is_string() || value["query"].is_null(),
                "query must be a string or null: {value}",
            );
            assert!(
                value["min_relevance"].is_f64() || value["min_relevance"].is_null(),
                "min_relevance must be a JSON number or null, never a string: {value}",
            );
            assert!(
                value["records"].is_array(),
                "records must be an array: {value}",
            );
        }

        let owner = AgentId::new("owner@local", [4u8; 32]);
        let record = MemoryRecord {
            id: uuid::Uuid::nil(),
            tier: MemoryTier::Working,
            owner,
            text: "memory read fixture".into(),
            embedding: vec![0.1, 0.2],
            metadata: serde_json::json!({"source": "test"}),
            created_at: 1_700_000_000_000,
            parent: None,
        };

        let search = memory_read_json(
            "search",
            Some(MemoryTier::Working),
            5,
            Some("memory read"),
            Some(0.6),
            &[record],
        );
        assert_shape(&search);

        let list = memory_read_json("recent", None, 3, None, None, &[]);
        assert_shape(&list);
    }

    #[test]
    fn ignore_report_json_renders_stable_shape() {
        let value = ignore_report_json(true, Some("*.pem"), 2);
        assert_eq!(value["kind"], "ignore_report");
        assert_eq!(value["ignored"], true);
        assert_eq!(value["matched_pattern"], "*.pem");
        assert_eq!(value["rules_loaded"], 2);

        let clear = ignore_report_json(false, None, 2);
        assert!(clear["matched_pattern"].is_null());
    }

    #[test]
    fn tool_list_json_renders_stable_shape() {
        let spec = ToolSpec {
            name: "echo".into(),
            description: "Echo text".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                }
            }),
        };

        let value = tool_list_json(&[spec]);
        assert_eq!(value["kind"], "tool_list");
        assert_eq!(value["tools"][0]["name"], "echo");
        assert_eq!(value["tools"][0]["description"], "Echo text");
        assert_eq!(value["tools"][0]["inputSchema"]["type"], "object");
        assert_eq!(
            value["tools"][0]["inputSchema"]["properties"]["text"]["type"],
            "string"
        );
    }

    #[test]
    fn tool_list_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["kind", "tools"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("tool_list_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "tool_list_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("tool_list"));
            assert!(
                value["tools"].is_array(),
                "tools must be an array of tool specs, not a string blob: {value}",
            );
        }

        let spec = ToolSpec {
            name: "echo".into(),
            description: "Echo text".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                }
            }),
        };

        assert_shape(&tool_list_json(&[spec]));
        assert_shape(&tool_list_json(&[]));
    }

    #[test]
    fn tool_result_json_renders_stable_shape() {
        let content = vec![
            covenant_mcp::Content::Text {
                text: "hello".into(),
            },
            covenant_mcp::Content::Json {
                value: serde_json::json!({ "ok": true }),
            },
        ];

        let value = tool_result_json("echo", &content, false);
        assert_eq!(value["kind"], "tool_result");
        assert_eq!(value["name"], "echo");
        assert_eq!(value["is_error"], false);
        assert_eq!(value["content"][0]["type"], "text");
        assert_eq!(value["content"][0]["text"], "hello");
        assert_eq!(value["content"][1]["type"], "json");
        assert_eq!(value["content"][1]["value"]["ok"], true);
    }

    #[test]
    fn tool_result_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["content", "is_error", "kind", "name"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("tool_result_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "tool_result_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("tool_result"));
            assert!(value["name"].is_string(), "name must be a string: {value}");
            assert!(
                value["content"].is_array(),
                "content must be an array: {value}",
            );
            assert!(
                value["is_error"].is_boolean(),
                "is_error must be a JSON boolean, not 0/1 or string: {value}",
            );
        }

        let content = vec![
            covenant_mcp::Content::Text {
                text: "hello".into(),
            },
            covenant_mcp::Content::Json {
                value: serde_json::json!({ "ok": true }),
            },
        ];

        assert_shape(&tool_result_json("echo", &content, true));
        assert_shape(&tool_result_json("echo", &[], false));
    }

    #[test]
    fn receipt_batch_list_json_renders_stable_shape() {
        let batch = ReceiptBatchSummary {
            batch_id: "batch-1".into(),
            merkle_root: "ab".repeat(32),
            receipt_count: 2,
            tx_sig: None,
            slot: None,
        };
        let value = receipt_batch_list_json(10, &[batch]);
        assert_eq!(value["kind"], "receipt_batch_list");
        assert_eq!(value["limit"], 10);
        assert_eq!(value["batches"][0]["batch_id"], "batch-1");
        assert_eq!(value["batches"][0]["receipt_count"], 2);
        assert!(value["batches"][0]["tx_sig"].is_null());
        assert!(value["batches"][0]["slot"].is_null());
    }

    #[test]
    fn receipt_batch_list_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["batches", "kind", "limit"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("receipt_batch_list_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "receipt_batch_list_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("receipt_batch_list"));
            assert!(
                value["limit"].is_u64(),
                "limit must serialize as a non-negative integer, not a string: {value}",
            );
            assert!(
                value["batches"].is_array(),
                "batches must be an array: {value}",
            );
        }

        let batch = ReceiptBatchSummary {
            batch_id: "batch-1".into(),
            merkle_root: "ab".repeat(32),
            receipt_count: 2,
            tx_sig: None,
            slot: None,
        };

        assert_shape(&receipt_batch_list_json(10, &[batch]));
        assert_shape(&receipt_batch_list_json(10, &[]));
    }

    #[test]
    fn chain_status_json_renders_stable_shape() {
        let status = ChainStatus {
            chain: "solana".into(),
            cluster: "localnet".into(),
            rpc_url: Some("http://127.0.0.1:8899".into()),
            ws_url: None,
            program_id: None,
            covnt_mint: Some("mint".into()),
            ready: false,
            missing: vec!["program_id".into()],
        };
        let value = chain_status_json(&status);
        assert_eq!(value["kind"], "chain_status");
        assert_eq!(value["status"]["chain"], "solana");
        assert_eq!(value["status"]["cluster"], "localnet");
        assert_eq!(value["status"]["rpc_url"], "http://127.0.0.1:8899");
        assert!(value["status"]["ws_url"].is_null());
        assert_eq!(value["status"]["ready"], false);
        assert_eq!(value["status"]["missing"][0], "program_id");
    }

    #[test]
    fn chain_status_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["kind", "status"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("chain_status_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "chain_status_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("chain_status"));
            assert!(
                value["status"].is_object(),
                "status must be a structured object, not a string blob: {value}",
            );
        }

        let ready = ChainStatus {
            chain: "solana".into(),
            cluster: "mainnet".into(),
            rpc_url: Some("https://api.mainnet-beta.solana.com".into()),
            ws_url: Some("wss://api.mainnet-beta.solana.com".into()),
            program_id: Some("11111111111111111111111111111111".into()),
            covnt_mint: Some("mint".into()),
            ready: true,
            missing: vec![],
        };
        let not_ready = ChainStatus {
            chain: "solana".into(),
            cluster: "localnet".into(),
            rpc_url: Some("http://127.0.0.1:8899".into()),
            ws_url: None,
            program_id: None,
            covnt_mint: Some("mint".into()),
            ready: false,
            missing: vec!["program_id".into()],
        };

        assert_shape(&chain_status_json(&ready));
        assert_shape(&chain_status_json(&not_ready));
    }

    #[test]
    fn verify_report_json_renders_stable_shape() {
        let checks = vec![VerifyCheck {
            name: "memory audit".into(),
            passed: false,
            message: "1 orphan".into(),
        }];
        let drift = vec![VerifyDrift {
            kind: "memory_without_audit".into(),
            id: Some("record-1".into()),
            message: "memory record has no matching audit row".into(),
            repair: "inspect before deleting".into(),
        }];
        let value = verify_report_json(100, &checks, &drift, 1);
        assert_eq!(value["kind"], "verify_report");
        assert_eq!(value["window"], 100);
        assert_eq!(value["orphans_total"], 1);
        assert_eq!(value["checks"][0]["name"], "memory audit");
        assert_eq!(value["checks"][0]["passed"], false);
        assert_eq!(value["drift"][0]["kind"], "memory_without_audit");
        assert_eq!(value["drift"][0]["id"], "record-1");
    }

    #[test]
    fn verify_report_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["checks", "drift", "kind", "orphans_total", "window"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("verify_report_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "verify_report_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("verify_report"));
            assert!(
                value["window"].is_u64(),
                "window must serialize as a non-negative integer, not a string: {value}",
            );
            assert!(
                value["orphans_total"].is_u64(),
                "orphans_total must serialize as a non-negative integer, not a string-of-integer: {value}",
            );
            assert!(
                value["checks"].is_array(),
                "checks must be an array: {value}",
            );
            assert!(
                value["drift"].is_array(),
                "drift must be an array: {value}",
            );
        }

        let checks = vec![VerifyCheck {
            name: "memory audit".into(),
            passed: false,
            message: "1 orphan".into(),
        }];
        let drift = vec![VerifyDrift {
            kind: "memory_without_audit".into(),
            id: Some("record-1".into()),
            message: "memory record has no matching audit row".into(),
            repair: "inspect before deleting".into(),
        }];

        assert_shape(&verify_report_json(100, &checks, &drift, 1));
        assert_shape(&verify_report_json(100, &[], &[], 0));
    }

    #[test]
    fn flush_receipts_json_renders_stable_shape() {
        let batch = ReceiptBatchSummary {
            batch_id: "batch-1".into(),
            merkle_root: "ab".repeat(32),
            receipt_count: 2,
            tx_sig: None,
            slot: None,
        };
        let value = flush_receipts_json(10, &batch, 7);
        assert_eq!(value["kind"], "receipt_batch_flushed");
        assert_eq!(value["limit"], 10);
        assert_eq!(value["receipts_updated"], 7);
        assert_eq!(value["batch"]["batch_id"], "batch-1");
        assert_eq!(value["batch"]["receipt_count"], 2);
        assert!(value["batch"]["tx_sig"].is_null());
        assert!(value["batch"]["slot"].is_null());
    }

    #[test]
    fn flush_receipts_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &["batch", "kind", "limit", "receipts_updated"];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("flush_receipts_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "flush_receipts_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("receipt_batch_flushed"));
            assert!(
                value["limit"].is_u64(),
                "limit must serialize as a non-negative integer, not a string: {value}",
            );
            assert!(
                value["receipts_updated"].is_u64(),
                "receipts_updated must serialize as a non-negative integer, not a string-of-integer: {value}",
            );
            assert!(
                value["batch"].is_object(),
                "batch must be a structured object, not a string blob: {value}",
            );
        }

        let unconfirmed = ReceiptBatchSummary {
            batch_id: "batch-1".into(),
            merkle_root: "ab".repeat(32),
            receipt_count: 2,
            tx_sig: None,
            slot: None,
        };
        let confirmed = ReceiptBatchSummary {
            batch_id: "batch-2".into(),
            merkle_root: "cd".repeat(32),
            receipt_count: 5,
            tx_sig: Some("sigb58".into()),
            slot: Some(123_456),
        };

        assert_shape(&flush_receipts_json(10, &unconfirmed, 7));
        assert_shape(&flush_receipts_json(10, &confirmed, 5));
    }

    #[test]
    fn a2a_status_json_renders_stable_shape() {
        let sender = AgentId::new("sender@local", [1u8; 32]);
        let recipient = AgentId::new("recipient@local", [2u8; 32]);
        let task_id = uuid::Uuid::nil();
        let task = A2ATask {
            id: task_id,
            sender,
            recipient,
            intent_text: "status probe".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        let entry = A2ATaskQueueEntry {
            state: A2ATaskQueueState::Queued,
            task,
            lease_id: None,
            leased_to: None,
            leased_at_ms: None,
            attempt: 0,
        };
        let result = A2ATaskResult::ok(task_id, vec![]);
        let value = a2a_status_json(
            5,
            Some(300_000),
            Some(60_000),
            Some(A2ATaskQueueState::InFlight),
            &[entry],
            &[result],
        );
        assert_eq!(value["kind"], "a2a_status");
        assert_eq!(value["limit"], 5);
        assert_eq!(value["min_lease_age_ms"], 300_000);
        assert_eq!(value["deadline_within_ms"], 60_000);
        assert_eq!(value["state_filter"], "in_flight");
        assert_eq!(value["tasks"][0]["state"], "queued");
        assert_eq!(value["tasks"][0]["task"]["intent_text"], "status probe");
        assert_eq!(value["results"][0]["status"], "ok");
    }

    #[test]
    fn a2a_status_json_omits_deadline_filter_when_inactive() {
        let value = a2a_status_json(5, None, None, None, &[], &[]);
        assert_eq!(value["kind"], "a2a_status");
        assert!(value["min_lease_age_ms"].is_null());
        assert!(value["deadline_within_ms"].is_null());
        assert!(value["state_filter"].is_null());
    }

    #[test]
    fn a2a_status_json_pins_top_level_schema() {
        const EXPECTED_KEYS: &[&str] = &[
            "deadline_within_ms",
            "kind",
            "limit",
            "min_lease_age_ms",
            "results",
            "state_filter",
            "tasks",
        ];

        fn assert_shape(value: &serde_json::Value) {
            let object = value
                .as_object()
                .expect("a2a_status_json must return an object");
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let expected: Vec<String> = EXPECTED_KEYS.iter().map(|k| (*k).to_string()).collect();
            assert_eq!(
                keys, expected,
                "a2a_status_json top-level keys must match the documented schema exactly; an extra or missing key is a forcing function to update docs/ipc-and-http-gateway.md",
            );

            assert!(value["kind"].is_string(), "kind must be a string: {value}");
            assert_eq!(value["kind"].as_str(), Some("a2a_status"));
            assert!(
                value["limit"].is_u64(),
                "limit must serialize as a non-negative integer, not a string: {value}",
            );
            assert!(
                value["min_lease_age_ms"].is_u64() || value["min_lease_age_ms"].is_null(),
                "min_lease_age_ms must be u64-or-null (never a string-of-integer): {value}",
            );
            assert!(
                value["deadline_within_ms"].is_u64() || value["deadline_within_ms"].is_null(),
                "deadline_within_ms must be u64-or-null (never a string-of-integer): {value}",
            );
            assert!(
                value["state_filter"].is_string() || value["state_filter"].is_null(),
                "state_filter must be string-or-null (never integer / array): {value}",
            );
            assert!(
                value["tasks"].is_array(),
                "tasks must be an array: {value}",
            );
            assert!(
                value["results"].is_array(),
                "results must be an array: {value}",
            );
        }

        let sender = AgentId::new("sender@local", [1u8; 32]);
        let recipient = AgentId::new("recipient@local", [2u8; 32]);
        let task_id = uuid::Uuid::nil();
        let task = A2ATask {
            id: task_id,
            sender,
            recipient,
            intent_text: "status probe".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        let entry = A2ATaskQueueEntry {
            state: A2ATaskQueueState::Queued,
            task,
            lease_id: None,
            leased_to: None,
            leased_at_ms: None,
            attempt: 0,
        };
        let result = A2ATaskResult::ok(task_id, vec![]);

        assert_shape(&a2a_status_json(
            5,
            Some(300_000),
            Some(60_000),
            Some(A2ATaskQueueState::InFlight),
            &[entry.clone()],
            &[result.clone()],
        ));
        assert_shape(&a2a_status_json(
            5,
            Some(300_000),
            Some(60_000),
            Some(A2ATaskQueueState::Queued),
            &[],
            &[],
        ));
        assert_shape(&a2a_status_json(5, None, None, None, &[entry], &[result]));
        assert_shape(&a2a_status_json(5, None, None, None, &[], &[]));
    }

    #[test]
    fn parse_a2a_queue_state_accepts_both_spellings() {
        assert_eq!(
            parse_a2a_queue_state("queued").unwrap(),
            A2ATaskQueueState::Queued,
        );
        assert_eq!(
            parse_a2a_queue_state("in_flight").unwrap(),
            A2ATaskQueueState::InFlight,
        );
        assert_eq!(
            parse_a2a_queue_state("in-flight").unwrap(),
            A2ATaskQueueState::InFlight,
        );
        let err = parse_a2a_queue_state("InFlight").unwrap_err();
        assert!(
            err.to_string().contains("unknown a2a state"),
            "case-sensitive parser must reject mixed case: {err:?}",
        );
        let err = parse_a2a_queue_state("queued ").unwrap_err();
        assert!(
            err.to_string().contains("unknown a2a state"),
            "trailing whitespace must be rejected so a typo does not silently disable the filter: {err:?}",
        );
    }

    #[test]
    fn peer_revoke_json_exit_classification_matches_human_cli() {
        let p = make_peer(7, "alice@host", false);
        assert!(!peer_revoke_is_failure(&RevokeOutcome::Revoked(p.clone())));
        assert!(!peer_revoke_is_failure(&RevokeOutcome::AlreadyRevoked(
            p.clone()
        )));
        assert!(peer_revoke_is_failure(&RevokeOutcome::NotFound));
        assert!(peer_revoke_is_failure(&RevokeOutcome::Ambiguous {
            matches: vec![p.clone()],
            truncated: false,
        }));
        assert!(peer_revoke_is_failure(&RevokeOutcome::SelfRevokeForbidden(
            p
        )));
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
