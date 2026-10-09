//! `covenant-compute-mcp` — the compute network's thinnest demand path:
//! a stdio MCP server exposing the buyer tool surface (`compute.infer`
//! and `compute.run` to buy work, the `compute.stream_start`/
//! `compute.stream_poll` pair to buy inference and read it as it
//! generates, plus receipts/deposit/balance/dispute/verify). Any MCP
//! client adds it, calls a tool, and gets back the job output plus a
//! metadata block for the operator's signed, locally re-verified
//! receipt. No covenantd required.
//!
//! Spend control without a budget subsystem: a per-call price ceiling
//! (`COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC`, default $1), an
//! optional cumulative session cap
//! (`COVENANT_COMPUTE_MAX_TOTAL_MICRO_USDC`) that counts in-flight
//! streaming offers, and a cap on concurrently running streaming jobs
//! (`COVENANT_COMPUTE_MAX_ACTIVE_STREAMS`, default 4). All refuse
//! before any dispatch. `COVENANT_COMPUTE_COORDINATOR_URL` is required — it is
//! the trust anchor. `COVENANT_COMPUTE_RPC_URL` (optional) is this
//! buyer's own Solana RPC endpoint for `compute.verify`'s on-chain
//! read-back — never taken from the coordinator, whose pick could
//! vouch for its own transfers. The buyer identity persists under
//! `$COVENANT_COMPUTE_MCP_HOME` (default `$HOME/.covenant-compute-mcp`).
//!
//! Protocol: newline-delimited JSON-RPC 2.0 over stdin/stdout
//! (`initialize`, `notifications/initialized`, `ping`, `tools/list`,
//! `tools/call`), protocol revision 2024-11-05. All content blocks are
//! emitted as `text` for maximum client interop; the receipt metadata
//! block is a JSON document in a text block. Logs go to stderr —
//! stdout belongs to the protocol.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use covenant_compute_buyer::{
    agent_tool_spec, apply_patch, balance_tool_spec, cancel_job, cancel_tool_spec, capacity,
    capacity_tool_spec, cheapest_matching_ask, claim_deposit, deposit_tool_spec, describe_reworks,
    describe_round, describe_verdict, dispatch_and_verify, dispatch_signed, dispute_job,
    dispute_tool_spec, embed_tool_spec, fetch_job_output, funds_with_deposit_info, hire_agent,
    infer_tool_spec, list_verified_jobs, list_withdrawals, output_tool_spec, prepare_agent_task,
    preview_value, quote_price, receipts_tool_spec, run_tool_spec, save_speech_clip, sign_envelope,
    speak_tool_spec, stream_and_verify, stream_poll_tool_spec, stream_start_tool_spec,
    submit_streaming, transcribe_tool_spec, verify_payout, verify_tool_spec, withdraw,
    withdraw_tool_spec, withdrawals_tool_spec, AgentArgs, AgentOutcome, BuyerConfig, BuyerError,
    CancelArgs, DisputeArgs, EmbedArgs, InferArgs, JobOutputView, JobRequest, OutputArgs,
    PurchaseBook, PurchaseEntry, RunArgs, SpeakArgs, SpendCaps, StreamJobs, StreamPollArgs,
    TranscribeArgs, VerifyArgs, WithdrawArgs, AGENT_TOOL, BALANCE_TOOL, CANCEL_TOOL, CAPACITY_TOOL,
    DEFAULT_AGENT_DEADLINE_MS, DEFAULT_AGENT_OFFER_MICRO_USDC, DEPOSIT_TOOL, DISPUTE_TOOL,
    EMBED_TOOL, INFER_TOOL, OUTPUT_TOOL, RECEIPTS_TOOL, RUN_TOOL, SPEAK_TOOL, STREAM_POLL_TOOL,
    STREAM_START_TOOL, TRANSCRIBE_TOOL, VERIFY_TOOL, WITHDRAWALS_TOOL, WITHDRAW_TOOL,
};
use covenant_compute_protocol::{parse_speech_output, JobKind};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing_subscriber::EnvFilter;

const PROTOCOL_VERSION: &str = "2024-11-05";

/// The stream ledger's owner key. One stdio session is one principal —
/// the buyer identity itself — so every start and poll shares it; the
/// ledger still enforces the active cap and holds concluded feeds.
const STREAM_OWNER: &str = "stdio";

#[derive(Debug, Deserialize)]
struct Incoming {
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

struct ServerState {
    http: reqwest::Client,
    buyer: BuyerConfig,
    identity: LocalIdentity,
    default_deadline_ms: u64,
    /// The per-call ceiling and cumulative session cap this server buys
    /// under, plus its running settled spend.
    caps: SpendCaps,
    /// In-flight and recently concluded `compute.stream_start` jobs;
    /// their offers count against the session cap while they run.
    streams: StreamJobs<Vec<Value>>,
    /// Idempotent purchases, durable in the server home — a call
    /// retried after a crash re-drives the journaled envelope instead
    /// of buying a second job.
    purchases: PurchaseBook,
    /// Where `compute.speak` writes synthesized clips. The audio never
    /// rides back in the tool result — base64 does not belong in an
    /// agent's context — so the clip lands here and the result names the
    /// file. Created lazily on the first speak.
    clips_dir: PathBuf,
}

fn response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: String) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tool_error(id: Value, message: String) -> Value {
    response(
        id,
        json!({ "content": [{ "type": "text", "text": message }], "isError": true }),
    )
}

/// A dispatch failure as a tool error. When the coordinator refused for
/// want of funds — the first thing a new buyer hits before it has
/// deposited — the message names the deposit path, in the server's own
/// tools since those are all an MCP client can call. Every other failure
/// reads as before.
fn dispatch_failed(id: Value, e: BuyerError) -> Value {
    if e.is_underfunded() {
        return tool_error(
            id,
            format!(
                "{e} — top up first: call compute.balance for this deployment's deposit \
                 instructions, then compute.deposit"
            ),
        );
    }
    tool_error(id, format!("compute dispatch failed: {e}"))
}

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Flatten output blocks to `text`-only for strict-client interop.
/// How a paid job's output is returned to the caller. Most tools hand the
/// output blocks back inline; `compute.speak` writes its clip to disk and
/// names the file instead, so multi-megabyte base64 audio never lands in an
/// agent's context.
#[derive(Clone, Copy)]
enum OutputRender<'a> {
    Inline,
    SpeechClip { dir: &'a Path },
}

fn text_blocks(output: &[Content]) -> Vec<Value> {
    output
        .iter()
        .map(|c| match c {
            Content::Text { text } => json!({ "type": "text", "text": text }),
            Content::Json { value } => json!({ "type": "text", "text": value.to_string() }),
        })
        .collect()
}

/// The content block for a synthesized clip: the saved file's path and
/// shape, never the base64 audio. Writes the clip under `dir`, named for
/// the job. A base64 or write failure surfaces as a block the caller can
/// read rather than a silent drop — the receipt block still follows, so a
/// buyer always learns they paid. Falls back to the inline blocks if the
/// output is not speech-shaped, which would be a server bug worth seeing.
fn speech_blocks(dir: &Path, output: &[Content], job_id: uuid::Uuid) -> Vec<Value> {
    let Ok(speech) = parse_speech_output(output) else {
        return text_blocks(output);
    };
    if let Err(e) = std::fs::create_dir_all(dir) {
        return vec![json!({ "type": "text", "text": json!({
            "saved": false,
            "error": format!("create {}: {e}", dir.display()),
            "format": speech.format,
            "model": speech.model,
        }).to_string() })];
    }
    let described = match save_speech_clip(dir, &speech, job_id) {
        Ok((path, bytes)) => json!({
            "saved": true,
            "path": path.display().to_string(),
            "bytes": bytes,
            "format": speech.format,
            "sample_rate_hz": speech.sample_rate_hz,
            "model": speech.model,
        }),
        Err(e) => json!({
            "saved": false,
            "error": e,
            "format": speech.format,
            "model": speech.model,
        }),
    };
    vec![json!({ "type": "text", "text": described.to_string() })]
}

async fn call_infer(state: &ServerState, id: Value, arguments: Value) -> Value {
    let args: InferArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(e) => return error(id, -32602, format!("invalid arguments: {e}")),
    };
    let input = match args.input() {
        Ok(input) => input,
        Err(e) => return error(id, -32602, format!("invalid arguments: {e}")),
    };
    dispatch_with_caps(
        state,
        id,
        JobKind::InferenceCall,
        input,
        args.model,
        args.gpu_class,
        args.min_vram_gb,
        args.min_reputation_bps,
        args.price_micro_usdc,
        args.deadline_ms,
        args.idempotency_key,
        args.dry_run,
        OutputRender::Inline,
    )
    .await
}

/// `compute.run`: one command bought as a batch job, behind the same
/// per-call and session spend caps as `compute.infer`.
/// Most of a patch an agent's context should carry inline; a larger one is
/// named by its file only.
const INLINE_PATCH_BYTES: usize = 60 * 1024;

/// Hires a coding agent and answers with the checked result. A patch that
/// passed rides back as a diff the calling agent can read and apply (and is
/// saved beside the server's other outputs); one that did not comes back as
/// the check's verdict, with nothing charged. The same price ceiling and
/// session cap as every other purchase apply.
async fn call_agent(state: &ServerState, id: Value, arguments: Value) -> Value {
    let args: AgentArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(e) => return error(id, -32602, format!("invalid arguments: {e}")),
    };
    let task = match prepare_agent_task(&args) {
        Ok(task) => task,
        Err(e) => return tool_error(id, e.to_string()),
    };
    let cap = state.caps.max_price_micro_usdc();
    // An offer is a ceiling: a passing task is charged what its build spent
    // plus its checks, so the default is room for a build, capped by the
    // session's own per-call limit.
    let price = args
        .price_micro_usdc
        .unwrap_or(DEFAULT_AGENT_OFFER_MICRO_USDC.min(cap));
    if let Some(refusal) = state.caps.per_call_refusal(price) {
        return tool_error(id, refusal);
    }
    if let Some(refusal) = session_cap_refusal(state, price) {
        return tool_error(id, refusal);
    }
    let deadline_ms = args.deadline_ms.unwrap_or(DEFAULT_AGENT_DEADLINE_MS);
    let outcome = match hire_agent(
        &state.http,
        &state.buyer,
        &state.identity,
        task,
        price,
        deadline_ms,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(e) => return dispatch_failed(id, e),
    };
    let text = match outcome {
        AgentOutcome::NotPaid {
            job_id,
            status,
            reason,
            verdict,
            round,
            reworks,
        } => {
            let mut text = format!(
                "Not paid: job {job_id} ended {status} ({}). Nothing was charged.",
                reason.as_deref().unwrap_or("no reason given")
            );
            if let Some(verdict) = verdict {
                text.push_str("\n\n");
                text.push_str(&describe_verdict(&verdict));
            }
            if let Some(line) = describe_reworks(reworks, false) {
                text.push('\n');
                text.push_str(&line);
            }
            if let Some(round) = round {
                text.push('\n');
                text.push_str(&describe_round(&round));
            }
            text
        }
        AgentOutcome::Accepted {
            outcome,
            built,
            patch,
            verdict,
            round,
            charged_micro_usdc,
            reworks,
        } => {
            let charged = charged_micro_usdc.unwrap_or(outcome.envelope.payload.price_micro_usdc);
            state.caps.record_spend(charged);
            let job_id = outcome.receipt.receipt.job_id;
            let dir = state.clips_dir.with_file_name("agent-patches");
            let path = dir.join(format!("{job_id}.patch"));
            let saved = std::fs::create_dir_all(&dir)
                .and_then(|()| std::fs::write(&path, &patch))
                .is_ok();
            let mut text = format!(
                "Accepted: job {job_id} passed another operator's check. Charged {charged} \
                 micro-USDC of a {} ceiling. {} file(s) changed.",
                outcome.envelope.payload.price_micro_usdc, built.files_changed
            );
            if args.apply && !args.repo.starts_with("https://") {
                match apply_patch(std::path::Path::new(&args.repo), &patch) {
                    Ok(()) => text.push_str(" The patch is applied to the working tree."),
                    Err(e) => text.push_str(&format!(" The patch was not applied: {e}.")),
                }
            }
            if saved {
                text.push_str(&format!(" Saved at {}.", path.display()));
            }
            if !built.summary.is_empty() {
                text.push_str("\n\nThe agent's summary:\n");
                text.push_str(&built.summary);
            }
            if let Some(verdict) = verdict {
                text.push_str("\n\n");
                text.push_str(&describe_verdict(&verdict));
            }
            if let Some(line) = describe_reworks(reworks, true) {
                text.push('\n');
                text.push_str(&line);
            }
            if let Some(round) = round {
                text.push('\n');
                text.push_str(&describe_round(&round));
            }
            if patch.len() <= INLINE_PATCH_BYTES {
                text.push_str("\n\n```diff\n");
                text.push_str(&String::from_utf8_lossy(&patch));
                text.push_str("\n```");
            }
            text
        }
    };
    response(
        id,
        json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
    )
}

async fn call_run(state: &ServerState, id: Value, arguments: Value) -> Value {
    let args: RunArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(e) => return error(id, -32602, format!("invalid arguments: {e}")),
    };
    let input = match args.input() {
        Ok(input) => input,
        Err(e) => return error(id, -32602, format!("invalid arguments: {e}")),
    };
    dispatch_with_caps(
        state,
        id,
        JobKind::BatchJob,
        input,
        None,
        args.gpu_class,
        args.min_vram_gb,
        args.min_reputation_bps,
        args.price_micro_usdc,
        args.deadline_ms,
        args.idempotency_key,
        args.dry_run,
        OutputRender::Inline,
    )
    .await
}

async fn call_embed(state: &ServerState, id: Value, arguments: Value) -> Value {
    let args: EmbedArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(e) => return error(id, -32602, format!("invalid arguments: {e}")),
    };
    let input = match args.input() {
        Ok(input) => input,
        Err(e) => return error(id, -32602, format!("invalid arguments: {e}")),
    };
    dispatch_with_caps(
        state,
        id,
        JobKind::Embedding,
        input,
        args.model,
        args.gpu_class,
        args.min_vram_gb,
        args.min_reputation_bps,
        args.price_micro_usdc,
        args.deadline_ms,
        args.idempotency_key,
        args.dry_run,
        OutputRender::Inline,
    )
    .await
}

/// `compute.transcribe`: one audio clip turned to text, behind the same
/// per-call and session spend caps as `compute.infer`.
async fn call_transcribe(state: &ServerState, id: Value, arguments: Value) -> Value {
    let args: TranscribeArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(e) => return error(id, -32602, format!("invalid arguments: {e}")),
    };
    let input = match args.input() {
        Ok(input) => input,
        Err(e) => return error(id, -32602, format!("invalid arguments: {e}")),
    };
    dispatch_with_caps(
        state,
        id,
        JobKind::Transcription,
        input,
        args.model,
        args.gpu_class,
        args.min_vram_gb,
        args.min_reputation_bps,
        args.price_micro_usdc,
        args.deadline_ms,
        args.idempotency_key,
        args.dry_run,
        OutputRender::Inline,
    )
    .await
}

/// `compute.speak`: text synthesized to an audio clip on a speech operator,
/// on the exact spend path `compute.infer` uses. The clip is written under
/// the server's clips directory and the result names the file; the base64
/// audio never rides back into the agent's context.
async fn call_speak(state: &ServerState, id: Value, arguments: Value) -> Value {
    let args: SpeakArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(e) => return error(id, -32602, format!("invalid arguments: {e}")),
    };
    let input = match args.input() {
        Ok(input) => input,
        Err(e) => return error(id, -32602, format!("invalid arguments: {e}")),
    };
    dispatch_with_caps(
        state,
        id,
        JobKind::SpeechSynthesis,
        input,
        args.model,
        args.gpu_class,
        args.min_vram_gb,
        args.min_reputation_bps,
        args.price_micro_usdc,
        args.deadline_ms,
        args.idempotency_key,
        args.dry_run,
        OutputRender::SpeechClip {
            dir: &state.clips_dir,
        },
    )
    .await
}

/// The session spend cap, counting settled spend AND in-flight
/// streaming offers — a streaming job debits the session only when its
/// drain task completes, so its offer must count here or the two paths
/// together could overcommit the cap.
fn session_cap_refusal(state: &ServerState, price: u64) -> Option<String> {
    // Read in-flight streaming offers before delegating, so settled spend
    // is loaded last (inside `session_refusal`). A drain adds to `spent`
    // and only then drops the job from `active`, so an in-flight offer is
    // always in at least one of the two snapshots; loading `spent` first
    // could miss a job that settled in the gap and admit a buy past the cap.
    let streaming = state.streams.active_committed(STREAM_OWNER);
    state.caps.session_refusal(price, streaming)
}

/// The one spend path both buying tools ride: price ceiling, session
/// cap, dispatch, local re-verification, then the output plus the
/// receipt-metadata block. An idempotency key makes the purchase
/// exactly-once: the signed envelope journals in the server home
/// before its first submission, so a repeat of the key — this session
/// or after a restart — re-drives the same job and only ever counts
/// the spend once.
#[allow(clippy::too_many_arguments)]
async fn dispatch_with_caps(
    state: &ServerState,
    id: Value,
    kind: JobKind,
    input: Vec<Content>,
    model: Option<String>,
    gpu_class: Option<String>,
    min_vram_gb: Option<u32>,
    min_reputation_bps: Option<u32>,
    price_arg: Option<u64>,
    deadline_arg: Option<u64>,
    idempotency_key: Option<String>,
    dry_run: bool,
    render: OutputRender<'_>,
) -> Value {
    let cap = state.caps.max_price_micro_usdc();
    // A preview resolves the price and routing a real buy would use and
    // returns them without dispatching or spending. It reserves no
    // purchase, so an idempotency key has nothing to bind; and it refuses
    // an unservable ask up front rather than dispatch-then-refund, so what
    // it quotes is always a price a buy would truly pay.
    if dry_run {
        if idempotency_key.is_some() {
            return tool_error(
                id,
                "a dry run reserves no purchase, so it takes no idempotency_key — drop the key to \
                 preview, or drop dry_run to buy"
                    .into(),
            );
        }
        let quote = match quote_price(
            &state.http,
            &state.buyer,
            kind,
            model.as_deref(),
            gpu_class.as_deref(),
            min_vram_gb,
            min_reputation_bps,
            price_arg,
            cap,
        )
        .await
        {
            Ok(quote) => quote,
            Err(e) => return tool_error(id, e.to_string()),
        };
        let deadline_ms = deadline_arg.unwrap_or(state.default_deadline_ms);
        let preview = preview_value(
            kind,
            model.as_deref(),
            gpu_class.as_deref(),
            min_vram_gb,
            min_reputation_bps,
            quote,
            deadline_ms,
            cap,
            input.len(),
        );
        return response(
            id,
            json!({ "content": [{ "type": "text", "text": preview.to_string() }], "isError": false }),
        );
    }
    let price = match price_arg {
        Some(p) => p,
        // No price named: offer the cheapest matching ask, not the
        // ceiling — settlement charges the envelope's price, so
        // defaulting to the cap overpays an operator that asked less. A
        // market read that fails or finds nothing falls back to the
        // ceiling, so this never refuses a call the old default allowed.
        None => cheapest_matching_ask(
            &state.http,
            &state.buyer,
            kind,
            model.as_deref(),
            gpu_class.as_deref(),
            min_vram_gb,
            min_reputation_bps,
        )
        .await
        .ok()
        .flatten()
        .map(|floor| floor.min(cap))
        .unwrap_or(cap),
    };
    if let Some(refusal) = state.caps.per_call_refusal(price) {
        return tool_error(id, refusal);
    }

    // A keyed purchase resolves against the book first: a settled key
    // replays the recorded job (no caps, no spend — that happened
    // once already); an in-flight key re-drives its journaled
    // envelope; only a fresh key signs anything new.
    if let Some(key) = idempotency_key {
        if key.is_empty() || key.len() > 128 {
            return error(id, -32602, "idempotency_key must be 1..=128 bytes".into());
        }
        let scoped = format!("{}:{key}", state.identity.agent_id().pubkey_base58());
        let looked_up = state.purchases.lookup(&scoped);
        if let Some(entry) = &looked_up {
            // The key names one purchase. Explicit arguments must
            // agree with it; refusing here keeps a reused key from
            // silently answering a new question with the old job.
            if let Some(argument) = entry.conflicting_argument(
                kind,
                &input,
                model.as_deref(),
                gpu_class.as_deref(),
                min_vram_gb,
                min_reputation_bps,
                price_arg,
                deadline_arg,
            ) {
                return tool_error(
                    id,
                    format!(
                        "idempotency key {key:?} was already used with a different {argument}: a \
                         key names one purchase, so repeat its original arguments to retrieve it \
                         or use a fresh key for new work"
                    ),
                );
            }
        }
        let envelope = match looked_up {
            Some(entry) if entry.settled() => {
                return match dispatch_signed(
                    &state.http,
                    &state.buyer,
                    &state.identity,
                    entry.envelope,
                )
                .await
                {
                    Ok(outcome) => paid_response(id, &outcome, render),
                    Err(e) => dispatch_failed(id, e),
                };
            }
            Some(entry) => entry.envelope,
            None => {
                let envelope = match sign_envelope(
                    &state.buyer,
                    &state.identity,
                    JobRequest {
                        kind,
                        input,
                        model,
                        gpu_class,
                        min_vram_gb,
                        min_reputation_bps,
                        price_micro_usdc: price,
                        deadline_ms: deadline_arg.unwrap_or(state.default_deadline_ms),
                    },
                ) {
                    Ok(envelope) => envelope,
                    Err(e) => return tool_error(id, format!("compute dispatch failed: {e}")),
                };
                if let Err(e) = state.purchases.record(PurchaseEntry {
                    key: scoped.clone(),
                    envelope: envelope.clone(),
                    opened_at_ms: epoch_ms(),
                    receipt_id: None,
                    voided: false,
                }) {
                    return tool_error(id, format!("purchase book: {e}"));
                }
                envelope
            }
        };
        if let Some(refusal) = session_cap_refusal(state, price) {
            return tool_error(id, refusal);
        }
        return match dispatch_signed(&state.http, &state.buyer, &state.identity, envelope).await {
            Ok(outcome) => count_spend_and_respond(state, id, &outcome, Some(&scoped), render),
            // Refunded, rejected, failed — or refused at submission
            // with a verdict on the envelope itself: the money never
            // moved, the key frees so a retry may honestly re-buy.
            Err(e) if e.concludes_purchase_unpaid() => {
                if let Err(ve) = state.purchases.void(&scoped) {
                    tracing::warn!(key = %scoped, error = %ve, "purchase book void failed");
                }
                tool_error(id, e.to_string())
            }
            // Ambiguous — a timeout or transport failure. The entry
            // stays in flight; the next retry resolves it.
            Err(e @ BuyerError::ReceiptTimeout(_)) => tool_error(id, e.to_string()),
            Err(e) => dispatch_failed(id, e),
        };
    }

    if let Some(refusal) = session_cap_refusal(state, price) {
        return tool_error(id, refusal);
    }
    let deadline_ms = deadline_arg.unwrap_or(state.default_deadline_ms);

    match dispatch_and_verify(
        &state.http,
        &state.buyer,
        &state.identity,
        JobRequest {
            kind,
            input,
            model,
            gpu_class,
            min_vram_gb,
            min_reputation_bps,
            price_micro_usdc: price,
            deadline_ms,
        },
    )
    .await
    {
        Ok(outcome) => count_spend_and_respond(state, id, &outcome, None, render),
        // A refusal the calling agent should read and adapt to, not a
        // protocol failure: surface as a tool-level error result.
        Err(e @ (BuyerError::NotServed { .. } | BuyerError::ReceiptTimeout(_))) => {
            tool_error(id, e.to_string())
        }
        Err(e) => dispatch_failed(id, e),
    }
}

/// Counts the session spend, settles the purchase book when a key is
/// in play, and shapes the response. Spend is counted before the key
/// settles, so a crash between the two can only over-count a
/// session-scoped cap — never replay a purchase into a second job.
fn count_spend_and_respond(
    state: &ServerState,
    id: Value,
    outcome: &covenant_compute_buyer::DispatchOutcome,
    book_key: Option<&str>,
    render: OutputRender,
) -> Value {
    let receipt = &outcome.receipt.receipt;
    // Count the offered (envelope) price against the session cap: the
    // coordinator charges the held envelope price, not the receipt's
    // claimed price, so counting the receipt price would let a node
    // under-report its way past a buyer's configured session cap.
    state
        .caps
        .record_spend(outcome.envelope.payload.price_micro_usdc);
    if let Some(scoped) = book_key {
        if let Err(e) = state.purchases.settle(scoped, receipt.job_id) {
            tracing::warn!(key = %scoped, error = %e, "purchase book settle failed");
        }
    }
    paid_response(id, outcome, render)
}

/// The output blocks plus the receipt-metadata block every purchase —
/// fresh or replayed — answers with. `render` chooses whether the output
/// rides back inline or, for a synthesized clip, as a saved-file reference.
fn paid_response(
    id: Value,
    outcome: &covenant_compute_buyer::DispatchOutcome,
    render: OutputRender,
) -> Value {
    let receipt = &outcome.receipt.receipt;
    let mut content = match render {
        OutputRender::Inline => text_blocks(&outcome.output),
        OutputRender::SpeechClip { dir } => speech_blocks(dir, &outcome.output, receipt.job_id),
    };
    let metadata = json!({
        "job_id": receipt.job_id,
        "operator_pubkey_b58": receipt.operator.pubkey_base58(),
        // What the buyer was charged: the escrowed envelope price the
        // coordinator releases, not the receipt's claimed price. A node
        // that signs a receipt below its offer is still paid — and the
        // buyer still debited — the full offer, so the receipt figure
        // would under-report the real charge.
        "price_micro_usdc": outcome.envelope.payload.price_micro_usdc,
        "result_hash_hex": receipt.result_hash_hex,
        "wall_ms": receipt.meter.wall_ms,
        "tokens_in": receipt.meter.tokens_in,
        "tokens_out": receipt.meter.tokens_out,
        // Why generation stopped, as the operator signed it: `content_filter`
        // or `length` means the answer above was cut short, not a clean end —
        // an agent must see that rather than read a partial as complete. Null
        // for a backend that reported none. Mirrors the HTTP doors.
        "finish_reason": receipt.meter.finish_reason,
        "receipt_verified": true,
        // On-chain pointer when the payout push had already landed by
        // receipt time; null while it's still in flight (re-check via
        // compute.receipts).
        "payout": outcome.payout,
    });
    content.push(json!({ "type": "text", "text": metadata.to_string() }));
    response(id, json!({ "content": content, "isError": false }))
}

/// `compute.stream_start`: `compute.infer`'s caps and dispatch, but
/// the call returns the job id as soon as the coordinator accepts the
/// envelope; a background task drains the feed into the stream ledger
/// and lands the same output-plus-receipt blocks the synchronous call
/// returns, for `compute.stream_poll` to serve. The session spend is
/// committed when the drain verifies the receipt — never at poll time.
async fn call_stream_start(state: &Arc<ServerState>, id: Value, arguments: Value) -> Value {
    let args: InferArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(e) => return error(id, -32602, format!("invalid arguments: {e}")),
    };
    // Streaming returns a job_id before the drain settles, so it can't
    // dedupe on a key the way compute.infer does. Refuse the key rather
    // than accept it and quietly pay twice on a retry.
    if args.idempotency_key.is_some() {
        return tool_error(
            id,
            format!(
                "{STREAM_START_TOOL} can't honor idempotency_key: it returns a job_id before \
                 payment settles, so it can't guarantee exactly-once. Use {INFER_TOOL} for an \
                 idempotent buy, or omit the key to stream."
            ),
        );
    }
    // A stream is a live feed to open, not a figure to quote — there is
    // nothing to preview. Point a caller wanting the cost at the
    // synchronous tool's dry run.
    if args.dry_run {
        return tool_error(
            id,
            format!(
                "{STREAM_START_TOOL} opens a live feed and has nothing to preview — call \
                 {INFER_TOOL} with dry_run to see the price and routing first, then stream."
            ),
        );
    }
    let input = match args.input() {
        Ok(input) => input,
        Err(e) => return error(id, -32602, format!("invalid arguments: {e}")),
    };
    let cap = state.caps.max_price_micro_usdc();
    let price = match args.price_micro_usdc {
        Some(p) => p,
        None => cheapest_matching_ask(
            &state.http,
            &state.buyer,
            JobKind::InferenceCall,
            args.model.as_deref(),
            args.gpu_class.as_deref(),
            args.min_vram_gb,
            args.min_reputation_bps,
        )
        .await
        .ok()
        .flatten()
        .map(|floor| floor.min(cap))
        .unwrap_or(cap),
    };
    if let Some(refusal) = state.caps.per_call_refusal(price) {
        return tool_error(id, refusal);
    }
    if let Some(refusal) = session_cap_refusal(state, price) {
        return tool_error(id, refusal);
    }
    let deadline_ms = args.deadline_ms.unwrap_or(state.default_deadline_ms);

    let job_id = uuid::Uuid::new_v4();
    if let Err(e) = state.streams.try_start(STREAM_OWNER, job_id, price) {
        return tool_error(id, e.to_string());
    }
    let envelope = match submit_streaming(
        &state.http,
        &state.buyer,
        &state.identity,
        job_id,
        JobRequest {
            kind: JobKind::InferenceCall,
            input,
            model: args.model,
            gpu_class: args.gpu_class,
            min_vram_gb: args.min_vram_gb,
            min_reputation_bps: args.min_reputation_bps,
            price_micro_usdc: price,
            deadline_ms,
        },
    )
    .await
    {
        Ok(envelope) => envelope,
        Err(e) => {
            state.streams.remove(job_id);
            return dispatch_failed(id, e);
        }
    };

    let task_state = state.clone();
    tokio::spawn(async move {
        let work = tokio::spawn({
            let state = task_state.clone();
            async move { drain_stream(&state, envelope).await }
        });
        let outcome = match work.await {
            Ok(outcome) => outcome,
            // A panic in the drain must still free the session's slot
            // and tell the poller something true.
            Err(e) => Err(format!("stream drain task died: {e}")),
        };
        task_state.streams.conclude(job_id, outcome);
    });

    let started = json!({
        "job_id": job_id,
        "status": "streaming",
        "next_seq": 0,
        "price_micro_usdc": price,
        "poll_tool": STREAM_POLL_TOOL,
    });
    response(
        id,
        json!({
            "content": [{ "type": "text", "text": started.to_string() }],
            "isError": false,
        }),
    )
}

/// The drain half of one streaming job: relay chunks into the ledger,
/// verify the receipt, commit the session spend, and return the exact
/// blocks the synchronous dispatch would have — or the error string it
/// would have failed with.
async fn drain_stream(
    state: &ServerState,
    envelope: covenant_compute_buyer::SignedJobEnvelope,
) -> Result<Vec<Value>, String> {
    let job_id = envelope.payload.job_id;
    let streamed = match stream_and_verify(
        &state.http,
        &state.buyer,
        &state.identity,
        envelope,
        |chunk| state.streams.append_chunk(job_id, chunk),
    )
    .await
    {
        Ok(streamed) => streamed,
        Err(e @ (BuyerError::NotServed { .. } | BuyerError::ReceiptTimeout(_))) => {
            return Err(e.to_string())
        }
        Err(e) => return Err(format!("compute dispatch failed: {e}")),
    };

    let outcome = streamed.outcome;
    let receipt = &outcome.receipt.receipt;
    // The offered (envelope) price is the buyer's charge; count it against
    // the session cap, not the receipt's claimed price (see count_spend).
    state
        .caps
        .record_spend(outcome.envelope.payload.price_micro_usdc);

    let mut content = text_blocks(&outcome.output);
    let metadata = json!({
        "job_id": receipt.job_id,
        "operator_pubkey_b58": receipt.operator.pubkey_base58(),
        // The charge is the escrowed envelope price, not the receipt's
        // claim (see paid_response); report what the buyer actually paid.
        "price_micro_usdc": outcome.envelope.payload.price_micro_usdc,
        "result_hash_hex": receipt.result_hash_hex,
        "wall_ms": receipt.meter.wall_ms,
        "tokens_in": receipt.meter.tokens_in,
        "tokens_out": receipt.meter.tokens_out,
        // Why generation stopped (see paid_response): a `content_filter` or
        // `length` cut-off is named rather than read as a clean end.
        "finish_reason": receipt.meter.finish_reason,
        "receipt_verified": true,
        // Whether the live feed, assembled, equals the verified final
        // output. False grades only the preview — the output above is
        // receipt-verified either way.
        "stream_matched_output": streamed.stream_matched_output,
        "payout": outcome.payout,
    });
    content.push(json!({ "type": "text", "text": metadata.to_string() }));
    Ok(content)
}

/// `compute.stream_poll`: a cursor read of one streaming job. While it
/// runs, new chunks and status `streaming`; the concluding poll carries
/// the verified output and receipt blocks; a job whose drain failed
/// fails the poll with the original error.
async fn call_stream_poll(state: &ServerState, id: Value, arguments: Value) -> Value {
    let args: StreamPollArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(e) => return error(id, -32602, format!("invalid arguments: {e}")),
    };
    let page = match state.streams.poll(STREAM_OWNER, args.job_id, args.since) {
        Ok(page) => page,
        Err(e) => return tool_error(id, e.to_string()),
    };
    let status = match &page.outcome {
        None => "streaming",
        Some(Ok(_)) => "completed",
        Some(Err(_)) => "failed",
    };
    let bookkeeping = json!({
        "job_id": args.job_id,
        "status": status,
        "chunks": page.chunks,
        "next_seq": page.next_seq,
    });
    let mut content = vec![json!({ "type": "text", "text": bookkeeping.to_string() })];
    match page.outcome {
        None => {}
        Some(Ok(blocks)) => content.extend(blocks),
        Some(Err(message)) => return tool_error(id, message),
    }
    response(id, json!({ "content": content, "isError": false }))
}

/// `compute.receipts`: the buyer's paid-for view, every receipt
/// re-verified locally before it is shown. A verification failure is
/// surfaced on its row, never dropped — an agent reading this should
/// see exactly which receipt didn't check out.
async fn call_receipts(state: &ServerState, id: Value, arguments: Value) -> Value {
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .map(|l| l as usize)
        .unwrap_or(20);
    match list_verified_jobs(&state.http, &state.buyer, &state.identity, limit).await {
        Ok(rows) => {
            let unverified = rows
                .iter()
                .filter(|r| r.receipt_verified == Some(false))
                .count();
            let summary = json!({
                "jobs": rows,
                "count": rows.len(),
                "receipts_failing_verification": unverified,
            });
            response(
                id,
                json!({
                    "content": [{ "type": "text", "text": summary.to_string() }],
                    "isError": false,
                }),
            )
        }
        Err(e) => tool_error(id, format!("receipt list failed: {e}")),
    }
}

/// `compute.deposit`: claim a confirmed on-chain payment for this
/// buyer's pre-funded balance. Idempotent upstream — a re-claim
/// reports `credited: false` with the balance unchanged.
async fn call_deposit(state: &ServerState, id: Value, arguments: Value) -> Value {
    let Some(deposit_id) = arguments
        .get("deposit_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return error(id, -32602, "deposit_id (string) is required".into());
    };
    match claim_deposit(&state.http, &state.buyer, &state.identity, deposit_id).await {
        Ok(outcome) => {
            let text = serde_json::to_string(&outcome).unwrap_or_else(|e| e.to_string());
            response(
                id,
                json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
            )
        }
        Err(e) => tool_error(id, format!("deposit claim failed: {e}")),
    }
}

/// `compute.balance`: funds plus this deployment's top-up
/// instructions.
async fn call_balance(state: &ServerState, id: Value) -> Value {
    match funds_with_deposit_info(&state.http, &state.buyer, &state.identity).await {
        Ok(view) => response(
            id,
            json!({ "content": [{ "type": "text", "text": view.to_string() }], "isError": false }),
        ),
        Err(e) => tool_error(id, format!("balance fetch failed: {e}")),
    }
}

/// `compute.capacity`: the coordinator's live-capacity directory —
/// what is purchasable right now, per (kind, model) row with its ask
/// range. The read an agent makes before spending.
async fn call_capacity(state: &ServerState, id: Value) -> Value {
    match capacity(&state.http, &state.buyer).await {
        Ok(view) => {
            let text = serde_json::to_string(&view).unwrap_or_else(|e| e.to_string());
            response(
                id,
                json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
            )
        }
        Err(e) => tool_error(id, format!("capacity fetch failed: {e}")),
    }
}

/// `compute.dispute`: sign and record a dispute of one of this buyer's
/// completed jobs. No refund, no spend — the outcome is the reputation
/// fault on the coordinator's books.
async fn call_dispute(state: &ServerState, id: Value, arguments: Value) -> Value {
    let args: DisputeArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(e) => return error(id, -32602, format!("invalid arguments: {e}")),
    };
    match dispute_job(
        &state.http,
        &state.buyer,
        &state.identity,
        args.job_id,
        args.reason,
    )
    .await
    {
        Ok(outcome) => {
            let text = serde_json::to_string(&outcome).unwrap_or_else(|e| e.to_string());
            response(
                id,
                json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
            )
        }
        Err(e) => tool_error(id, format!("dispute failed: {e}")),
    }
}

/// `compute.cancel`: withdraw one of this buyer's still-unaccepted
/// jobs and take the refund now. On success the job's purchase entry
/// (if this home holds one) is voided, so its idempotency key may
/// honestly buy again — the coordinator already answered that the
/// money never moved.
async fn call_cancel(state: &ServerState, id: Value, arguments: Value) -> Value {
    let args: CancelArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(e) => return error(id, -32602, format!("invalid arguments: {e}")),
    };
    match cancel_job(&state.http, &state.buyer, &state.identity, args.job_id).await {
        Ok(view) => {
            let key_freed = match state.purchases.void_by_job(args.job_id) {
                Ok(freed) => freed,
                Err(e) => {
                    // The refund already happened; a re-drive of the
                    // still-bound key meets the coordinator's refunded
                    // echo and voids then. Answer the cancel, note the
                    // book problem where diagnostics go.
                    tracing::warn!(job_id = %args.job_id, error = %e, "cancelled but could not void the purchase entry");
                    false
                }
            };
            let text = serde_json::to_string(&json!({
                "job_id": view.job_id,
                "status": view.status,
                "refunded_micro_usdc": view.refunded_micro_usdc,
                "purchase_key_freed": key_freed,
            }))
            .unwrap_or_else(|e| e.to_string());
            response(
                id,
                json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
            )
        }
        Err(e) => tool_error(id, format!("cancel failed: {e}")),
    }
}

/// `compute.verify`: hold the chain to one job's money trail — the
/// receipt re-verified locally, then the payout transaction it names
/// read back from this buyer's own RPC endpoint and required to carry
/// exactly this receipt's memo and amount. Read-only; a not-yet-paid
/// job reports how far the trail goes instead of failing.
async fn call_verify(state: &ServerState, id: Value, arguments: Value) -> Value {
    let args: VerifyArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(e) => return error(id, -32602, format!("invalid arguments: {e}")),
    };
    match verify_payout(&state.http, &state.buyer, &state.identity, args.job_id).await {
        Ok(verification) => {
            let text = serde_json::to_string(&verification).unwrap_or_else(|e| e.to_string());
            response(
                id,
                json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
            )
        }
        Err(e) => tool_error(id, format!("payout verification failed: {e}")),
    }
}

/// `compute.output`: re-read a past job's output and its locally
/// re-verified receipt — the answer, recoverable after the session that
/// bought it is gone. Read-only; no spend, no book change.
async fn call_output(state: &ServerState, id: Value, arguments: Value) -> Value {
    let args: OutputArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(e) => return error(id, -32602, format!("invalid arguments: {e}")),
    };
    let view = match fetch_job_output(&state.http, &state.buyer, &state.identity, args.job_id).await
    {
        Ok(view) => view,
        Err(e) => return tool_error(id, format!("compute output failed: {e}")),
    };
    output_response(id, &view)
}

/// A re-read job's output blocks plus a receipt-metadata block, flagged
/// `isError` when the leading blocks are not the answer the buyer paid for.
/// Two cases fail: output that does not hash to the receipt (the coordinator
/// handed back bytes the operator never signed), and a refunded job, whose
/// output is the operator's signed failure cause and whose hold was returned
/// so nothing was charged. A verified failure receipt hashes fine, so
/// `receipt_verified` alone would call it clean and an agent would read the
/// failure text as the answer; the refund reason is what marks it unpaid.
fn output_response(id: Value, view: &JobOutputView) -> Value {
    let mut content = text_blocks(&view.output);
    let metadata = json!({
        "job_id": view.job_id,
        "status": view.status,
        "refund_reason": view.refund_reason,
        "receipt_verified": view.receipt_verified,
        "verification_error": view.verification_error,
        "receipt_status": view.receipt.as_ref().map(|r| format!("{:?}", r.receipt.status)),
        "operator_pubkey_b58": view.receipt.as_ref().map(|r| r.receipt.operator.pubkey_base58()),
        // Why generation stopped (see paid_response): a re-read of a
        // content-filtered or length-truncated answer names its cause too.
        "finish_reason": view
            .receipt
            .as_ref()
            .and_then(|r| r.receipt.meter.finish_reason),
        // What the buyer was charged: the settled hold (the released
        // envelope price for a fixed job, the metered draw for a lease),
        // not the receipt's committed price — which under-reports a fixed
        // job the node priced below its offer and over-reports a lease
        // closed before its window ran out (see paid_response). Falls back
        // to the receipt price only for a coordinator too old to report the
        // charge, the same posture the CLI's `output` takes.
        "price_micro_usdc": view
            .charged_micro_usdc
            .or_else(|| view.receipt.as_ref().map(|r| r.receipt.price_micro_usdc)),
        "payout": view.payout,
    });
    content.push(json!({ "type": "text", "text": metadata.to_string() }));
    // A receipt whose operator status is not Ok is a signed failure even when
    // the coordinator serves the job as completed with no refund — the leading
    // blocks are the failure cause, not the paid answer.
    let is_error = view.receipt_verified == Some(false)
        || view.refund_reason.is_some()
        || view.receipt_reports_failure();
    response(id, json!({ "content": content, "isError": is_error }))
}

/// `compute.withdraw`: move unspent balance out to a wallet the caller
/// names. The signed request authorizes itself. A client-supplied
/// `withdrawal_id` is the idempotency key end to end — request, books
/// debit, on-chain memo — so a retry under the same id never debits
/// twice; a caller that wants a safe retry must supply and reuse one.
/// An omitted id is minted fresh per call, so a retry after a lost
/// response is a distinct withdrawal. Overdraws are refused upstream by
/// the coordinator's books.
async fn call_withdraw(state: &ServerState, id: Value, arguments: Value) -> Value {
    let args: WithdrawArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(e) => return error(id, -32602, format!("invalid arguments: {e}")),
    };
    let withdrawal_id = args.withdrawal_id.unwrap_or_else(uuid::Uuid::new_v4);
    match withdraw(
        &state.http,
        &state.buyer,
        &state.identity,
        withdrawal_id,
        args.amount_micro_usdc,
        &args.recipient_address_b58,
    )
    .await
    {
        Ok(outcome) => {
            let text = serde_json::to_string(&outcome).unwrap_or_else(|e| e.to_string());
            response(
                id,
                json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
            )
        }
        Err(e) => tool_error(id, format!("withdraw failed: {e}")),
    }
}

/// `compute.withdrawals`: this buyer's withdrawal history, newest
/// first — the audit companion to `compute.withdraw`. Read-only, a
/// signed read, same posture as the balance; each row carries whether
/// its transfer has landed and the on-chain memo to check it against.
async fn call_withdrawals(state: &ServerState, id: Value) -> Value {
    match list_withdrawals(&state.http, &state.buyer, &state.identity).await {
        Ok(rows) => {
            let summary = json!({ "withdrawals": rows, "count": rows.len() });
            response(
                id,
                json!({
                    "content": [{ "type": "text", "text": summary.to_string() }],
                    "isError": false,
                }),
            )
        }
        Err(e) => tool_error(id, format!("withdrawal list failed: {e}")),
    }
}

async fn handle_line(state: &Arc<ServerState>, line: &str) -> Option<Value> {
    let msg: Incoming = match serde_json::from_str(line) {
        Ok(msg) => msg,
        Err(e) => return Some(error(Value::Null, -32700, format!("parse error: {e}"))),
    };
    let Some(id) = msg.id else {
        // Notification — nothing to answer, per JSON-RPC 2.0.
        return None;
    };

    match msg.method.as_str() {
        "initialize" => Some(response(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "covenant-compute",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
        )),
        "ping" => Some(response(id, json!({}))),
        "tools/list" => Some(response(
            id,
            json!({ "tools": [
                infer_tool_spec(state.caps.max_price_micro_usdc()),
                embed_tool_spec(state.caps.max_price_micro_usdc()),
                transcribe_tool_spec(state.caps.max_price_micro_usdc()),
                speak_tool_spec(state.caps.max_price_micro_usdc()),
                run_tool_spec(state.caps.max_price_micro_usdc()),
                stream_start_tool_spec(state.caps.max_price_micro_usdc()),
                stream_poll_tool_spec(),
                receipts_tool_spec(),
                deposit_tool_spec(),
                balance_tool_spec(),
                capacity_tool_spec(),
                withdraw_tool_spec(),
                withdrawals_tool_spec(),
                dispute_tool_spec(),
                cancel_tool_spec(),
                verify_tool_spec(),
                output_tool_spec(),
                agent_tool_spec(state.caps.max_price_micro_usdc()),
            ] }),
        )),
        "tools/call" => {
            let name = msg.params.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = msg.params.get("arguments").cloned().unwrap_or(json!({}));
            match name {
                INFER_TOOL => Some(call_infer(state, id, arguments).await),
                EMBED_TOOL => Some(call_embed(state, id, arguments).await),
                TRANSCRIBE_TOOL => Some(call_transcribe(state, id, arguments).await),
                SPEAK_TOOL => Some(call_speak(state, id, arguments).await),
                RUN_TOOL => Some(call_run(state, id, arguments).await),
                STREAM_START_TOOL => Some(call_stream_start(state, id, arguments).await),
                STREAM_POLL_TOOL => Some(call_stream_poll(state, id, arguments).await),
                RECEIPTS_TOOL => Some(call_receipts(state, id, arguments).await),
                DEPOSIT_TOOL => Some(call_deposit(state, id, arguments).await),
                BALANCE_TOOL => Some(call_balance(state, id).await),
                CAPACITY_TOOL => Some(call_capacity(state, id).await),
                WITHDRAW_TOOL => Some(call_withdraw(state, id, arguments).await),
                WITHDRAWALS_TOOL => Some(call_withdrawals(state, id).await),
                DISPUTE_TOOL => Some(call_dispute(state, id, arguments).await),
                CANCEL_TOOL => Some(call_cancel(state, id, arguments).await),
                VERIFY_TOOL => Some(call_verify(state, id, arguments).await),
                OUTPUT_TOOL => Some(call_output(state, id, arguments).await),
                AGENT_TOOL => Some(call_agent(state, id, arguments).await),
                _ => Some(error(id, -32602, format!("unknown tool: {name}"))),
            }
        }
        other => Some(error(id, -32601, format!("method not found: {other}"))),
    }
}

/// Reads a numeric knob: unset (or blank) takes `default`, a set but
/// unparseable value fails loudly instead of silently falling back — a
/// mistyped `COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC` must not quietly
/// become the permissive default and remove the spend cap the operator
/// meant to set. `hint` reads into "{key} {hint}".
fn env_or<T: std::str::FromStr>(key: &str, default: T, hint: &str) -> anyhow::Result<T>
where
    T::Err: std::fmt::Display,
{
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => v
            .trim()
            .parse()
            .map_err(|e| anyhow::anyhow!("{key} {hint} (got {v:?}): {e}")),
        _ => Ok(default),
    }
}

fn mcp_home() -> anyhow::Result<PathBuf> {
    if let Ok(p) = std::env::var("COVENANT_COMPUTE_MCP_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".covenant-compute-mcp"))
}

const MCP_USAGE: &str = "\
covenant-compute-mcp — MCP stdio server selling Covenant compute network
inference and batch jobs as tools

Usage:
  covenant-compute-mcp             speak MCP over stdio (add it to an MCP client)
  covenant-compute-mcp --version   print the version

Requires COVENANT_COMPUTE_COORDINATOR_URL. Spend guards:
COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC caps any one call;
COVENANT_COMPUTE_MAX_TOTAL_MICRO_USDC caps how much one server run
commits before it refuses, and resets when the server restarts (the
funded balance is the limit that persists across runs). Identity and
purchase records live under COVENANT_COMPUTE_MCP_HOME. The full knob
table is in the covenant-compute-buyer README.
";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Help and version answer before config or the stdio loop — a user
    // asking a question must get an answer, not a server waiting on
    // stdin; any other argument refuses rather than hanging a
    // misconfigured MCP client.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--help" | "-h" | "help") => {
            print!("{MCP_USAGE}");
            return Ok(());
        }
        Some("--version" | "-V" | "version") => {
            println!("{} {}", env!("CARGO_BIN_NAME"), env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some(other) => anyhow::bail!(
            "unexpected argument {other:?} — this binary takes none; run `--help` for the \
             summary"
        ),
        None => {}
    }

    // stdout is the protocol channel; every diagnostic goes to stderr.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "covenant_compute_mcp=info".into()),
        )
        .init();

    let coordinator_url = std::env::var("COVENANT_COMPUTE_COORDINATOR_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .context("COVENANT_COMPUTE_COORDINATOR_URL must be set and non-empty")?;
    anyhow::ensure!(
        coordinator_url.starts_with("http://") || coordinator_url.starts_with("https://"),
        "COVENANT_COMPUTE_COORDINATOR_URL must start with http:// or https:// (got \
         {coordinator_url:?})"
    );

    let home = mcp_home()?;
    std::fs::create_dir_all(&home).with_context(|| format!("create {}", home.display()))?;
    let identity = LocalIdentity::load_or_create(&home.join("identity.json"), "buyer@compute")
        .context("load or create buyer identity")?;
    // Spend guards, parsed before readiness is announced: a mistyped cap
    // must fail the boot now, not silently widen to the permissive
    // default — or, for the total, to no cap at all — and let a client
    // start spending under a ceiling that was never really set.
    let max_price_micro_usdc = env_or(
        "COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC",
        1_000_000u64,
        "must be a whole number of micro-USDC",
    )?;
    let default_deadline_ms = env_or(
        "COVENANT_COMPUTE_DEADLINE_MS",
        60_000u64,
        "must be a whole number of milliseconds",
    )?;
    let max_active_streams = env_or(
        "COVENANT_COMPUTE_MAX_ACTIVE_STREAMS",
        4usize,
        "must be a whole number",
    )?;
    let max_total_micro_usdc: Option<u64> =
        match std::env::var("COVENANT_COMPUTE_MAX_TOTAL_MICRO_USDC") {
            Ok(v) if !v.trim().is_empty() => Some(v.trim().parse().map_err(|e| {
                anyhow::anyhow!(
                    "COVENANT_COMPUTE_MAX_TOTAL_MICRO_USDC must be a whole number of \
                     micro-USDC (got {v:?}): {e}"
                )
            })?),
            _ => None,
        };

    tracing::info!(
        pubkey = %bs58::encode(identity.pubkey_bytes()).into_string(),
        coordinator = %coordinator_url,
        "covenant-compute-mcp ready"
    );

    let state = Arc::new(ServerState {
        // The buyer crate's client: dispatch-sized timeouts plus the
        // wire version stamped on every request.
        http: covenant_compute_buyer::http_client(),
        buyer: BuyerConfig {
            coordinator_url,
            poll_interval: Duration::from_millis(500),
            referral_code: std::env::var("COVENANT_COMPUTE_REFERRAL_CODE")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
            rpc_url: std::env::var("COVENANT_COMPUTE_RPC_URL")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
        },
        identity,
        default_deadline_ms,
        caps: SpendCaps::new(max_price_micro_usdc, max_total_micro_usdc),
        streams: StreamJobs::new(max_active_streams),
        purchases: PurchaseBook::open(&home.join("purchases.jsonl"))
            .context("open purchase book")?,
        clips_dir: home.join("clips"),
    });

    // One request at a time: each handle_line runs to completion before
    // the next line is read. The synchronous tools guard the session
    // spend cap with a check-then-record (session_cap_refusal then
    // record_spend), not the atomic reservation the HTTP doors use, so
    // its correctness rests on this serialization — dispatching lines
    // concurrently would let several calls pass the cap before any
    // records its spend, overshooting the configured total.
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle_line(&state, &line).await {
            stdout.write_all(reply.to_string().as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state(max_price: u64, max_total: Option<u64>) -> Arc<ServerState> {
        Arc::new(ServerState {
            http: reqwest::Client::new(),
            buyer: BuyerConfig {
                // Guaranteed-unroutable: any test reaching the network
                // fails fast and loudly here.
                coordinator_url: "http://127.0.0.1:1".into(),
                poll_interval: Duration::from_millis(10),
                referral_code: None,
                rpc_url: None,
            },
            identity: LocalIdentity::generate("buyer@test"),
            default_deadline_ms: 1_000,
            caps: SpendCaps::new(max_price, max_total),
            streams: StreamJobs::new(4),
            purchases: PurchaseBook::in_memory(),
            clips_dir: std::env::temp_dir().join(format!(
                "covenant-compute-mcp-test-clips-{}",
                uuid::Uuid::new_v4()
            )),
        })
    }

    #[test]
    fn an_underfunded_dispatch_names_the_deposit_tools() {
        let v = dispatch_failed(
            Value::from(1),
            BuyerError::SubmitRefused {
                status: 402,
                body: "buyer X has insufficient funds: hold needs 1000 micro-USDC, 0 available"
                    .into(),
            },
        );
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        // The shortfall survives and the fix names the tools an MCP client
        // can actually call, so a first-run agent isn't left at a dead end.
        assert!(text.contains("insufficient funds"), "{text}");
        assert!(text.contains("compute.balance"), "{text}");
        assert!(text.contains("compute.deposit"), "{text}");
        assert_eq!(v["result"]["isError"], serde_json::json!(true));
    }

    #[test]
    fn a_non_funding_dispatch_failure_reads_plainly() {
        let v = dispatch_failed(
            Value::from(1),
            BuyerError::Coordinator("connection refused".into()),
        );
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("compute dispatch failed"), "{text}");
        assert!(!text.contains("compute.balance"), "{text}");
    }

    #[test]
    fn the_session_cap_counts_the_offered_price_not_a_lower_receipt_price() {
        // The coordinator charges the held envelope price; a node that
        // signs a receipt below its offer must not shrink what counts
        // against the session cap, or it under-reports a buyer past its
        // configured total while the buyer is charged the full offer.
        use covenant_a2a::A2ATaskStatus;
        use covenant_compute_buyer::DispatchOutcome;
        use covenant_compute_protocol::{JobMeter, SignedWorkReceipt, WorkReceiptPayload};

        let state = test_state(10_000, Some(10_000));
        let operator = LocalIdentity::generate("operator@test");
        let envelope = sign_envelope(
            &state.buyer,
            &state.identity,
            JobRequest {
                kind: JobKind::InferenceCall,
                input: vec![Content::text("hi")],
                model: None,
                gpu_class: None,
                min_vram_gb: None,
                min_reputation_bps: None,
                price_micro_usdc: 1_000,
                deadline_ms: 60_000,
            },
        )
        .expect("envelope signs");
        let receipt = SignedWorkReceipt::sign(
            WorkReceiptPayload {
                job_id: envelope.payload.job_id,
                operator: operator.agent_id(),
                job_hash_hex: "00".into(),
                result_hash_hex: "00".into(),
                meter: JobMeter {
                    wall_ms: 1,
                    tokens_in: None,
                    tokens_out: None,
                    gpu_seconds: None,
                    finish_reason: None,
                },
                // A node under-reporting: well below the 1000 it was offered.
                price_micro_usdc: 1,
                status: A2ATaskStatus::Ok,
                executed_at_ms: 1,
                node_audit_root_hex: "00".into(),
            },
            &operator,
        )
        .expect("receipt signs");
        let outcome = DispatchOutcome {
            envelope,
            receipt,
            output: vec![Content::text("hi")],
            payout: None,
        };

        let reply =
            count_spend_and_respond(&state, Value::from(1), &outcome, None, OutputRender::Inline);
        assert_eq!(state.caps.spent_micro_usdc(), 1_000);
        // The buyer is told the charge (the 1000 offer), not the node's
        // under-reported receipt claim of 1.
        let blocks = reply["result"]["content"].as_array().unwrap();
        let meta: Value = serde_json::from_str(blocks.last().unwrap()["text"].as_str().unwrap())
            .expect("metadata block is json");
        assert_eq!(meta["price_micro_usdc"], 1_000);
    }

    #[tokio::test]
    async fn initialize_advertises_tools_capability() {
        let state = test_state(1_000, None);
        let reply = handle_line(
            &state,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        )
        .await
        .expect("initialize gets a response");
        assert_eq!(reply["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert!(reply["result"]["capabilities"]["tools"].is_object());
        assert_eq!(reply["id"], 1);
    }

    #[tokio::test]
    async fn notifications_get_no_response() {
        let state = test_state(1_000, None);
        assert!(handle_line(
            &state,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        )
        .await
        .is_none());
    }

    #[tokio::test]
    async fn tools_list_advertises_infer_with_string_id_echoed() {
        let state = test_state(1_000, None);
        let reply = handle_line(
            &state,
            r#"{"jsonrpc":"2.0","id":"abc","method":"tools/list"}"#,
        )
        .await
        .unwrap();
        assert_eq!(reply["id"], "abc", "string ids echo verbatim");
        assert_eq!(reply["result"]["tools"][0]["name"], INFER_TOOL);
        let schema = &reply["result"]["tools"][0]["inputSchema"];
        assert!(schema["properties"]["prompt"].is_object());
        assert!(schema["properties"]["messages"].is_object());
    }

    #[tokio::test]
    async fn prompt_and_messages_together_are_invalid_params() {
        let state = test_state(1_000, None);
        let reply = handle_line(
            &state,
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"compute.infer","arguments":{"prompt":"hi","messages":[{"role":"user","content":"hi"}]}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(reply["error"]["code"], -32602);
        let msg = reply["error"]["message"].as_str().unwrap();
        assert!(msg.contains("not both"), "got: {msg}");
    }

    #[tokio::test]
    async fn an_out_of_bounds_idempotency_key_is_invalid_params_before_signing() {
        let state = test_state(1_000, None);
        let empty = handle_line(
            &state,
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"compute.infer","arguments":{"prompt":"hi","idempotency_key":""}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(empty["error"]["code"], -32602);
        let msg = empty["error"]["message"].as_str().unwrap();
        assert!(msg.contains("1..=128"), "got: {msg}");

        let line = format!(
            r#"{{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{{"name":"compute.infer","arguments":{{"prompt":"hi","idempotency_key":"{}"}}}}}}"#,
            "k".repeat(129)
        );
        let long = handle_line(&state, &line).await.unwrap();
        assert_eq!(long["error"]["code"], -32602);
        let msg = long["error"]["message"].as_str().unwrap();
        assert!(msg.contains("1..=128"), "got: {msg}");
    }

    #[tokio::test]
    async fn tools_list_advertises_the_full_buyer_surface() {
        let state = test_state(1_000, None);
        let reply = handle_line(&state, r#"{"jsonrpc":"2.0","id":9,"method":"tools/list"}"#)
            .await
            .unwrap();
        let names: Vec<&str> = reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                INFER_TOOL,
                EMBED_TOOL,
                TRANSCRIBE_TOOL,
                SPEAK_TOOL,
                RUN_TOOL,
                STREAM_START_TOOL,
                STREAM_POLL_TOOL,
                RECEIPTS_TOOL,
                DEPOSIT_TOOL,
                BALANCE_TOOL,
                CAPACITY_TOOL,
                WITHDRAW_TOOL,
                WITHDRAWALS_TOOL,
                DISPUTE_TOOL,
                CANCEL_TOOL,
                VERIFY_TOOL,
                OUTPUT_TOOL,
                AGENT_TOOL
            ]
        );
    }

    #[tokio::test]
    async fn transcribe_refuses_a_missing_or_malformed_clip_before_any_network() {
        let state = test_state(1_000, None);
        // audio_base64 is required: an empty argument object never
        // deserializes, so nothing is signed or dispatched.
        let reply = handle_line(
            &state,
            r#"{"jsonrpc":"2.0","id":31,"method":"tools/call","params":{"name":"compute.transcribe","arguments":{}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(reply["error"]["code"], -32602);

        // A clip that is not valid base64 is refused before any dispatch,
        // naming the reason — the buyer's error, never an operator's fault.
        let reply = handle_line(
            &state,
            r#"{"jsonrpc":"2.0","id":32,"method":"tools/call","params":{"name":"compute.transcribe","arguments":{"audio_base64":"not base64!"}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(reply["error"]["code"], -32602);
        assert!(
            reply["error"]["message"]
                .as_str()
                .unwrap()
                .contains("valid base64"),
            "got: {reply}"
        );
    }

    #[tokio::test]
    async fn speak_refuses_missing_or_empty_text_before_any_network() {
        let state = test_state(1_000, None);
        // text is required: an empty argument object never deserializes,
        // so nothing is signed or dispatched.
        let reply = handle_line(
            &state,
            r#"{"jsonrpc":"2.0","id":41,"method":"tools/call","params":{"name":"compute.speak","arguments":{}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(reply["error"]["code"], -32602);

        // Blank text is refused before any dispatch, naming the reason —
        // the buyer's error, never an operator's fault on empty input.
        let reply = handle_line(
            &state,
            r#"{"jsonrpc":"2.0","id":42,"method":"tools/call","params":{"name":"compute.speak","arguments":{"text":"   "}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(reply["error"]["code"], -32602);
        assert!(
            reply["error"]["message"]
                .as_str()
                .unwrap()
                .contains("no text"),
            "got: {reply}"
        );
    }

    #[test]
    fn a_speech_result_renders_as_a_saved_clip_never_base64() {
        use base64::Engine as _;
        use covenant_a2a::A2ATaskStatus;
        use covenant_compute_buyer::DispatchOutcome;
        use covenant_compute_protocol::{
            speech_output, JobMeter, SignedWorkReceipt, WorkReceiptPayload,
        };

        let dir = tempfile::tempdir().unwrap();
        let state = test_state(10_000, None);
        let operator = LocalIdentity::generate("operator@test");
        let envelope = sign_envelope(
            &state.buyer,
            &state.identity,
            JobRequest {
                kind: JobKind::SpeechSynthesis,
                input: vec![Content::text("hello")],
                model: None,
                gpu_class: None,
                min_vram_gb: None,
                min_reputation_bps: None,
                price_micro_usdc: 1_000,
                deadline_ms: 60_000,
            },
        )
        .expect("envelope signs");
        let receipt = SignedWorkReceipt::sign(
            WorkReceiptPayload {
                job_id: envelope.payload.job_id,
                operator: operator.agent_id(),
                job_hash_hex: "00".into(),
                result_hash_hex: "00".into(),
                meter: JobMeter {
                    wall_ms: 1,
                    tokens_in: None,
                    tokens_out: None,
                    gpu_seconds: None,
                    finish_reason: None,
                },
                price_micro_usdc: 1_000,
                status: A2ATaskStatus::Ok,
                executed_at_ms: 1,
                node_audit_root_hex: "00".into(),
            },
            &operator,
        )
        .expect("receipt signs");
        let clip = b"RIFF\0\0\0\0WAVEfmt ";
        let audio_b64 = base64::engine::general_purpose::STANDARD.encode(clip);
        let outcome = DispatchOutcome {
            envelope,
            receipt,
            output: vec![speech_output(
                "say-1",
                audio_b64.clone(),
                "wav",
                Some(22_050),
            )],
            payout: None,
        };

        let reply = paid_response(
            Value::from(1),
            &outcome,
            OutputRender::SpeechClip { dir: dir.path() },
        );

        // The base64 audio never rides back into the caller's context.
        let whole = serde_json::to_string(&reply).unwrap();
        assert!(
            !whole.contains(&audio_b64),
            "the clip's base64 leaked into the tool result"
        );

        let blocks = reply["result"]["content"].as_array().unwrap();
        let saved: Value = serde_json::from_str(blocks[0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(saved["saved"], true);
        assert_eq!(saved["format"], "wav");
        assert_eq!(saved["model"], "say-1");
        // The clip is really on disk, with the operator's exact bytes.
        let path = saved["path"].as_str().unwrap();
        assert!(path.ends_with(".wav"), "{path}");
        assert_eq!(std::fs::read(path).unwrap(), clip);
        // The receipt block still follows, so a buyer always learns they paid.
        let meta: Value = serde_json::from_str(blocks[1]["text"].as_str().unwrap()).unwrap();
        assert_eq!(meta["receipt_verified"], true);
    }

    #[test]
    fn a_re_read_judges_success_by_the_operators_signed_receipt() {
        use covenant_a2a::A2ATaskStatus;
        use covenant_compute_protocol::{JobMeter, SignedWorkReceipt, WorkReceiptPayload};

        let operator = LocalIdentity::generate("operator@test");
        let job_id = uuid::Uuid::new_v4();
        // The operator signed a failure receipt: it verifies by signature and
        // output hash, so receipt_verified is Some(true). Its content is the
        // cause of the failure, and the coordinator refunded the hold.
        let receipt = SignedWorkReceipt::sign(
            WorkReceiptPayload {
                job_id,
                operator: operator.agent_id(),
                job_hash_hex: "00".into(),
                result_hash_hex: "00".into(),
                meter: JobMeter {
                    wall_ms: 1,
                    tokens_in: None,
                    tokens_out: None,
                    gpu_seconds: None,
                    finish_reason: None,
                },
                price_micro_usdc: 0,
                status: A2ATaskStatus::Error,
                executed_at_ms: 1,
                node_audit_root_hex: "00".into(),
            },
            &operator,
        )
        .expect("receipt signs");
        let failed = JobOutputView {
            job_id,
            status: "refunded".into(),
            refund_reason: Some("execution_failed".into()),
            output: vec![Content::Text {
                text: "the model crashed mid-run".into(),
            }],
            receipt: Some(receipt),
            receipt_verified: Some(true),
            verification_error: None,
            payout: None,
            charged_micro_usdc: None,
        };

        // A verified hash is not enough: the refund reason is what says the
        // bytes are a failure cause, not the answer the buyer paid for.
        let reply = output_response(Value::from(1), &failed);
        assert_eq!(
            reply["result"]["isError"], true,
            "a refunded job's output must not read as the answer: {reply}"
        );
        // The cause still rides back for a caller that wants it — but flagged.
        let blocks = reply["result"]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["text"], "the model crashed mid-run");

        // A charged, un-refunded job the operator signed Ok is the answer and
        // reads clean.
        let ok_receipt = SignedWorkReceipt::sign(
            WorkReceiptPayload {
                status: A2ATaskStatus::Ok,
                ..failed.receipt.clone().unwrap().receipt
            },
            &operator,
        )
        .expect("receipt signs");
        let served = JobOutputView {
            job_id,
            status: "completed".into(),
            refund_reason: None,
            output: vec![Content::Text {
                text: "the answer".into(),
            }],
            receipt: Some(ok_receipt),
            receipt_verified: Some(true),
            verification_error: None,
            payout: None,
            charged_micro_usdc: None,
        };
        let reply = output_response(Value::from(2), &served);
        assert_eq!(reply["result"]["isError"], false, "{reply}");

        // The operator signed an Error receipt but the coordinator served the
        // job as completed with no refund. Trust the operator's signed verdict
        // over the relay: the failure text must not read as the paid answer.
        let hidden_failure = JobOutputView {
            status: "completed".into(),
            refund_reason: None,
            ..failed
        };
        let reply = output_response(Value::from(3), &hidden_failure);
        assert_eq!(
            reply["result"]["isError"], true,
            "an operator-signed failure served as completed must not read clean: {reply}"
        );
    }

    #[test]
    fn a_re_read_reports_the_settled_charge_not_the_receipt_claim() {
        use covenant_a2a::A2ATaskStatus;
        use covenant_compute_protocol::{JobMeter, SignedWorkReceipt, WorkReceiptPayload};

        let operator = LocalIdentity::generate("operator@test");
        let job_id = uuid::Uuid::new_v4();
        // The receipt commits the node's claim: a lease's window ceiling, or
        // a fixed job priced below the offer the buyer escrowed.
        let receipt = SignedWorkReceipt::sign(
            WorkReceiptPayload {
                job_id,
                operator: operator.agent_id(),
                job_hash_hex: "00".into(),
                result_hash_hex: "00".into(),
                meter: JobMeter {
                    wall_ms: 1,
                    tokens_in: None,
                    tokens_out: None,
                    gpu_seconds: None,
                    finish_reason: None,
                },
                price_micro_usdc: 30_000,
                status: A2ATaskStatus::Ok,
                executed_at_ms: 1,
                node_audit_root_hex: "00".into(),
            },
            &operator,
        )
        .expect("receipt signs");

        let reported_charge = |view: &JobOutputView| -> u64 {
            let reply = output_response(Value::from(1), view);
            let blocks = reply["result"]["content"].as_array().unwrap().clone();
            let meta: Value =
                serde_json::from_str(blocks.last().unwrap()["text"].as_str().unwrap()).unwrap();
            meta["price_micro_usdc"].as_u64().unwrap()
        };

        let settled = JobOutputView {
            job_id,
            status: "completed".into(),
            refund_reason: None,
            output: vec![Content::Text {
                text: "the answer".into(),
            }],
            receipt: Some(receipt),
            receipt_verified: Some(true),
            verification_error: None,
            payout: None,
            charged_micro_usdc: Some(5_000),
        };
        // An early-closed lease settled 5_000 of its 30_000 window: the
        // re-read reports what settled, never the ceiling the receipt commits.
        assert_eq!(reported_charge(&settled), 5_000);

        // A coordinator too old to report the charge falls back to the
        // receipt's committed price rather than reporting nothing.
        let legacy = JobOutputView {
            charged_micro_usdc: None,
            ..settled
        };
        assert_eq!(reported_charge(&legacy), 30_000);
    }

    #[test]
    fn a_re_read_names_a_content_filtered_answers_cause() {
        use covenant_a2a::A2ATaskStatus;
        use covenant_compute_protocol::{
            FinishReason, JobMeter, SignedWorkReceipt, WorkReceiptPayload,
        };

        let operator = LocalIdentity::generate("operator@test");
        let job_id = uuid::Uuid::new_v4();
        // An Ok receipt whose backend cut the generation short for content
        // policy: the job settled and charged, but the text is a partial
        // stopped for safety, not a complete answer. An agent must see that.
        let receipt = SignedWorkReceipt::sign(
            WorkReceiptPayload {
                job_id,
                operator: operator.agent_id(),
                job_hash_hex: "00".into(),
                result_hash_hex: "00".into(),
                meter: JobMeter {
                    wall_ms: 1,
                    tokens_in: None,
                    tokens_out: None,
                    gpu_seconds: None,
                    finish_reason: Some(FinishReason::ContentFilter),
                },
                price_micro_usdc: 1_000,
                status: A2ATaskStatus::Ok,
                executed_at_ms: 1,
                node_audit_root_hex: "00".into(),
            },
            &operator,
        )
        .expect("receipt signs");
        let view = JobOutputView {
            job_id,
            status: "completed".into(),
            refund_reason: None,
            output: vec![Content::Text {
                text: "as far as I can".into(),
            }],
            receipt: Some(receipt),
            receipt_verified: Some(true),
            verification_error: None,
            payout: None,
            charged_micro_usdc: Some(1_000),
        };
        let reply = output_response(Value::from(1), &view);
        let blocks = reply["result"]["content"].as_array().unwrap();
        let meta: Value =
            serde_json::from_str(blocks.last().unwrap()["text"].as_str().unwrap()).unwrap();
        assert_eq!(meta["finish_reason"], "content_filter");
    }

    #[tokio::test]
    async fn embed_refuses_missing_or_blank_text_before_any_network() {
        let state = test_state(1_000, None);
        // The text arg is required: an empty argument object never
        // deserializes, so nothing is signed or dispatched.
        let reply = handle_line(
            &state,
            r#"{"jsonrpc":"2.0","id":21,"method":"tools/call","params":{"name":"compute.embed","arguments":{}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(reply["error"]["code"], -32602);

        // A blank text is refused before any dispatch, naming the reason.
        let reply = handle_line(
            &state,
            r#"{"jsonrpc":"2.0","id":22,"method":"tools/call","params":{"name":"compute.embed","arguments":{"text":"   "}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(reply["error"]["code"], -32602);
        assert!(
            reply["error"]["message"]
                .as_str()
                .unwrap()
                .contains("must not be empty"),
            "got: {reply}"
        );
    }

    #[tokio::test]
    async fn stream_start_refuses_bad_args_and_over_cap_price_before_any_network() {
        let state = test_state(100, None);
        let reply = handle_line(
            &state,
            r#"{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"compute.stream_start","arguments":{}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(reply["error"]["code"], -32602);

        let reply = handle_line(
            &state,
            r#"{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"compute.stream_start","arguments":{"prompt":"hi","price_micro_usdc":200}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(reply["result"]["isError"], true);
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("ceiling"), "got: {text}");
        assert_eq!(state.streams.active_committed(STREAM_OWNER), 0);

        // Streaming can't back exactly-once, so a key is refused up front,
        // pointing at compute.infer — never accepted and quietly double-paid.
        let reply = handle_line(
            &state,
            r#"{"jsonrpc":"2.0","id":14,"method":"tools/call","params":{"name":"compute.stream_start","arguments":{"prompt":"hi","price_micro_usdc":50,"idempotency_key":"k"}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(reply["result"]["isError"], true);
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("idempotency_key") && text.contains(INFER_TOOL),
            "got: {text}"
        );
        assert_eq!(state.streams.active_committed(STREAM_OWNER), 0);
    }

    #[tokio::test]
    async fn stream_start_rolls_back_and_frees_the_slot_when_submit_fails() {
        let state = test_state(1_000, None);
        let reply = handle_line(
            &state,
            r#"{"jsonrpc":"2.0","id":13,"method":"tools/call","params":{"name":"compute.stream_start","arguments":{"prompt":"hi"}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(reply["result"]["isError"], true);
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("compute dispatch failed"), "got: {text}");
        assert_eq!(state.streams.active_committed(STREAM_OWNER), 0);
    }

    #[tokio::test]
    async fn session_cap_counts_in_flight_streaming_offers() {
        let state = test_state(1_000, Some(500));
        state
            .streams
            .try_start(STREAM_OWNER, uuid::Uuid::new_v4(), 400)
            .unwrap();
        // 0 spent + 400 streaming + 200 offered > 500: refused, on the
        // sync path and the streaming path alike.
        for tool in ["compute.infer", "compute.stream_start"] {
            let reply = handle_line(
                &state,
                &format!(
                    r#"{{"jsonrpc":"2.0","id":14,"method":"tools/call","params":{{"name":"{tool}","arguments":{{"prompt":"hi","price_micro_usdc":200}}}}}}"#
                ),
            )
            .await
            .unwrap();
            assert_eq!(reply["result"]["isError"], true, "tool: {tool}");
            let text = reply["result"]["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("session spend cap"), "got: {text}");
            assert!(text.contains("400 in flight"), "got: {text}");
        }
    }

    #[tokio::test]
    async fn stream_poll_reads_cursor_terminal_payload_and_refuses_unknown_jobs() {
        let state = test_state(1_000, None);
        let unknown = uuid::Uuid::new_v4();
        let reply = handle_line(
            &state,
            &format!(
                r#"{{"jsonrpc":"2.0","id":15,"method":"tools/call","params":{{"name":"compute.stream_poll","arguments":{{"job_id":"{unknown}"}}}}}}"#
            ),
        )
        .await
        .unwrap();
        assert_eq!(reply["result"]["isError"], true);
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("no streaming job"), "got: {text}");

        let job = uuid::Uuid::new_v4();
        state.streams.try_start(STREAM_OWNER, job, 100).unwrap();
        state.streams.append_chunk(job, "hel");
        state.streams.append_chunk(job, "lo");
        let reply = handle_line(
            &state,
            &format!(
                r#"{{"jsonrpc":"2.0","id":16,"method":"tools/call","params":{{"name":"compute.stream_poll","arguments":{{"job_id":"{job}"}}}}}}"#
            ),
        )
        .await
        .unwrap();
        assert_eq!(reply["result"]["isError"], false);
        let page: Value =
            serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(page["status"], "streaming");
        assert_eq!(page["chunks"], json!(["hel", "lo"]));
        assert_eq!(page["next_seq"], 2);

        state
            .streams
            .conclude(job, Ok(vec![json!({"type": "text", "text": "hello"})]));
        let reply = handle_line(
            &state,
            &format!(
                r#"{{"jsonrpc":"2.0","id":17,"method":"tools/call","params":{{"name":"compute.stream_poll","arguments":{{"job_id":"{job}","since":2}}}}}}"#
            ),
        )
        .await
        .unwrap();
        assert_eq!(reply["result"]["isError"], false);
        let page: Value =
            serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(page["status"], "completed");
        assert_eq!(page["chunks"], json!([]));
        assert_eq!(reply["result"]["content"][1]["text"], "hello");

        let failed = uuid::Uuid::new_v4();
        state.streams.try_start(STREAM_OWNER, failed, 100).unwrap();
        state
            .streams
            .conclude(failed, Err("job was not served: refunded".into()));
        let reply = handle_line(
            &state,
            &format!(
                r#"{{"jsonrpc":"2.0","id":18,"method":"tools/call","params":{{"name":"compute.stream_poll","arguments":{{"job_id":"{failed}"}}}}}}"#
            ),
        )
        .await
        .unwrap();
        assert_eq!(reply["result"]["isError"], true);
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("not served"), "got: {text}");
    }

    #[tokio::test]
    async fn withdraw_with_bad_arguments_is_invalid_params() {
        let state = test_state(1_000, None);
        for arguments in [
            r#"{}"#,
            r#"{"amount_micro_usdc":100}"#,
            r#"{"amount_micro_usdc":"lots","recipient_address_b58":"x"}"#,
            r#"{"amount_micro_usdc":100,"recipient_address_b58":"w","withdrawal_id":"not-a-uuid"}"#,
        ] {
            let reply = handle_line(
                &state,
                &format!(
                    r#"{{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{{"name":"compute.withdraw","arguments":{arguments}}}}}"#
                ),
            )
            .await
            .unwrap();
            assert_eq!(reply["error"]["code"], -32602, "arguments: {arguments}");
        }
    }

    #[tokio::test]
    async fn run_with_a_blank_command_is_invalid_params() {
        let state = test_state(1_000, None);
        for arguments in [r#"{}"#, r#"{"command":"   "}"#] {
            let reply = handle_line(
                &state,
                &format!(
                    r#"{{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{{"name":"compute.run","arguments":{arguments}}}}}"#
                ),
            )
            .await
            .unwrap();
            assert_eq!(reply["error"]["code"], -32602, "arguments: {arguments}");
        }
    }

    #[tokio::test]
    async fn run_refuses_a_price_above_the_ceiling_before_any_network() {
        let state = test_state(1_000, None);
        let reply = handle_line(
            &state,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"compute.run","arguments":{"command":"echo hi","price_micro_usdc":2000}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(reply["result"]["isError"], true);
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("ceiling"), "got: {text}");
    }

    #[tokio::test]
    async fn verify_with_malformed_arguments_is_invalid_params() {
        let state = test_state(1_000, None);
        for arguments in [r#"{}"#, r#"{"job_id":"not-a-uuid"}"#] {
            let reply = handle_line(
                &state,
                &format!(
                    r#"{{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{{"name":"compute.verify","arguments":{arguments}}}}}"#
                ),
            )
            .await
            .unwrap();
            assert_eq!(reply["error"]["code"], -32602, "arguments: {arguments}");
        }
    }

    #[tokio::test]
    async fn dispute_with_malformed_arguments_is_invalid_params() {
        let state = test_state(1_000, None);
        let reply = handle_line(
            &state,
            r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"compute.dispute","arguments":{"job_id":"not-a-uuid","reason":"bad"}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(reply["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn cancel_with_malformed_arguments_is_invalid_params() {
        let state = test_state(1_000, None);
        for arguments in [r#"{}"#, r#"{"job_id":"not-a-uuid"}"#, r#"{"job_id":42}"#] {
            let reply = handle_line(
                &state,
                &format!(
                    r#"{{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{{"name":"compute.cancel","arguments":{arguments}}}}}"#
                ),
            )
            .await
            .unwrap();
            assert_eq!(reply["error"]["code"], -32602, "for {arguments}");
        }
    }

    #[tokio::test]
    async fn deposit_without_a_deposit_id_is_invalid_params() {
        let state = test_state(1_000, None);
        let reply = handle_line(
            &state,
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"compute.deposit","arguments":{}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(reply["error"]["code"], -32602);
        let msg = reply["error"]["message"].as_str().unwrap();
        assert!(msg.contains("deposit_id"), "got: {msg}");
    }

    #[tokio::test]
    async fn unknown_method_is_a_json_rpc_error() {
        let state = test_state(1_000, None);
        let reply = handle_line(&state, r#"{"jsonrpc":"2.0","id":2,"method":"nope"}"#)
            .await
            .unwrap();
        assert_eq!(reply["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn malformed_json_is_a_parse_error() {
        let state = test_state(1_000, None);
        let reply = handle_line(&state, "{not json").await.unwrap();
        assert_eq!(reply["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn unknown_tool_is_invalid_params() {
        let state = test_state(1_000, None);
        let reply = handle_line(
            &state,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"other.tool","arguments":{}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(reply["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn over_cap_price_is_refused_as_a_tool_error_before_any_dispatch() {
        let state = test_state(100, None);
        let reply = handle_line(
            &state,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"compute.infer","arguments":{"prompt":"hi","price_micro_usdc":200}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(reply["result"]["isError"], true);
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("ceiling"), "got: {text}");
    }

    #[tokio::test]
    async fn session_spend_cap_refuses_before_any_dispatch() {
        let state = test_state(1_000, Some(500));
        state.caps.record_spend(400);
        let reply = handle_line(
            &state,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"compute.infer","arguments":{"prompt":"hi","price_micro_usdc":200}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(reply["result"]["isError"], true);
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("session spend cap"), "got: {text}");
    }

    /// The transport frame itself is attacker-reachable — a client (or
    /// a broken one) can send any bytes on the line. `handle_line` must
    /// answer a JSON-RPC error, stay silent on a notification, or fall
    /// through — never panic reaching for a field a malformed frame
    /// doesn't have. The per-tool argument refusals are pinned above;
    /// this pins the envelope around them.
    #[tokio::test]
    async fn the_transport_envelope_survives_malformed_frames() {
        let state = test_state(1_000, None);

        // Not a dispatchable request object: a parse error answered on
        // the null id (there is no id to echo yet).
        for line in [
            r#"{"jsonrpc":"2.0","id":1}"#,                   // no method
            r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#, // a batch, unsupported
            r#"{"jsonrpc":"2.0","id":1,"method":42}"#,       // method wrong type
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":[1,2}"#, // truncated
        ] {
            let reply = handle_line(&state, line).await.unwrap();
            assert_eq!(reply["error"]["code"], -32700, "line: {line}");
            assert!(reply["id"].is_null(), "line: {line}");
        }

        // No id is a notification: nothing is answered, whatever the
        // method — including a tools/call the server would otherwise run.
        for line in [
            r#"{"jsonrpc":"2.0","method":"ping"}"#,
            r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"compute.balance"}}"#,
            r#"{"jsonrpc":"2.0","id":null,"method":"tools/call","params":{"name":"compute.balance"}}"#,
        ] {
            assert!(handle_line(&state, line).await.is_none(), "line: {line}");
        }

        // A tools/call whose params are missing or the wrong type must
        // not panic reaching for name/arguments — it answers unknown
        // tool, echoing the caller's id.
        for params in [
            "",                         // absent (serde default -> Null)
            r#","params":"a string""#,  // a string
            r#","params":[1,2,3]"#,     // an array
            r#","params":{}"#,          // an object with no name
            r#","params":{"name":42}"#, // name the wrong type
        ] {
            let line = format!(r#"{{"jsonrpc":"2.0","id":7,"method":"tools/call"{params}}}"#);
            let reply = handle_line(&state, &line).await.unwrap();
            assert_eq!(reply["error"]["code"], -32602, "params: {params}");
            assert_eq!(reply["id"], 7, "params: {params}");
        }
    }

    /// `compute.withdrawals` routes to its handler (a result, never the
    /// unknown-tool JSON-RPC error) and maps a coordinator it can't reach
    /// to a tool error, not a panic — the read companion to
    /// compute.withdraw is dispatchable and wired to the buyer library.
    #[tokio::test]
    async fn withdrawals_routes_and_reports_an_unreachable_coordinator() {
        let state = test_state(1_000, None);
        let reply = handle_line(
            &state,
            r#"{"jsonrpc":"2.0","id":21,"method":"tools/call","params":{"name":"compute.withdrawals"}}"#,
        )
        .await
        .unwrap();
        assert_eq!(reply["id"], 21);
        assert!(
            reply["error"].is_null(),
            "routed, not unknown-tool: {reply}"
        );
        assert_eq!(reply["result"]["isError"], true);
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("withdrawal list failed"), "got: {text}");
    }
}
