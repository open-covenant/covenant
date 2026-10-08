//! Daemon glue for the Covenant Compute network's buyer side.
//!
//! The dispatch/verify client itself lives in
//! `covenant-compute-buyer` (shared with the standalone
//! `covenant-compute-mcp` server so the verification logic never
//! forks); this module is what the daemon adds on top: the
//! operator-configured spend policy, the `tool.call`-gated MCP tool
//! surface, and — for `compute.infer`, `compute.embed`,
//! `compute.transcribe`, `compute.speak`, `compute.run`, and
//! `compute.stream_start`, the tools that spend — the accounting
//! that lands the budget debit, `ResourceKind::Compute` settlement
//! receipt, and `ComputeJobDispatched` audit row against the agent that
//! invoked the tool — the same payer binding [`crate::hyre`] uses. The
//! first five differ only in what they buy (a prompt against a model,
//! text against an embedding model, speech against a transcription
//! model, text against a speech-synthesis model, or a command run to
//! completion on a batch node); everything from the price ceiling to the
//! verified-receipt-then-debit order is one shared path. `compute.stream_start`/`compute.stream_poll` split
//! that same purchase across a start/poll pair so an agent reads the
//! output as it generates: the debit still fires only when the drain
//! task verifies the receipt, never at poll time.
//! `compute.receipts`, `compute.deposit`, `compute.balance`,
//! `compute.capacity`, `compute.withdraw`, `compute.withdrawals`,
//! `compute.dispute`, `compute.cancel` and
//! `compute.verify` act for the daemon's own buyer identity and spend
//! nothing from any caller's budget: history, balance, the live-
//! capacity directory and the on-chain payout verification are reads,
//! a deposit claim only asks the coordinator to credit a payment that
//! already happened on-chain, a withdrawal moves the daemon's own
//! unspent deposit back to a wallet it names, and a dispute records a
//! signed reputation fault without touching the released escrow.
//!
//! No real USDC leaves the daemon on this path yet: the coordinator's
//! escrow is custodial and no inbound buyer payment rail exists, so the
//! daemon's accounting is the internal spend record. When a real
//! payment rail lands, `crate::x402::pay_and_record` is the model to
//! follow.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use covenant_a2a::A2ATaskStatus;
use covenant_audit::{AuditEvent, AuditKind, AuditLog};
use covenant_budget::{BudgetError, BudgetLedger};
use covenant_compute_buyer::{
    balance_tool_spec, cancel_job, cancel_tool_spec, capacity, capacity_tool_spec,
    cheapest_matching_ask, claim_deposit, deposit_tool_spec, dispatch_and_verify, dispatch_signed,
    dispute_job, dispute_tool_spec, embed_tool_spec, fetch_job_output, funds_with_deposit_info,
    infer_tool_spec, list_verified_jobs, list_withdrawals, output_tool_spec, preview_value,
    quote_price, receipts_tool_spec, run_tool_spec, save_speech_clip, sign_envelope,
    speak_tool_spec, stream_and_verify, stream_poll_tool_spec, stream_start_tool_spec,
    submit_streaming, transcribe_tool_spec, verify_payout, verify_tool_spec, withdraw,
    withdraw_tool_spec, withdrawals_tool_spec, BuyerConfig, BuyerError, CancelArgs,
    DispatchOutcome, DisputeArgs, EmbedArgs, InferArgs, JobKind, JobRequest, OutputArgs,
    PurchaseEntry, QuoteError, RunArgs, SignedJobEnvelope, SpeakArgs, SpeechResult, StreamJobs,
    StreamJobsError, StreamPollArgs, TranscribeArgs, VerifyArgs, WithdrawArgs,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::{Content, ToolCallResult, ToolSpec};
use covenant_settlement::Settlement;
use covenant_types::{AgentId, ResourceKind, SettlementReceipt};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::x402::SettlementContext;

pub use covenant_compute_buyer::{
    PurchaseBook, BALANCE_TOOL, CANCEL_TOOL, CAPACITY_TOOL, DEPOSIT_TOOL, DISPUTE_TOOL, EMBED_TOOL,
    INFER_TOOL, OUTPUT_TOOL, RECEIPTS_TOOL, RUN_TOOL, SPEAK_TOOL, STREAM_POLL_TOOL,
    STREAM_START_TOOL, TRANSCRIBE_TOOL, VERIFY_TOOL, WITHDRAWALS_TOOL, WITHDRAW_TOOL,
};

#[derive(Debug, Clone)]
pub struct ComputeConfig {
    /// Base URL of the compute coordinator, e.g. `http://127.0.0.1:8720`.
    pub coordinator_url: String,
    /// Per-call price ceiling in micro-USDC. A call offering more is
    /// refused; a call omitting a price offers exactly this.
    pub max_price_micro_usdc: u64,
    /// Job deadline when the caller doesn't pass one.
    pub default_deadline_ms: u64,
    /// How often the receipt endpoint is polled while a job runs.
    pub poll_interval: Duration,
    /// Demand-side partner attribution (C8), signed into every job
    /// this daemon dispatches. The partner's share comes out of the
    /// coordinator's fee — never on top of what this daemon pays.
    pub referral_code: Option<String>,
    /// This daemon's own Solana RPC endpoint for reading payout
    /// transactions back off the chain (`compute.verify`). Never taken
    /// from the coordinator — an endpoint the counterparty picks could
    /// vouch for its own transfers.
    pub rpc_url: Option<String>,
    /// How many `compute.stream_start` jobs one payer may have running
    /// at once. Bounds both the daemon's chunk buffers and the spend a
    /// payer can commit before the authoritative completion-time debits
    /// land.
    pub max_active_streams_per_payer: usize,
    /// Where `compute.speak` writes the audio it buys. A synthesized clip
    /// rides back base64 inside the receipt-verified output; the daemon
    /// writes it here and hands the caller the file's path in its place,
    /// so multi-megabyte audio never lands in the calling agent's context.
    pub clips_dir: PathBuf,
}

impl Default for ComputeConfig {
    fn default() -> Self {
        Self {
            coordinator_url: String::new(),
            max_price_micro_usdc: 1_000_000, // $1
            default_deadline_ms: 60_000,
            poll_interval: Duration::from_millis(500),
            referral_code: None,
            rpc_url: None,
            max_active_streams_per_payer: 4,
            clips_dir: std::env::temp_dir().join("covenant-compute-clips"),
        }
    }
}

/// Coordinator endpoint + spend policy, built once at daemon startup
/// and shared behind an `Arc`, mirroring [`crate::hyre::HyreState`].
pub struct ComputeState {
    pub config: ComputeConfig,
    /// In-flight and recently concluded `compute.stream_start` jobs,
    /// keyed to their payer. In-memory only — a daemon restart degrades
    /// a streaming caller to re-dispatching, never to losing money (the
    /// debit only ever lands with a verified receipt).
    pub streams: StreamJobs<Vec<Content>>,
    /// Idempotent purchases, keyed `<payer_pubkey>:<caller key>`. The
    /// daemon journals the signed envelope here before its first
    /// submission, so a call retried after a crash re-drives the same
    /// job instead of buying a second one.
    pub purchases: Arc<PurchaseBook>,
    /// One async lock per purchase key with a drive in flight. The
    /// book makes a key name one purchase; this makes one CALL at a
    /// time drive it — two racing retries would otherwise both find
    /// the journaled envelope unconcluded, both drive it to the same
    /// coordinator receipt, and each book its own debit and settlement
    /// row for the one purchase.
    drives: std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl ComputeState {
    pub fn new(config: ComputeConfig) -> Self {
        let streams = StreamJobs::new(config.max_active_streams_per_payer);
        Self {
            config,
            streams,
            purchases: Arc::new(PurchaseBook::in_memory()),
            drives: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// The serialization handle for one key's drive. Await its lock
    /// before touching the key's book entry; racers queue here and
    /// find the concluded entry when their turn comes.
    fn drive_permit(&self, scoped_key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut drives = self.drives.lock().expect("drive map poisoned");
        drives.entry(scoped_key.to_string()).or_default().clone()
    }

    /// Drops a key's handle once nobody is driving or queued — the map
    /// stays bounded by keys actually in flight, not keys ever used.
    /// Callers drop their own clone first, so the map's reference is
    /// the only one left exactly when no racer holds or awaits the
    /// lock; removing it any earlier would hand the next retry a
    /// second lock while a queued racer still drives under the first.
    fn release_drive(&self, scoped_key: &str) {
        let mut drives = self.drives.lock().expect("drive map poisoned");
        if let Some(permit) = drives.get(scoped_key) {
            if Arc::strong_count(permit) == 1 {
                drives.remove(scoped_key);
            }
        }
    }

    /// Swaps in a durable purchase book — the real daemon's form, so
    /// idempotency keys keep their meaning across restarts. The
    /// default in-memory book protects only within one process life.
    pub fn with_purchase_book(mut self, purchases: Arc<PurchaseBook>) -> Self {
        self.purchases = purchases;
        self
    }
}

/// Owned handles to the daemon's accounting subsystems, for the drain
/// task that outlives its originating `tools/call`. The debit fires at
/// job completion inside that task — never at poll time, so a caller
/// that stops polling still pays for work an operator verifiably did.
pub struct StreamAccounting {
    pub settlement: Arc<dyn Settlement>,
    pub audit: Arc<dyn AuditLog>,
    pub budget: Arc<dyn BudgetLedger>,
    pub issuer: AgentId,
}

/// The advertised buyer surface, identical to the standalone MCP
/// server's: dispatch (inference and batch), history, top-up, funds,
/// dispute. Kept as a function of config so the dispatch descriptions
/// state the live ceiling instead of a stale constant.
pub fn compute_specs(config: &ComputeConfig) -> Vec<ToolSpec> {
    vec![
        infer_tool_spec(config.max_price_micro_usdc),
        embed_tool_spec(config.max_price_micro_usdc),
        transcribe_tool_spec(config.max_price_micro_usdc),
        speak_tool_spec(config.max_price_micro_usdc),
        run_tool_spec(config.max_price_micro_usdc),
        stream_start_tool_spec(config.max_price_micro_usdc),
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
    ]
}

#[derive(Debug, thiserror::Error)]
pub enum ComputeError {
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),
    #[error(
        "idempotency key {key:?} was already used with a different {argument}: a key names one \
         purchase, so repeat its original arguments to retrieve it or use a fresh key for new work"
    )]
    KeyedConflict { key: String, argument: &'static str },
    #[error("offered price {asked} micro-USDC exceeds the per-call ceiling {cap}")]
    PriceCap { asked: u64, cap: u64 },
    /// A `dry_run` preview could not name a price a buy would truly pay —
    /// no operator serves the job, or the cheapest ask is over the ceiling.
    #[error(transparent)]
    Quote(#[from] QuoteError),
    #[error("payer has no budget capacity; refusing to spend")]
    NoCapacity,
    #[error("payer budget would be exceeded by this call")]
    BudgetExceeded,
    #[error("budget: {0}")]
    Budget(BudgetError),
    #[error(transparent)]
    Buyer(#[from] BuyerError),
    #[error("job {job_id} executed with status {status}; not charging the caller")]
    JobFailed { job_id: Uuid, status: String },
    #[error("accounting: {0}")]
    Accounting(String),
    #[error(transparent)]
    Stream(#[from] StreamJobsError),
    /// A streaming job's drain task concluded with this error; the poll
    /// re-surfaces it verbatim, so the caller reads exactly what the
    /// synchronous call would have returned.
    #[error("{0}")]
    StreamFailed(String),
}

/// USD-pegged budget credits (cents) for a job price, ceiling-rounded.
/// Hyre's floor division would make a sub-cent job free — acceptable
/// there because real USDC still moves per call; on this path the
/// budget debit is the only spend cap, so round up instead.
pub fn credits_for_price(price_micro_usdc: u64) -> u64 {
    price_micro_usdc.div_ceil(10_000)
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn http_client() -> reqwest::Client {
    // The buyer crate's client: dispatch-sized timeouts plus the wire
    // version stamped on every request, so a coordinator with a raised
    // floor refuses this daemon by name instead of failing a parse.
    covenant_compute_buyer::http_client()
}

fn buyer_config(config: &ComputeConfig) -> BuyerConfig {
    BuyerConfig {
        coordinator_url: config.coordinator_url.clone(),
        poll_interval: config.poll_interval,
        referral_code: config.referral_code.clone(),
        rpc_url: config.rpc_url.clone(),
    }
}

/// Records the budget debit, settlement receipt, and audit row for one
/// verified compute job, in [`crate::x402::record_paid_call`]'s
/// debit -> receipt -> audit order so the logs never carry a
/// half-recorded call. Returns the shared receipt id.
async fn record_compute_job(
    ctx: &SettlementContext<'_>,
    payer: &AgentId,
    outcome: &DispatchOutcome,
    credits: u64,
) -> Result<Uuid, ComputeError> {
    let receipt_id = Uuid::new_v4();
    let now = epoch_ms();

    ctx.budget
        .try_debit(payer, credits, receipt_id)
        .await
        .map_err(ComputeError::Budget)?;

    ctx.settlement
        .record(SettlementReceipt {
            id: receipt_id,
            payer: payer.clone(),
            resource: ResourceKind::Compute,
            memory_record_id: None,
            credits_consumed: credits,
            settled_at: now,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        })
        .await
        .map_err(|e| ComputeError::Accounting(e.to_string()))?;

    let receipt = &outcome.receipt.receipt;
    ctx.audit
        .record(AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: now,
            issuer: ctx.issuer.clone(),
            kind: AuditKind::ComputeJobDispatched {
                job_id: receipt.job_id,
                operator_pubkey_b58: receipt.operator.pubkey_base58(),
                status: "ok".into(),
                result_hash_hex: receipt.result_hash_hex.clone(),
                price_micro_usdc: receipt.price_micro_usdc,
                receipt_id,
            },
        })
        .await
        .map_err(|e| ComputeError::Accounting(e.to_string()))?;

    debug!(
        job_id = %receipt.job_id,
        credits,
        %receipt_id,
        "recorded compute job dispatch"
    );
    Ok(receipt_id)
}

/// The `compute.infer` call bound to one payer: parse, then the shared
/// paid-dispatch path below.
pub async fn run_infer_call(
    state: &ComputeState,
    ctx: &SettlementContext<'_>,
    buyer_identity: &LocalIdentity,
    payer: &AgentId,
    arguments: serde_json::Value,
) -> Result<ToolCallResult, ComputeError> {
    let args: InferArgs =
        serde_json::from_value(arguments).map_err(|e| ComputeError::InvalidArgs(e.to_string()))?;
    let input = args.input().map_err(ComputeError::InvalidArgs)?;
    dispatch_paid_job(
        state,
        ctx,
        buyer_identity,
        payer,
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
    )
    .await
}

/// The `compute.embed` call: text vectorized on an embedding operator,
/// on the exact spend path `compute.infer` uses. The vector rides back
/// as the tool result, bound by the same signed, hash-verified receipt.
pub async fn run_embed_call(
    state: &ComputeState,
    ctx: &SettlementContext<'_>,
    buyer_identity: &LocalIdentity,
    payer: &AgentId,
    arguments: serde_json::Value,
) -> Result<ToolCallResult, ComputeError> {
    let args: EmbedArgs =
        serde_json::from_value(arguments).map_err(|e| ComputeError::InvalidArgs(e.to_string()))?;
    let input = args.input().map_err(ComputeError::InvalidArgs)?;
    dispatch_paid_job(
        state,
        ctx,
        buyer_identity,
        payer,
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
    )
    .await
}

/// The `compute.transcribe` call: speech turned to text on a whisper
/// operator, on the exact spend path `compute.infer` uses. The audio
/// rides in as base64, the transcript rides back as the tool result,
/// bound by the same signed, hash-verified receipt.
pub async fn run_transcribe_call(
    state: &ComputeState,
    ctx: &SettlementContext<'_>,
    buyer_identity: &LocalIdentity,
    payer: &AgentId,
    arguments: serde_json::Value,
) -> Result<ToolCallResult, ComputeError> {
    let args: TranscribeArgs =
        serde_json::from_value(arguments).map_err(|e| ComputeError::InvalidArgs(e.to_string()))?;
    let input = args.input().map_err(ComputeError::InvalidArgs)?;
    dispatch_paid_job(
        state,
        ctx,
        buyer_identity,
        payer,
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
    )
    .await
}

/// The `compute.speak` call: text turned to speech on a synthesis
/// operator, on the exact spend path `compute.infer` uses. The words
/// ride in as text, the operator's audio rides back base64 inside the
/// receipt-verified output — but base64 audio does not belong in an
/// agent's context, so the daemon writes the clip to its clips directory
/// and returns the file's path, size and shape in the audio's place. The
/// receipt block still rides back, so a buyer always learns what they
/// paid for.
pub async fn run_speak_call(
    state: &ComputeState,
    ctx: &SettlementContext<'_>,
    buyer_identity: &LocalIdentity,
    payer: &AgentId,
    arguments: serde_json::Value,
) -> Result<ToolCallResult, ComputeError> {
    let args: SpeakArgs =
        serde_json::from_value(arguments).map_err(|e| ComputeError::InvalidArgs(e.to_string()))?;
    let input = args.input().map_err(ComputeError::InvalidArgs)?;
    let result = dispatch_paid_job(
        state,
        ctx,
        buyer_identity,
        payer,
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
    )
    .await?;
    Ok(render_speech_clip(&state.config.clips_dir, result))
}

/// Rewrites a synthesized job's result so its base64 audio becomes a clip
/// the caller can play, naming the clip for the job the receipt block
/// carries. Every other block — the receipt metadata [`paid_response`]
/// appends included — rides through untouched.
fn render_speech_clip(dir: &Path, mut result: ToolCallResult) -> ToolCallResult {
    // Name the clip for its job, the way the CLI and MCP surfaces do, so
    // a re-driven idempotent call rewrites the same file. The job id
    // rides in the receipt block `paid_response` appended; a settled
    // speak always carries it, so the fresh id is only a defensive floor
    // that still keeps the audio off the wire.
    let job_id = result
        .content
        .iter()
        .find_map(|c| match c {
            Content::Json { value } => value
                .get("job_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|s| Uuid::parse_str(s).ok()),
            _ => None,
        })
        .unwrap_or_else(Uuid::new_v4);
    save_speech_block(dir, &mut result.content, job_id);
    result
}

/// Rewrites a synthesized job's base64 audio, in place, into a block
/// naming the clip written to `dir` — its path, byte count and container.
/// Base64 audio does not belong in an agent's context, so both the buy
/// (`compute.speak`) and the re-read (`compute.output`) drop it here the
/// moment they hand a synthesized job back. A dry-run preview or any
/// non-speech job carries no such block and is left untouched. A directory
/// or write failure lands as a readable `saved:false` block rather than a
/// silent drop, and the base64 is dropped either way so it never reaches
/// the agent.
fn save_speech_block(dir: &Path, blocks: &mut [Content], job_id: Uuid) {
    let Some((idx, speech)) = blocks.iter().enumerate().find_map(|(i, c)| match c {
        Content::Json { value } if value.get("audio_base64").is_some() => {
            serde_json::from_value::<SpeechResult>(value.clone())
                .ok()
                .map(|s| (i, s))
        }
        _ => None,
    }) else {
        return;
    };
    let saved = std::fs::create_dir_all(dir)
        .map_err(|e| format!("create {}: {e}", dir.display()))
        .and_then(|()| save_speech_clip(dir, &speech, job_id));
    let described = match saved {
        Ok((path, bytes)) => serde_json::json!({
            "saved": true,
            "path": path.display().to_string(),
            "bytes": bytes,
            "format": speech.format,
            "sample_rate_hz": speech.sample_rate_hz,
            "model": speech.model,
        }),
        Err(e) => serde_json::json!({
            "saved": false,
            "error": e,
            "format": speech.format,
            "model": speech.model,
        }),
    };
    blocks[idx] = Content::json(described);
}

/// The `compute.run` call: one command bought as a batch job, on the
/// exact spend path `compute.infer` uses. What the command can touch
/// is the executing node's sandbox policy; what it can cost this payer
/// is decided here.
pub async fn run_batch_call(
    state: &ComputeState,
    ctx: &SettlementContext<'_>,
    buyer_identity: &LocalIdentity,
    payer: &AgentId,
    arguments: serde_json::Value,
) -> Result<ToolCallResult, ComputeError> {
    let args: RunArgs =
        serde_json::from_value(arguments).map_err(|e| ComputeError::InvalidArgs(e.to_string()))?;
    let input = args.input().map_err(ComputeError::InvalidArgs)?;
    dispatch_paid_job(
        state,
        ctx,
        buyer_identity,
        payer,
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
    )
    .await
}

/// The `compute.stream_start` call: `compute.infer`'s front half bound
/// to one payer — parse, price ceiling, budget pre-check (counting the
/// payer's other in-flight streaming offers), register in the stream
/// ledger, submit with the envelope's `stream` flag signed — then hand
/// the drain to a background task and return the job id immediately.
/// The task relays chunks into the ledger and, on a verified `Ok`
/// receipt, lands the exact debit/settlement/audit row the synchronous
/// path records; its terminal payload (or failure) is what
/// `compute.stream_poll` serves. A submit failure rolls the
/// registration back entirely.
pub async fn run_stream_start_call(
    state: &Arc<ComputeState>,
    accounting: StreamAccounting,
    buyer_identity: Arc<LocalIdentity>,
    payer: &AgentId,
    arguments: serde_json::Value,
) -> Result<ToolCallResult, ComputeError> {
    let args: InferArgs =
        serde_json::from_value(arguments).map_err(|e| ComputeError::InvalidArgs(e.to_string()))?;
    // Streaming returns a job_id before the drain settles, so it can't
    // dedupe on a key the way compute.infer does. Refuse the key rather
    // than accept it and quietly pay twice on a retry.
    if args.idempotency_key.is_some() {
        return Err(ComputeError::InvalidArgs(format!(
            "{STREAM_START_TOOL} can't honor idempotency_key: it returns a job_id before payment \
             settles, so it can't guarantee exactly-once. Use {INFER_TOOL} for an idempotent buy, \
             or omit the key to stream."
        )));
    }
    // A stream is a live feed to open, not a figure to quote. Point a
    // caller wanting the cost at the synchronous tool's dry run.
    if args.dry_run {
        return Err(ComputeError::InvalidArgs(format!(
            "{STREAM_START_TOOL} opens a live feed and has nothing to preview — call {INFER_TOOL} \
             with dry_run to see the price and routing first, then stream."
        )));
    }
    let input = args.input().map_err(ComputeError::InvalidArgs)?;

    let cap = state.config.max_price_micro_usdc;
    let price = resolve_offer(
        state,
        args.price_micro_usdc,
        JobKind::InferenceCall,
        args.model.as_deref(),
        args.gpu_class.as_deref(),
        args.min_vram_gb,
        args.min_reputation_bps,
        cap,
    )
    .await;
    if price > cap {
        return Err(ComputeError::PriceCap { asked: price, cap });
    }
    let deadline_ms = args.deadline_ms.unwrap_or(state.config.default_deadline_ms);
    let credits = credits_for_price(price);
    let owner = payer.pubkey_base58();

    let pending = state.streams.active_committed(&owner);
    match accounting
        .budget
        .would_exceed(payer, pending.saturating_add(credits))
        .await
    {
        Ok(false) => {}
        Ok(true) => return Err(ComputeError::BudgetExceeded),
        Err(BudgetError::NoCapacity(_)) => return Err(ComputeError::NoCapacity),
        Err(e) => return Err(ComputeError::Budget(e)),
    }

    let job_id = Uuid::new_v4();
    state.streams.try_start(&owner, job_id, credits)?;
    let envelope = match submit_streaming(
        &http_client(),
        &buyer_config(&state.config),
        &buyer_identity,
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
            return Err(e.into());
        }
    };

    let task_state = state.clone();
    let payer = payer.clone();
    tokio::spawn(async move {
        let work = tokio::spawn({
            let state = task_state.clone();
            async move { drain_and_record(&state, &accounting, &buyer_identity, &payer, envelope).await }
        });
        let outcome = match work.await {
            Ok(outcome) => outcome,
            // A panic in the drain must still free the payer's slot and
            // tell the poller something true.
            Err(e) => Err(format!("stream drain task died: {e}")),
        };
        task_state.streams.conclude(job_id, outcome);
    });

    Ok(ToolCallResult::ok(vec![Content::json(serde_json::json!({
        "job_id": job_id,
        "status": "streaming",
        "next_seq": 0,
        "price_micro_usdc": price,
        "credits": credits,
        "poll_tool": STREAM_POLL_TOOL,
    }))]))
}

/// The drain half of one streaming job, run in its own task: relay
/// every chunk into the ledger, verify the receipt exactly as the
/// synchronous path does, then debit and record. Returns the same
/// content `compute.infer` would have — the poll's terminal payload —
/// or the error string the synchronous call would have failed with.
async fn drain_and_record(
    state: &ComputeState,
    accounting: &StreamAccounting,
    buyer_identity: &LocalIdentity,
    payer: &AgentId,
    envelope: SignedJobEnvelope,
) -> Result<Vec<Content>, String> {
    let job_id = envelope.payload.job_id;
    let credits = credits_for_price(envelope.payload.price_micro_usdc);
    let streamed = stream_and_verify(
        &http_client(),
        &buyer_config(&state.config),
        buyer_identity,
        envelope,
        |chunk| state.streams.append_chunk(job_id, chunk),
    )
    .await
    .map_err(|e| ComputeError::from(e).to_string())?;

    let outcome = streamed.outcome;
    let receipt = &outcome.receipt.receipt;
    if receipt.status != A2ATaskStatus::Ok {
        let status = match receipt.status {
            A2ATaskStatus::Ok => "ok",
            A2ATaskStatus::Error => "error",
            A2ATaskStatus::Partial => "partial",
        };
        return Err(ComputeError::JobFailed {
            job_id: receipt.job_id,
            status: status.into(),
        }
        .to_string());
    }

    let ctx = SettlementContext {
        settlement: accounting.settlement.as_ref(),
        audit: accounting.audit.as_ref(),
        budget: accounting.budget.as_ref(),
        issuer: &accounting.issuer,
    };
    let receipt_id = match record_compute_job(&ctx, payer, &outcome, credits).await {
        Ok(id) => id,
        Err(e) => {
            // The job already executed and escrow released on the
            // network side; a local accounting gap must surface loudly.
            warn!(error = %e, job_id = %receipt.job_id, "streamed compute job succeeded but accounting failed");
            return Err(e.to_string());
        }
    };

    let mut content = outcome.output.clone();
    content.push(Content::json(serde_json::json!({
        "job_id": receipt.job_id,
        "receipt_id": receipt_id,
        "operator_pubkey_b58": receipt.operator.pubkey_base58(),
        "price_micro_usdc": receipt.price_micro_usdc,
        "credits": credits,
        "result_hash_hex": receipt.result_hash_hex,
        "wall_ms": receipt.meter.wall_ms,
        // Whether the live feed, assembled, equals the verified final
        // output. False grades only the preview — the output above is
        // receipt-verified either way.
        "stream_matched_output": streamed.stream_matched_output,
        // On-chain pointer when the payout push had already landed by
        // receipt time; null while it's still in flight (re-check via
        // compute.receipts).
        "payout": outcome.payout,
    })));
    Ok(content)
}

/// The `compute.stream_poll` call: an owner-checked cursor read of one
/// streaming job. While the job runs it returns new chunks and status
/// `streaming`; the concluding poll carries the verified output and
/// receipt block exactly as `compute.infer` returns them, and a job
/// whose drain failed fails the poll with the original error. Nothing
/// is debited here — the drain task already settled the money.
pub async fn run_stream_poll_call(
    state: &ComputeState,
    payer: &AgentId,
    arguments: serde_json::Value,
) -> Result<ToolCallResult, ComputeError> {
    let args: StreamPollArgs =
        serde_json::from_value(arguments).map_err(|e| ComputeError::InvalidArgs(e.to_string()))?;
    let page = state
        .streams
        .poll(&payer.pubkey_base58(), args.job_id, args.since)?;
    let status = match &page.outcome {
        None => "streaming",
        Some(Ok(_)) => "completed",
        Some(Err(_)) => "failed",
    };
    let mut content = vec![Content::json(serde_json::json!({
        "job_id": args.job_id,
        "status": status,
        "chunks": page.chunks,
        "next_seq": page.next_seq,
    }))];
    match page.outcome {
        None => Ok(ToolCallResult::ok(content)),
        Some(Ok(final_content)) => {
            content.extend(final_content);
            Ok(ToolCallResult::ok(content))
        }
        Some(Err(message)) => Err(ComputeError::StreamFailed(message)),
    }
}

/// The price to offer for one job: an explicit `price_micro_usdc` wins;
/// otherwise the cheapest matching ask the coordinator advertises for
/// this `(kind, model)`, capped by the per-call ceiling. Settlement
/// charges the envelope's price, so defaulting an unpriced call to the
/// ceiling hands an operator asking far less its full cap — the same
/// overpay the buyer CLI and MCP tools already avoid. A market read that
/// fails or finds nothing falls back to `cap`, so no call the old default
/// allowed is refused and a keyed replay never depends on capacity.
#[allow(clippy::too_many_arguments)]
async fn resolve_offer(
    state: &ComputeState,
    price_micro_usdc: Option<u64>,
    kind: JobKind,
    model: Option<&str>,
    gpu_class: Option<&str>,
    min_vram_gb: Option<u32>,
    min_reputation_bps: Option<u32>,
    cap: u64,
) -> u64 {
    match price_micro_usdc {
        Some(p) => p,
        None => cheapest_matching_ask(
            &http_client(),
            &buyer_config(&state.config),
            kind,
            model,
            gpu_class,
            min_vram_gb,
            min_reputation_bps,
        )
        .await
        .ok()
        .flatten()
        .map(|floor| floor.min(cap))
        .unwrap_or(cap),
    }
}

/// The one paid-dispatch path both spending tools ride: enforce the
/// price ceiling, pre-check the budget read-only (so the daemon never
/// dispatches a job the caller can't afford), dispatch, verify, then
/// debit and record. The authoritative debit happens only after a
/// verified `Ok` receipt — a refunded, timed-out, or failed job never
/// charges the caller.
#[allow(clippy::too_many_arguments)]
async fn dispatch_paid_job(
    state: &ComputeState,
    ctx: &SettlementContext<'_>,
    buyer_identity: &LocalIdentity,
    payer: &AgentId,
    kind: JobKind,
    input: Vec<Content>,
    model: Option<String>,
    gpu_class: Option<String>,
    min_vram_gb: Option<u32>,
    min_reputation_bps: Option<u32>,
    price_micro_usdc: Option<u64>,
    deadline_ms: Option<u64>,
    idempotency_key: Option<String>,
    dry_run: bool,
) -> Result<ToolCallResult, ComputeError> {
    let cap = state.config.max_price_micro_usdc;
    // A preview resolves the price and routing a real buy would use and
    // returns them without dispatching or debiting. It reserves no
    // purchase, so an idempotency key has nothing to bind; and it refuses
    // an unservable ask up front instead of the ceiling fallback a real
    // agent-side buy takes, so what it quotes is a price a buy would pay.
    if dry_run {
        if idempotency_key.is_some() {
            return Err(ComputeError::InvalidArgs(
                "a dry run reserves no purchase, so it takes no idempotency_key — drop the key to \
                 preview, or drop dry_run to buy"
                    .into(),
            ));
        }
        let quote = quote_price(
            &http_client(),
            &buyer_config(&state.config),
            kind,
            model.as_deref(),
            gpu_class.as_deref(),
            min_vram_gb,
            min_reputation_bps,
            price_micro_usdc,
            cap,
        )
        .await?;
        let deadline_ms = deadline_ms.unwrap_or(state.config.default_deadline_ms);
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
        return Ok(ToolCallResult::ok(vec![Content::json(preview)]));
    }
    let price = resolve_offer(
        state,
        price_micro_usdc,
        kind,
        model.as_deref(),
        gpu_class.as_deref(),
        min_vram_gb,
        min_reputation_bps,
        cap,
    )
    .await;
    if price > cap {
        return Err(ComputeError::PriceCap { asked: price, cap });
    }
    let deadline_arg = deadline_ms;
    let deadline_ms = deadline_ms.unwrap_or(state.config.default_deadline_ms);

    // A keyed call is exactly-once per payer and key: the signed
    // envelope journals in the purchase book before its first
    // submission, so a retry — same process or the next one — re-drives
    // the same job through the coordinator's duplicate detection
    // instead of buying a second one.
    if let Some(key) = idempotency_key {
        if key.is_empty() || key.len() > 128 {
            return Err(ComputeError::InvalidArgs(
                "idempotency_key must be 1..=128 bytes".into(),
            ));
        }
        let scoped = format!("{}:{key}", payer.pubkey_base58());
        // One driver per key at a time. A retry racing the original —
        // an agent runtime firing the same call from two places — must
        // wait for the conclusion and serve the recorded purchase, not
        // re-drive the same envelope into a second debit.
        let permit = state.drive_permit(&scoped);
        let concluded = {
            let _driving = permit.lock().await;
            drive_keyed_purchase(
                state,
                ctx,
                buyer_identity,
                payer,
                kind,
                input,
                model,
                gpu_class,
                min_vram_gb,
                min_reputation_bps,
                price_micro_usdc,
                deadline_arg,
                price,
                deadline_ms,
                scoped.clone(),
            )
            .await
        };
        drop(permit);
        state.release_drive(&scoped);
        return concluded;
    }

    check_budget(state, ctx, payer, credits_for_price(price)).await?;
    let outcome = dispatch_and_verify(
        &http_client(),
        &buyer_config(&state.config),
        buyer_identity,
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
    .await?;
    settle_paid_outcome(ctx, payer, outcome)
        .await
        .map(|(_, response)| response)
}

/// The keyed branch's body, one caller at a time per key (the drive
/// permit in [`dispatch_paid_job`]): replay a settled entry, conclude
/// an unconcluded one, or journal and conclude a fresh purchase.
#[allow(clippy::too_many_arguments)]
async fn drive_keyed_purchase(
    state: &ComputeState,
    ctx: &SettlementContext<'_>,
    buyer_identity: &LocalIdentity,
    payer: &AgentId,
    kind: JobKind,
    input: Vec<Content>,
    model: Option<String>,
    gpu_class: Option<String>,
    min_vram_gb: Option<u32>,
    min_reputation_bps: Option<u32>,
    price_micro_usdc: Option<u64>,
    deadline_arg: Option<u64>,
    price: u64,
    deadline_ms: u64,
    scoped: String,
) -> Result<ToolCallResult, ComputeError> {
    if let Some(entry) = state.purchases.lookup(&scoped) {
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
            price_micro_usdc,
            deadline_arg,
        ) {
            let key = scoped
                .split_once(':')
                .map_or(scoped.as_str(), |(_, k)| k)
                .to_string();
            return Err(ComputeError::KeyedConflict { key, argument });
        }
        if let Some(receipt_id) = entry.receipt_id {
            // Already paid and returned once: serve the recorded
            // purchase again. The coordinator answers the replayed
            // envelope with the concluded job, the receipt
            // re-verifies locally, and nothing is debited — that
            // happened with the first return.
            let outcome = dispatch_signed(
                &http_client(),
                &buyer_config(&state.config),
                buyer_identity,
                entry.envelope,
            )
            .await?;
            // The envelope price is what was charged on the first return
            // (see `settle_paid_outcome`), so a replay reports the same.
            let credits = credits_for_price(outcome.envelope.payload.price_micro_usdc);
            return Ok(paid_response(&outcome, receipt_id, credits));
        }
        // Journaled but never concluded — a previous life died
        // mid-purchase. Drive the same envelope to its conclusion.
        return conclude_keyed_purchase(state, ctx, buyer_identity, payer, scoped, entry.envelope)
            .await;
    }
    let envelope = sign_envelope(
        &buyer_config(&state.config),
        buyer_identity,
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
    )?;
    state
        .purchases
        .record(PurchaseEntry {
            key: scoped.clone(),
            envelope: envelope.clone(),
            opened_at_ms: epoch_ms(),
            receipt_id: None,
            voided: false,
        })
        .map_err(|e| ComputeError::Accounting(e.to_string()))?;
    conclude_keyed_purchase(state, ctx, buyer_identity, payer, scoped, envelope).await
}

/// Streaming jobs debit at completion, so the ledger can't see them
/// yet; count their offers here or a payer could overcommit through
/// the two paths at once.
async fn check_budget(
    state: &ComputeState,
    ctx: &SettlementContext<'_>,
    payer: &AgentId,
    credits: u64,
) -> Result<(), ComputeError> {
    let pending = state
        .streams
        .active_committed(&payer.pubkey_base58())
        .saturating_add(credits);
    match ctx.budget.would_exceed(payer, pending).await {
        Ok(false) => Ok(()),
        Ok(true) => Err(ComputeError::BudgetExceeded),
        Err(BudgetError::NoCapacity(_)) => Err(ComputeError::NoCapacity),
        Err(e) => Err(ComputeError::Budget(e)),
    }
}

/// Drives a journaled purchase to a conclusion and keeps the book
/// honest about it: a paid return settles the key, a job that
/// concluded unpaid frees it, and anything ambiguous — transport
/// failure, receipt timeout, an answered refusal that may heal (a
/// topped-up balance, a not-yet-expired envelope) — leaves the entry
/// in flight for the next retry to resolve.
async fn conclude_keyed_purchase(
    state: &ComputeState,
    ctx: &SettlementContext<'_>,
    buyer_identity: &LocalIdentity,
    payer: &AgentId,
    scoped_key: String,
    envelope: SignedJobEnvelope,
) -> Result<ToolCallResult, ComputeError> {
    // Price and credits come from the journaled envelope — on a retry
    // it is the purchase of record, whatever the arguments now say.
    check_budget(
        state,
        ctx,
        payer,
        credits_for_price(envelope.payload.price_micro_usdc),
    )
    .await?;
    match dispatch_signed(
        &http_client(),
        &buyer_config(&state.config),
        buyer_identity,
        envelope,
    )
    .await
    {
        Ok(outcome) => match settle_paid_outcome(ctx, payer, outcome).await {
            Ok((receipt_id, response)) => {
                // Settle after the debit, mirroring the node's
                // credit-before-tombstone: a crash between the two
                // re-debits an hourly-refilling budget bucket at worst
                // — never the buyer's real funds, which the
                // coordinator settles exactly once.
                if let Err(e) = state.purchases.settle(&scoped_key, receipt_id) {
                    warn!(key = %scoped_key, error = %e, "purchase book settle failed");
                }
                Ok(response)
            }
            Err(e @ ComputeError::JobFailed { .. }) => {
                // The job ran and concluded unpaid (the hold
                // refunded); the key frees so a retry may honestly
                // buy again.
                if let Err(ve) = state.purchases.void(&scoped_key) {
                    warn!(key = %scoped_key, error = %ve, "purchase book void failed");
                }
                Err(e)
            }
            // A debit or accounting failure: the work is paid for on
            // the network side but not yet booked here — the entry
            // stays in flight so a retry re-drives into the books.
            Err(e) => Err(e),
        },
        // Refunded, rejected, failed — or refused at submission with a
        // verdict on the envelope itself (e.g. 400 past-deadline, the
        // fate of any envelope journaled by a life that died before
        // submitting and retried late): the money provably never
        // moved, the key frees. Without this a permanently-refused
        // envelope would wedge its key forever.
        Err(e) if e.concludes_purchase_unpaid() => {
            if let Err(ve) = state.purchases.void(&scoped_key) {
                warn!(key = %scoped_key, error = %ve, "purchase book void failed");
            }
            Err(e.into())
        }
        Err(e) => Err(e.into()),
    }
}

/// The unchanged back half of every synchronous paid dispatch: refuse
/// to charge for a non-Ok receipt, then debit, record, and shape the
/// tool response. Returns the spend-side receipt id alongside the
/// response so a keyed purchase can settle its book entry.
async fn settle_paid_outcome(
    ctx: &SettlementContext<'_>,
    payer: &AgentId,
    outcome: DispatchOutcome,
) -> Result<(Uuid, ToolCallResult), ComputeError> {
    let receipt = &outcome.receipt.receipt;
    if receipt.status != A2ATaskStatus::Ok {
        let status = match receipt.status {
            A2ATaskStatus::Ok => "ok",
            A2ATaskStatus::Error => "error",
            A2ATaskStatus::Partial => "partial",
        };
        return Err(ComputeError::JobFailed {
            job_id: receipt.job_id,
            status: status.into(),
        });
    }

    // Debit what the buyer is actually charged: the coordinator settles
    // the held envelope price, and a receipt price (verified never larger)
    // would under-charge the payer's budget — the only spend cap on this
    // path. The streaming sibling already debits the envelope price.
    let credits = credits_for_price(outcome.envelope.payload.price_micro_usdc);
    let receipt_id = match record_compute_job(ctx, payer, &outcome, credits).await {
        Ok(id) => id,
        Err(e) => {
            // The job already executed and escrow released on the
            // network side; a local accounting gap must surface loudly.
            warn!(error = %e, job_id = %receipt.job_id, "compute job succeeded but accounting failed");
            return Err(e);
        }
    };
    Ok((receipt_id, paid_response(&outcome, receipt_id, credits)))
}

fn paid_response(outcome: &DispatchOutcome, receipt_id: Uuid, credits: u64) -> ToolCallResult {
    let receipt = &outcome.receipt.receipt;
    let mut content = outcome.output.clone();
    content.push(Content::json(serde_json::json!({
        "job_id": receipt.job_id,
        "receipt_id": receipt_id,
        "operator_pubkey_b58": receipt.operator.pubkey_base58(),
        "price_micro_usdc": receipt.price_micro_usdc,
        "credits": credits,
        "result_hash_hex": receipt.result_hash_hex,
        "wall_ms": receipt.meter.wall_ms,
        // On-chain pointer when the payout push had already landed by
        // receipt time; null while it's still in flight (re-check via
        // compute.receipts).
        "payout": outcome.payout,
    })));
    ToolCallResult::ok(content)
}

/// `compute.receipts` (A4): this daemon identity's job history from
/// the coordinator, every receipt re-verified locally before it is
/// shown; a failed verification is surfaced on its row, never dropped.
/// Read-only — nothing is debited or recorded.
pub async fn run_receipts_call(
    state: &ComputeState,
    buyer_identity: &LocalIdentity,
    arguments: serde_json::Value,
) -> Result<ToolCallResult, ComputeError> {
    let limit = arguments
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map(|l| l as usize)
        .unwrap_or(20);
    let rows = list_verified_jobs(
        &http_client(),
        &buyer_config(&state.config),
        buyer_identity,
        limit,
    )
    .await?;
    let failing = rows
        .iter()
        .filter(|r| r.receipt_verified == Some(false))
        .count();
    Ok(ToolCallResult::ok(vec![Content::json(serde_json::json!({
        "jobs": rows,
        "count": rows.len(),
        "receipts_failing_verification": failing,
    }))]))
}

/// `compute.deposit` (A3): claim a confirmed on-chain payment so it
/// credits this daemon's pre-funded balance with the coordinator. The
/// rail decides whose deposit it is and how much; re-claiming an
/// applied deposit id reports `credited: false` and never
/// double-credits. No daemon money moves on this path — the deposit
/// already happened on-chain, and the coordinator books the credit.
pub async fn run_deposit_call(
    state: &ComputeState,
    buyer_identity: &LocalIdentity,
    arguments: serde_json::Value,
) -> Result<ToolCallResult, ComputeError> {
    let deposit_id = arguments
        .get("deposit_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ComputeError::InvalidArgs("deposit_id (string) is required".into()))?
        .to_string();
    let outcome = claim_deposit(
        &http_client(),
        &buyer_config(&state.config),
        buyer_identity,
        &deposit_id,
    )
    .await?;
    let view = serde_json::to_value(&outcome)
        .map_err(|e| ComputeError::Accounting(format!("deposit view: {e}")))?;
    Ok(ToolCallResult::ok(vec![Content::json(view)]))
}

/// `compute.balance` (A3): this daemon's funds with the coordinator
/// (a signed read — the balance is the daemon's own spend pattern)
/// plus the deployment's public deposit instructions for topping up.
pub async fn run_balance_call(
    state: &ComputeState,
    buyer_identity: &LocalIdentity,
) -> Result<ToolCallResult, ComputeError> {
    let view =
        funds_with_deposit_info(&http_client(), &buyer_config(&state.config), buyer_identity)
            .await?;
    Ok(ToolCallResult::ok(vec![Content::json(view)]))
}

/// `compute.capacity` (Track A discovery): the coordinator's live
/// directory of purchasable (kind, model) rows with operator counts
/// and ask ranges — the free read an agent makes before spending
/// budget on a dispatch that can't match. Public aggregates, so no
/// signature and no payer binding.
pub async fn run_capacity_call(state: &ComputeState) -> Result<ToolCallResult, ComputeError> {
    let view = capacity(&http_client(), &buyer_config(&state.config)).await?;
    let view = serde_json::to_value(&view)
        .map_err(|e| ComputeError::Accounting(format!("capacity view: {e}")))?;
    Ok(ToolCallResult::ok(vec![Content::json(view)]))
}

/// `compute.verify` (A4): hold the chain to one job's money trail.
/// Read-only — the coordinator's history row is re-verified locally,
/// then the payout transaction it names is fetched from this daemon's
/// own RPC endpoint and checked to carry exactly this receipt's memo
/// and amount. Where the trail hasn't reached the chain yet, the
/// verdict says how far it goes instead of failing.
pub async fn run_verify_call(
    state: &ComputeState,
    buyer_identity: &LocalIdentity,
    arguments: serde_json::Value,
) -> Result<ToolCallResult, ComputeError> {
    let args: VerifyArgs =
        serde_json::from_value(arguments).map_err(|e| ComputeError::InvalidArgs(e.to_string()))?;
    let verification = verify_payout(
        &http_client(),
        &buyer_config(&state.config),
        buyer_identity,
        args.job_id,
    )
    .await?;
    let view = serde_json::to_value(&verification)
        .map_err(|e| ComputeError::Accounting(format!("verification view: {e}")))?;
    Ok(ToolCallResult::ok(vec![Content::json(view)]))
}

/// `compute.output` (A4): re-read one of this daemon's past jobs — the
/// output the operator produced, plus its receipt re-verified locally
/// (signature, operator key, output hash). Read-only; the coordinator
/// returns a job's output only to the buyer identity that signed it, so
/// no budget or settlement is touched. A job still in flight carries no
/// output yet; a refunded, rejected or failed one never will.
pub async fn run_output_call(
    state: &ComputeState,
    buyer_identity: &LocalIdentity,
    arguments: serde_json::Value,
) -> Result<ToolCallResult, ComputeError> {
    let args: OutputArgs =
        serde_json::from_value(arguments).map_err(|e| ComputeError::InvalidArgs(e.to_string()))?;
    let mut view = fetch_job_output(
        &http_client(),
        &buyer_config(&state.config),
        buyer_identity,
        args.job_id,
    )
    .await?;
    // A synthesized job's output carries the operator's audio base64; keep
    // it out of the agent's context on a re-read the same way `compute.speak`
    // does on the buy — write the clip and name it in place. Every other
    // kind (a completion, a transcript, an embedding) is content the agent
    // asked for and rides through untouched. The receipt already verified
    // against the operator's real output inside `fetch_job_output`, so
    // `receipt_verified` stays authoritative; the block simply names the
    // file instead of inlining megabytes of base64.
    save_speech_block(&state.config.clips_dir, &mut view.output, view.job_id);
    let view = serde_json::to_value(&view)
        .map_err(|e| ComputeError::Accounting(format!("output view: {e}")))?;
    Ok(ToolCallResult::ok(vec![Content::json(view)]))
}

/// `compute.withdraw` (A3): move unspent pre-funded balance back out
/// to a wallet the caller names. The daemon's buyer identity signs the
/// request — the recipient can't be re-pointed in flight — and the
/// withdrawal id (caller-supplied or minted here) makes retries
/// idempotent from the request to the on-chain memo. The coordinator's
/// books refuse an overdraw; no budget or settlement is touched — this
/// is the daemon's own deposited money coming home.
pub async fn run_withdraw_call(
    state: &ComputeState,
    buyer_identity: &LocalIdentity,
    arguments: serde_json::Value,
) -> Result<ToolCallResult, ComputeError> {
    let args: WithdrawArgs =
        serde_json::from_value(arguments).map_err(|e| ComputeError::InvalidArgs(e.to_string()))?;
    let withdrawal_id = args.withdrawal_id.unwrap_or_else(Uuid::new_v4);
    let outcome = withdraw(
        &http_client(),
        &buyer_config(&state.config),
        buyer_identity,
        withdrawal_id,
        args.amount_micro_usdc,
        &args.recipient_address_b58,
    )
    .await?;
    let view = serde_json::to_value(&outcome)
        .map_err(|e| ComputeError::Accounting(format!("withdrawal view: {e}")))?;
    Ok(ToolCallResult::ok(vec![Content::json(view)]))
}

/// `compute.withdrawals` (A4): this daemon identity's withdrawal
/// history from the coordinator, newest first — the audit companion to
/// `compute.withdraw`. Read-only, a signed read; each row carries
/// whether its transfer has landed and the on-chain memo to check it.
pub async fn run_withdrawals_call(
    state: &ComputeState,
    buyer_identity: &LocalIdentity,
) -> Result<ToolCallResult, ComputeError> {
    let rows =
        list_withdrawals(&http_client(), &buyer_config(&state.config), buyer_identity).await?;
    let view = serde_json::json!({ "withdrawals": rows, "count": rows.len() });
    Ok(ToolCallResult::ok(vec![Content::json(view)]))
}

/// `compute.dispute` (C4): sign and record a dispute of one of this
/// daemon's completed jobs. No daemon money moves — the job was paid
/// through a verified receipt and stays paid; what lands is the signed
/// reputation fault on the coordinator's books.
pub async fn run_dispute_call(
    state: &ComputeState,
    buyer_identity: &LocalIdentity,
    arguments: serde_json::Value,
) -> Result<ToolCallResult, ComputeError> {
    let args: DisputeArgs =
        serde_json::from_value(arguments).map_err(|e| ComputeError::InvalidArgs(e.to_string()))?;
    let outcome = dispute_job(
        &http_client(),
        &buyer_config(&state.config),
        buyer_identity,
        args.job_id,
        args.reason,
    )
    .await?;
    let view = serde_json::to_value(&outcome)
        .map_err(|e| ComputeError::Accounting(format!("dispute view: {e}")))?;
    Ok(ToolCallResult::ok(vec![Content::json(view)]))
}

/// `compute.cancel`: withdraw one of this daemon's still-unaccepted
/// jobs and take the refund now instead of waiting out the deadline.
/// On a refund the job's purchase entry (if this home holds one) is
/// voided, so the idempotency key that bought it may honestly buy
/// again — the coordinator just answered that the money never moved.
pub async fn run_cancel_call(
    state: &ComputeState,
    buyer_identity: &LocalIdentity,
    arguments: serde_json::Value,
) -> Result<ToolCallResult, ComputeError> {
    let args: CancelArgs =
        serde_json::from_value(arguments).map_err(|e| ComputeError::InvalidArgs(e.to_string()))?;
    let view = cancel_job(
        &http_client(),
        &buyer_config(&state.config),
        buyer_identity,
        args.job_id,
    )
    .await?;
    let key_freed = match state.purchases.void_by_job(args.job_id) {
        Ok(freed) => freed,
        Err(e) => {
            // The refund already happened; a re-drive of the still
            // bound key meets the coordinator's refunded echo and
            // voids it then.
            warn!(job_id = %args.job_id, error = %e, "cancelled but could not void the purchase entry");
            false
        }
    };
    let mut value = serde_json::to_value(&view)
        .map_err(|e| ComputeError::Accounting(format!("cancel view: {e}")))?;
    value["purchase_key_freed"] = serde_json::Value::Bool(key_freed);
    Ok(ToolCallResult::ok(vec![Content::json(value)]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_audit::{AuditLog, InMemoryAuditLog};
    use covenant_budget::{BudgetLedger, InMemoryLedger};
    use covenant_settlement::{InMemorySettlement, Settlement};
    use std::sync::Arc;

    fn state(cap: u64) -> ComputeState {
        // Guaranteed-unroutable without a daemon: any test reaching the
        // network fails fast and loudly here.
        state_with_url(cap, "http://127.0.0.1:1".into())
    }

    fn state_with_url(cap: u64, coordinator_url: String) -> ComputeState {
        ComputeState::new(ComputeConfig {
            coordinator_url,
            max_price_micro_usdc: cap,
            default_deadline_ms: 1_000,
            poll_interval: Duration::from_millis(10),
            referral_code: None,
            rpc_url: None,
            max_active_streams_per_payer: 4,
            clips_dir: std::env::temp_dir().join("covenant-compute-test-clips"),
        })
    }

    fn accounting(
        settlement: &Arc<InMemorySettlement>,
        audit: &Arc<InMemoryAuditLog>,
        budget: &Arc<InMemoryLedger>,
        issuer: &AgentId,
    ) -> StreamAccounting {
        StreamAccounting {
            settlement: settlement.clone(),
            audit: audit.clone(),
            budget: budget.clone(),
            issuer: issuer.clone(),
        }
    }

    fn subsystems() -> (
        Arc<InMemorySettlement>,
        Arc<InMemoryAuditLog>,
        Arc<InMemoryLedger>,
    ) {
        (
            Arc::new(InMemorySettlement::new()),
            Arc::new(InMemoryAuditLog::new()),
            Arc::new(InMemoryLedger::new()),
        )
    }

    #[test]
    fn credits_round_up_so_no_paid_job_is_free() {
        assert_eq!(credits_for_price(0), 0);
        assert_eq!(credits_for_price(1), 1);
        assert_eq!(credits_for_price(9_999), 1);
        assert_eq!(credits_for_price(10_000), 1);
        assert_eq!(credits_for_price(10_001), 2);
        assert_eq!(credits_for_price(25_000), 3);
    }

    #[tokio::test]
    async fn the_sync_debit_charges_the_envelope_price_not_a_smaller_receipt_price() {
        use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
        use covenant_compute_protocol::{
            CapabilityRequirement, JobEnvelopePayload, JobMeter, SignedWorkReceipt,
            WorkReceiptPayload,
        };

        let buyer = LocalIdentity::generate("buyer@test");
        let operator = LocalIdentity::generate("operator@test");
        let payer = buyer.agent_id();
        let job_id = Uuid::new_v4();

        let envelope = SignedJobEnvelope::sign(
            JobEnvelopePayload {
                job_id,
                buyer: payer.clone(),
                kind: JobKind::InferenceCall,
                capability_requirement: CapabilityRequirement {
                    gpu_class: None,
                    min_vram_gb: None,
                    model_id: None,
                    kind: JobKind::InferenceCall,
                    max_duration_secs: 30,
                    min_reputation_bps: None,
                },
                input: vec![Content::text("in")],
                price_micro_usdc: 1_000_000,
                deadline_ms: 30_000,
                idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "debit-test"),
                issued_at_ms: 1,
                referral_code: None,
                stream: false,
            },
            &buyer,
        )
        .unwrap();

        // A receipt the buyer would accept (price never exceeds the
        // envelope) but that names a far smaller figure: settlement charges
        // the held envelope price, so basing the debit on the receipt would
        // under-charge the payer's spend cap ~100x.
        let receipt = SignedWorkReceipt::sign(
            WorkReceiptPayload {
                job_id,
                operator: operator.agent_id(),
                job_hash_hex: "00".repeat(32),
                result_hash_hex: "11".repeat(32),
                meter: JobMeter {
                    wall_ms: 5,
                    tokens_in: None,
                    tokens_out: None,
                    gpu_seconds: None,
                    finish_reason: None,
                },
                price_micro_usdc: 1,
                status: A2ATaskStatus::Ok,
                executed_at_ms: 2,
                node_audit_root_hex: "cc".repeat(32),
            },
            &operator,
        )
        .unwrap();

        let outcome = DispatchOutcome {
            envelope,
            receipt,
            output: vec![Content::text("out")],
            payout: None,
        };

        let (settlement, audit, budget) = subsystems();
        budget.set_capacity(&payer, 10_000_000).await.unwrap();
        let ctx = SettlementContext {
            settlement: settlement.as_ref(),
            audit: audit.as_ref(),
            budget: budget.as_ref(),
            issuer: &payer,
        };

        settle_paid_outcome(&ctx, &payer, outcome)
            .await
            .expect("settles");

        let debits = budget.recent_debits_all(10).await.unwrap();
        assert_eq!(debits.len(), 1);
        // Charged the envelope price (credits_for_price(1_000_000) = 100),
        // never the receipt's credits_for_price(1) = 1.
        assert_eq!(debits[0].credits, 100);
    }

    #[tokio::test]
    async fn an_unpriced_call_offers_the_market_floor_not_the_ceiling() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // A coordinator advertising one operator serving inference at
        // 10_000 micro-USDC — far under the $1 per-call ceiling.
        let server = MockServer::start().await;
        let view = serde_json::json!({
            "registered_operators": 1,
            "matchable_operators": 1,
            "liveness_window_ms": 45_000,
            "min_score_bps": 0,
            "min_bond_micro_usdc": 0,
            "entries": [{
                "kind": "inference_call",
                "model": "any",
                "operators": 1,
                "min_ask_micro_usdc": 10_000,
                "min_ask_unit": "per_job",
                "max_ask_micro_usdc": 10_000,
                "max_vram_gb": 0,
                "gpu_classes": ["cpu"],
                "tee_capable": false,
            }],
        });
        Mock::given(method("GET"))
            .and(path("/federation/capacity"))
            .respond_with(ResponseTemplate::new(200).set_body_json(view))
            .mount(&server)
            .await;
        let st = state_with_url(1_000_000, server.uri());
        let cap = st.config.max_price_micro_usdc;

        // No price named: offer the market floor, not the ceiling.
        // Settlement charges the envelope's price, so defaulting to the
        // cap would pay this operator 100x its ask.
        assert_eq!(
            resolve_offer(
                &st,
                None,
                JobKind::InferenceCall,
                None,
                None,
                None,
                None,
                cap
            )
            .await,
            10_000,
            "an unpriced call must offer the market floor"
        );
        // An explicit price always wins, capped by the ceiling.
        assert_eq!(
            resolve_offer(
                &st,
                Some(5),
                JobKind::InferenceCall,
                None,
                None,
                None,
                None,
                cap
            )
            .await,
            5
        );
        // Nothing serving the (kind, model) falls back to the ceiling, so
        // no call the old default allowed is refused.
        assert_eq!(
            resolve_offer(&st, None, JobKind::BatchJob, None, None, None, None, cap).await,
            cap
        );
    }

    /// A `dry_run` on the native capability resolves the price and routing a
    /// real buy would use and hands them back without dispatching, debiting,
    /// or recording anything — and, unlike a real dispatch, refuses an
    /// unservable ask rather than falling back to the ceiling.
    #[tokio::test]
    async fn a_dry_run_previews_the_price_and_debits_nothing() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let view = serde_json::json!({
            "registered_operators": 1,
            "matchable_operators": 1,
            "liveness_window_ms": 45_000,
            "min_score_bps": 0,
            "min_bond_micro_usdc": 0,
            "entries": [{
                "kind": "inference_call",
                "model": "any",
                "operators": 1,
                "min_ask_micro_usdc": 10_000,
                "min_ask_unit": "per_job",
                "max_ask_micro_usdc": 10_000,
                "max_vram_gb": 0,
                "gpu_classes": ["cpu"],
                "tee_capable": false,
            }],
        });
        Mock::given(method("GET"))
            .and(path("/federation/capacity"))
            .respond_with(ResponseTemplate::new(200).set_body_json(view))
            .mount(&server)
            .await;

        let (settlement, audit, budget) = subsystems();
        let payer = LocalIdentity::generate("payer@test").agent_id();
        budget.set_capacity(&payer, 10_000_000).await.unwrap();
        let identity = LocalIdentity::generate("daemon@test");
        let issuer = identity.agent_id();
        let ctx = SettlementContext {
            settlement: settlement.as_ref(),
            audit: audit.as_ref(),
            budget: budget.as_ref(),
            issuer: &issuer,
        };
        let st = state_with_url(1_000_000, server.uri());

        fn json_of(result: ToolCallResult) -> serde_json::Value {
            match result.content.into_iter().next() {
                Some(Content::Json { value }) => value,
                other => panic!("a preview is a single json block, got {other:?}"),
            }
        }

        // No price named: the preview reports the 10_000 cheapest matching
        // ask, tagged as a dry run, and charges nothing.
        let doc = json_of(
            run_infer_call(
                &st,
                &ctx,
                &identity,
                &payer,
                serde_json::json!({ "prompt": "preview me", "dry_run": true }),
            )
            .await
            .expect("preview resolves"),
        );
        assert_eq!(doc["dry_run"], true);
        assert_eq!(doc["kind"], "inference_call");
        assert_eq!(doc["price_micro_usdc"], 10_000);
        assert_eq!(doc["price_source"], "cheapest_matching_ask");

        // An explicit price previews as the caller's own figure.
        let doc = json_of(
            run_infer_call(
                &st,
                &ctx,
                &identity,
                &payer,
                serde_json::json!({ "prompt": "q", "price_micro_usdc": 20_000, "dry_run": true }),
            )
            .await
            .expect("preview resolves"),
        );
        assert_eq!(doc["price_micro_usdc"], 20_000);
        assert_eq!(doc["price_source"], "explicit");

        // The batch tool previews too. The market serves only inference, so
        // an explicit price shows the batch path resolving with no batch
        // operator to quote from.
        let doc = json_of(
            run_batch_call(
                &st,
                &ctx,
                &identity,
                &payer,
                serde_json::json!({ "command": "echo hi", "price_micro_usdc": 15_000, "dry_run": true }),
            )
            .await
            .expect("batch preview resolves"),
        );
        assert_eq!(doc["kind"], "batch_job");
        assert_eq!(doc["price_micro_usdc"], 15_000);
        assert_eq!(doc["price_source"], "explicit");

        // An ask no operator can serve refuses in preview — not the
        // ceiling fallback a real agent-side buy takes.
        let err = run_infer_call(
            &st,
            &ctx,
            &identity,
            &payer,
            serde_json::json!({ "prompt": "q", "min_vram_gb": 999, "dry_run": true }),
        )
        .await
        .expect_err("an unservable ask refuses");
        assert!(
            matches!(&err, ComputeError::Quote(_))
                && err.to_string().contains("no operator is serving"),
            "got: {err}"
        );

        // A preview reserves no purchase, so it refuses an idempotency key.
        let err = run_infer_call(
            &st,
            &ctx,
            &identity,
            &payer,
            serde_json::json!({ "prompt": "q", "idempotency_key": "k", "dry_run": true }),
        )
        .await
        .expect_err("a preview takes no key");
        assert!(
            matches!(&err, ComputeError::InvalidArgs(m) if m.contains("idempotency_key")),
            "got: {err}"
        );

        // The contract: none of those previews debited, settled, or
        // audited anything.
        assert!(budget.recent_debits_all(10).await.unwrap().is_empty());
        assert!(settlement.recent(10).await.unwrap().is_empty());
        assert!(audit.recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_unpriced_call_falls_back_to_the_ceiling_when_the_market_is_unreadable() {
        // Unroutable coordinator: the capacity read fails and the offer
        // falls back to the ceiling rather than refusing the call.
        let st = state(1_000_000);
        assert_eq!(
            resolve_offer(
                &st,
                None,
                JobKind::InferenceCall,
                None,
                None,
                None,
                None,
                1_000_000
            )
            .await,
            1_000_000
        );
        // An explicit price never touches the network at all.
        assert_eq!(
            resolve_offer(
                &st,
                Some(7),
                JobKind::InferenceCall,
                None,
                None,
                None,
                None,
                1_000_000
            )
            .await,
            7
        );
    }

    #[test]
    fn render_speech_clip_saves_the_audio_and_drops_the_base64() {
        let dir = std::env::temp_dir().join(format!("covenant-speak-render-{}", Uuid::new_v4()));
        let job_id = Uuid::new_v4();
        // The shape `paid_response` returns for a synthesized job: the
        // operator's speech block, then the receipt metadata carrying the
        // job id.
        let result = ToolCallResult::ok(vec![
            Content::json(serde_json::json!({
                "model": "say-1",
                "audio_base64": "aGVsbG8=", // "hello"
                "format": "wav",
            })),
            Content::json(serde_json::json!({
                "job_id": job_id,
                "receipt_id": Uuid::new_v4(),
                "price_micro_usdc": 10u64,
            })),
        ]);
        let rendered = render_speech_clip(&dir, result);
        assert_eq!(rendered.content.len(), 2);
        let Content::Json { value: described } = &rendered.content[0] else {
            panic!("the speech block should render as a json reference");
        };
        assert_eq!(described["saved"], serde_json::json!(true));
        assert_eq!(described["bytes"], serde_json::json!(5));
        assert_eq!(described["format"], serde_json::json!("wav"));
        assert!(
            described.get("audio_base64").is_none(),
            "the base64 audio must never ride back to the agent"
        );
        let path = described["path"].as_str().expect("a saved clip path");
        assert!(path.ends_with(&format!("speech-{job_id}.wav")));
        assert_eq!(std::fs::read(path).expect("the clip was written"), b"hello");
        let Content::Json { value: receipt } = &rendered.content[1] else {
            panic!("the receipt block should ride through untouched");
        };
        assert_eq!(receipt["job_id"], serde_json::json!(job_id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn render_speech_clip_passes_a_non_speech_result_through() {
        // A dry-run preview carries no speech block; it must ride through
        // untouched rather than have a clip forced onto it.
        let block = serde_json::json!({ "dry_run": true, "price_micro_usdc": 10u64 });
        let preview = ToolCallResult::ok(vec![Content::json(block.clone())]);
        let rendered = render_speech_clip(Path::new("/nonexistent-should-not-be-touched"), preview);
        assert_eq!(rendered.content.len(), 1);
        let Content::Json { value } = &rendered.content[0] else {
            panic!("the preview block should be unchanged");
        };
        assert_eq!(*value, block);
    }

    #[test]
    fn output_view_speech_is_saved_as_a_clip_never_base64() {
        // A re-read (`compute.output`) hands back a job view's output blocks
        // with no appended receipt metadata — the job id comes from the view,
        // not a block — so `save_speech_block` takes it explicitly. The audio
        // must still be written to a clip and dropped, exactly like the buy.
        let dir = std::env::temp_dir().join(format!("covenant-speak-output-{}", Uuid::new_v4()));
        let job_id = Uuid::new_v4();
        let mut blocks = vec![Content::json(serde_json::json!({
            "model": "say-1",
            "audio_base64": "aGVsbG8=", // "hello"
            "format": "wav",
            "sample_rate_hz": 22_050u32,
        }))];
        save_speech_block(&dir, &mut blocks, job_id);
        let Content::Json { value } = &blocks[0] else {
            panic!("the audio block should become a saved-clip reference");
        };
        assert_eq!(value["saved"], serde_json::json!(true));
        assert_eq!(value["sample_rate_hz"], serde_json::json!(22_050));
        assert!(
            value.get("audio_base64").is_none(),
            "base64 audio must never survive a re-read into the agent's context"
        );
        let path = value["path"].as_str().expect("a saved clip path");
        assert!(path.ends_with(&format!("speech-{job_id}.wav")));
        assert_eq!(std::fs::read(path).expect("the clip was written"), b"hello");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn specs_advertise_the_full_buyer_surface() {
        let specs = compute_specs(&ComputeConfig::default());
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
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
                OUTPUT_TOOL
            ]
        );
        // The transcription tool advertises its audio input so an agent
        // discovers it can buy speech-to-text, and requires it so a call
        // with no clip is refused before it costs anything.
        let transcribe = specs
            .iter()
            .find(|s| s.name == TRANSCRIBE_TOOL)
            .expect("transcribe is advertised");
        assert!(transcribe.input_schema["properties"]["audio_base64"].is_object());
        assert_eq!(
            transcribe.input_schema["required"],
            serde_json::json!(["audio_base64"])
        );
        // The synthesis tool advertises its text input so an agent
        // discovers it can buy text-to-speech, and requires it so a call
        // with nothing to say is refused before it costs anything.
        let speak = specs
            .iter()
            .find(|s| s.name == SPEAK_TOOL)
            .expect("speak is advertised");
        assert!(speak.input_schema["properties"]["text"].is_object());
        assert_eq!(speak.input_schema["required"], serde_json::json!(["text"]));
        assert!(specs[0].input_schema["properties"]["prompt"].is_object());
        assert!(specs[0].input_schema["properties"]["messages"].is_object());
        // Image input is advertised so an agent discovers it can attach one.
        assert!(specs[0].input_schema["properties"]["images"].is_object());
        // Sampling knobs are advertised so an agent can discover them,
        // and stream_start inherits them by sharing infer's schema.
        for knob in [
            "temperature",
            "top_p",
            "max_tokens",
            "seed",
            "presence_penalty",
            "frequency_penalty",
            "stop",
            "logprobs",
        ] {
            assert!(
                specs[0].input_schema["properties"][knob].is_object(),
                "compute.infer must advertise {knob}"
            );
        }
        // compute.embed takes text and routes by an embedding model,
        // compute.transcribe takes audio, compute.speak takes text to
        // voice, compute.run takes a command.
        assert!(specs[1].input_schema["properties"]["text"].is_object());
        assert!(specs[1].input_schema["properties"]["model"].is_object());
        assert!(specs[2].input_schema["properties"]["audio_base64"].is_object());
        assert!(specs[3].input_schema["properties"]["text"].is_object());
        assert!(specs[4].input_schema["properties"]["command"].is_object());
        // The five buying tools advertise the hardware ask so an agent
        // can require a GPU class or VRAM floor; run leans on it most,
        // having no model to route by. Each also advertises dry_run so an
        // agent can discover the cost-preview.
        for tool in [&specs[0], &specs[1], &specs[2], &specs[3], &specs[4]] {
            for field in ["gpu_class", "min_vram_gb", "dry_run"] {
                assert!(
                    tool.input_schema["properties"][field].is_object(),
                    "{} must advertise {field}",
                    tool.name
                );
            }
        }
        // stream_start shares infer's schema but for the two guarantees it
        // can't keep: exactly-once (idempotency_key) and a cost-preview
        // (dry_run). It must advertise neither.
        assert!(specs[5].input_schema["properties"]["prompt"].is_object());
        assert!(specs[5].input_schema["properties"]["idempotency_key"].is_null());
        assert!(specs[5].input_schema["properties"]["dry_run"].is_null());
        let mut infer_without_stream_gaps = specs[0].input_schema.clone();
        let props = infer_without_stream_gaps["properties"]
            .as_object_mut()
            .unwrap();
        props.remove("idempotency_key");
        props.remove("dry_run");
        assert_eq!(specs[5].input_schema, infer_without_stream_gaps);
        assert!(specs[6].input_schema["properties"]["since"].is_object());
    }

    /// The drive map under the keyed-purchase serialization: one lock
    /// per key while anyone holds it, a second driver waits its turn,
    /// a queued racer keeps the entry alive through the winner's
    /// release, and the map empties once the last handle drops — keys
    /// in flight bound it, not keys ever used.
    #[tokio::test]
    async fn drive_permits_serialize_per_key_and_the_map_stays_bounded() {
        let state = state(10);

        let permit = state.drive_permit("payer:serial");
        let guard = permit.try_lock().expect("a fresh key's lock is free");
        assert!(
            state.drive_permit("payer:serial").try_lock().is_err(),
            "a second driver must wait its turn"
        );
        assert!(
            state.drive_permit("payer:other").try_lock().is_ok(),
            "keys don't share locks"
        );
        state.release_drive("payer:other");
        drop(guard);

        // The winner concludes while a racer still holds its handle:
        // the entry must survive, or the next retry would mint a
        // second lock and drive beside the racer.
        let racer = state.drive_permit("payer:serial");
        drop(permit);
        state.release_drive("payer:serial");
        assert!(
            Arc::ptr_eq(&racer, &state.drive_permit("payer:serial")),
            "a queued racer keeps the key's lock alive"
        );
        drop(racer);
        state.release_drive("payer:serial");
        assert!(
            state.drives.lock().unwrap().is_empty(),
            "the last release empties the map"
        );
    }

    #[tokio::test]
    async fn verify_refuses_malformed_arguments_before_any_network() {
        let identity = LocalIdentity::generate("daemon@test");
        for arguments in [
            serde_json::json!({}),
            serde_json::json!({"job_id": "not-a-uuid"}),
        ] {
            let err = run_verify_call(&state(100), &identity, arguments)
                .await
                .expect_err("malformed verify args");
            assert!(matches!(err, ComputeError::InvalidArgs(_)), "got: {err}");
        }
        // Well-formed args reach the (unroutable) coordinator and
        // surface its failure as a buyer error, not a panic.
        let err = run_verify_call(
            &state(100),
            &identity,
            serde_json::json!({"job_id": Uuid::new_v4()}),
        )
        .await
        .expect_err("unroutable coordinator");
        assert!(matches!(err, ComputeError::Buyer(_)), "got: {err}");
    }

    #[tokio::test]
    async fn output_refuses_malformed_arguments_before_any_network() {
        let identity = LocalIdentity::generate("daemon@test");
        for arguments in [
            serde_json::json!({}),
            serde_json::json!({"job_id": "not-a-uuid"}),
        ] {
            let err = run_output_call(&state(100), &identity, arguments)
                .await
                .expect_err("malformed output args");
            assert!(matches!(err, ComputeError::InvalidArgs(_)), "got: {err}");
        }
        // A well-formed id reaches the (unroutable) coordinator and
        // surfaces its failure as a buyer error, not a panic — a re-read
        // spends nothing, so no budget is touched either way.
        let err = run_output_call(
            &state(100),
            &identity,
            serde_json::json!({"job_id": Uuid::new_v4()}),
        )
        .await
        .expect_err("unroutable coordinator");
        assert!(matches!(err, ComputeError::Buyer(_)), "got: {err}");
    }

    #[tokio::test]
    async fn withdraw_refuses_malformed_arguments_before_any_network() {
        let identity = LocalIdentity::generate("daemon@test");
        for arguments in [
            serde_json::json!({}),
            serde_json::json!({"amount_micro_usdc": 100}),
            serde_json::json!({"amount_micro_usdc": "lots", "recipient_address_b58": "w"}),
            serde_json::json!({
                "amount_micro_usdc": 100,
                "recipient_address_b58": "w",
                "withdrawal_id": "not-a-uuid",
            }),
        ] {
            let err = run_withdraw_call(&state(100), &identity, arguments)
                .await
                .expect_err("malformed withdraw args");
            assert!(matches!(err, ComputeError::InvalidArgs(_)), "got: {err}");
        }
        // A well-formed but bogus recipient bounces at signing, before
        // the (unroutable) coordinator is ever dialed.
        let err = run_withdraw_call(
            &state(100),
            &identity,
            serde_json::json!({"amount_micro_usdc": 100, "recipient_address_b58": "abc"}),
        )
        .await
        .expect_err("short recipient");
        assert!(matches!(err, ComputeError::Buyer(_)), "got: {err}");
    }

    #[tokio::test]
    async fn dispute_refuses_malformed_arguments_before_any_network() {
        let identity = LocalIdentity::generate("daemon@test");
        for arguments in [
            serde_json::json!({}),
            serde_json::json!({"job_id": "not-a-uuid", "reason": "bad"}),
            serde_json::json!({"job_id": Uuid::new_v4()}),
        ] {
            let err = run_dispute_call(&state(100), &identity, arguments)
                .await
                .expect_err("malformed dispute args");
            assert!(matches!(err, ComputeError::InvalidArgs(_)), "got: {err}");
        }
        // A well-formed but empty reason bounces at signing, before the
        // (unroutable) coordinator is ever dialed.
        let err = run_dispute_call(
            &state(100),
            &identity,
            serde_json::json!({"job_id": Uuid::new_v4(), "reason": "  "}),
        )
        .await
        .expect_err("empty reason");
        assert!(matches!(err, ComputeError::Buyer(_)), "got: {err}");
    }

    #[tokio::test]
    async fn refuses_a_price_above_the_ceiling_before_any_network_or_debit() {
        let (settlement, audit, budget) = subsystems();
        let payer = LocalIdentity::generate("payer@test").agent_id();
        budget.set_capacity(&payer, 1_000).await.unwrap();
        let identity = LocalIdentity::generate("daemon@test");
        let issuer = identity.agent_id();
        let ctx = SettlementContext {
            settlement: settlement.as_ref(),
            audit: audit.as_ref(),
            budget: budget.as_ref(),
            issuer: &issuer,
        };
        let err = run_infer_call(
            &state(100),
            &ctx,
            &identity,
            &payer,
            serde_json::json!({"prompt": "hi", "price_micro_usdc": 200}),
        )
        .await
        .expect_err("over-cap price");
        assert!(matches!(err, ComputeError::PriceCap { .. }), "got: {err}");
        assert!(settlement.recent(10).await.unwrap().is_empty());
        assert!(audit.recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn embed_rides_the_same_spend_path_and_price_cap() {
        let (settlement, audit, budget) = subsystems();
        let payer = LocalIdentity::generate("payer@test").agent_id();
        budget.set_capacity(&payer, 1_000).await.unwrap();
        let identity = LocalIdentity::generate("daemon@test");
        let issuer = identity.agent_id();
        let ctx = SettlementContext {
            settlement: settlement.as_ref(),
            audit: audit.as_ref(),
            budget: budget.as_ref(),
            issuer: &issuer,
        };
        // An over-cap embedding is refused before any network or debit,
        // exactly like an over-cap inference — the two share the path.
        let err = run_embed_call(
            &state(100),
            &ctx,
            &identity,
            &payer,
            serde_json::json!({"text": "vectorize me", "price_micro_usdc": 200}),
        )
        .await
        .expect_err("over-cap price");
        assert!(matches!(err, ComputeError::PriceCap { .. }), "got: {err}");
        assert!(settlement.recent(10).await.unwrap().is_empty());
        assert!(audit.recent(10).await.unwrap().is_empty());

        // A blank text is refused before the price is even weighed.
        let err = run_embed_call(
            &state(100_000),
            &ctx,
            &identity,
            &payer,
            serde_json::json!({"text": "   "}),
        )
        .await
        .expect_err("blank text");
        assert!(matches!(err, ComputeError::InvalidArgs(_)), "got: {err}");
    }

    #[tokio::test]
    async fn transcribe_rides_the_same_spend_path_and_price_cap() {
        let (settlement, audit, budget) = subsystems();
        let payer = LocalIdentity::generate("payer@test").agent_id();
        budget.set_capacity(&payer, 1_000).await.unwrap();
        let identity = LocalIdentity::generate("daemon@test");
        let issuer = identity.agent_id();
        let ctx = SettlementContext {
            settlement: settlement.as_ref(),
            audit: audit.as_ref(),
            budget: budget.as_ref(),
            issuer: &issuer,
        };
        // An over-cap transcription is refused before any network or debit,
        // exactly like an over-cap inference — every buying tool shares the
        // spend path.
        let err = run_transcribe_call(
            &state(100),
            &ctx,
            &identity,
            &payer,
            serde_json::json!({"audio_base64": "YWJj", "price_micro_usdc": 200}),
        )
        .await
        .expect_err("over-cap price");
        assert!(matches!(err, ComputeError::PriceCap { .. }), "got: {err}");
        assert!(settlement.recent(10).await.unwrap().is_empty());
        assert!(audit.recent(10).await.unwrap().is_empty());

        // A clip that is not valid base64 is the buyer's own error, refused
        // before the price is weighed — it never reaches an operator that
        // would fault the job on input the buyer controls.
        let err = run_transcribe_call(
            &state(100_000),
            &ctx,
            &identity,
            &payer,
            serde_json::json!({"audio_base64": "not base64!"}),
        )
        .await
        .expect_err("invalid base64");
        assert!(matches!(err, ComputeError::InvalidArgs(_)), "got: {err}");
    }

    #[tokio::test]
    async fn refuses_a_payer_without_budget_before_any_network_activity() {
        let (settlement, audit, budget) = subsystems();
        let payer = LocalIdentity::generate("payer@test").agent_id(); // never given capacity
        let identity = LocalIdentity::generate("daemon@test");
        let issuer = identity.agent_id();
        let ctx = SettlementContext {
            settlement: settlement.as_ref(),
            audit: audit.as_ref(),
            budget: budget.as_ref(),
            issuer: &issuer,
        };
        let err = run_infer_call(
            &state(100_000),
            &ctx,
            &identity,
            &payer,
            serde_json::json!({"prompt": "hi"}),
        )
        .await
        .expect_err("no capacity");
        assert!(matches!(err, ComputeError::NoCapacity), "got: {err}");
    }

    #[tokio::test]
    async fn deposit_without_a_deposit_id_is_refused_before_any_network() {
        let identity = LocalIdentity::generate("daemon@test");
        for arguments in [
            serde_json::json!({}),
            serde_json::json!({"deposit_id": ""}),
            serde_json::json!({"deposit_id": "   "}),
            serde_json::json!({"deposit_id": 42}),
        ] {
            let err = run_deposit_call(&state(100), &identity, arguments)
                .await
                .expect_err("missing deposit_id");
            assert!(matches!(err, ComputeError::InvalidArgs(_)), "got: {err}");
        }
    }

    #[tokio::test]
    async fn balance_surfaces_an_unreachable_coordinator_as_a_buyer_error() {
        let identity = LocalIdentity::generate("daemon@test");
        let err = run_balance_call(&state(100), &identity)
            .await
            .expect_err("unroutable coordinator");
        assert!(matches!(err, ComputeError::Buyer(_)), "got: {err}");
    }

    #[tokio::test]
    async fn capacity_surfaces_an_unreachable_coordinator_as_a_buyer_error() {
        let err = run_capacity_call(&state(100))
            .await
            .expect_err("unroutable coordinator");
        assert!(matches!(err, ComputeError::Buyer(_)), "got: {err}");
    }

    #[tokio::test]
    async fn rejects_arguments_without_a_prompt() {
        let (settlement, audit, budget) = subsystems();
        let payer = LocalIdentity::generate("payer@test").agent_id();
        let identity = LocalIdentity::generate("daemon@test");
        let issuer = identity.agent_id();
        let ctx = SettlementContext {
            settlement: settlement.as_ref(),
            audit: audit.as_ref(),
            budget: budget.as_ref(),
            issuer: &issuer,
        };
        let err = run_infer_call(
            &state(100_000),
            &ctx,
            &identity,
            &payer,
            serde_json::json!({"model": "llama-3-8b"}),
        )
        .await
        .expect_err("missing prompt");
        assert!(matches!(err, ComputeError::InvalidArgs(_)), "got: {err}");
    }

    #[tokio::test]
    async fn stream_start_refuses_bad_args_price_and_budget_before_any_network() {
        let (settlement, audit, budget) = subsystems();
        let payer = LocalIdentity::generate("payer@test").agent_id();
        let identity = Arc::new(LocalIdentity::generate("daemon@test"));
        let issuer = identity.agent_id();
        let state = Arc::new(state(100));

        for (arguments, wants) in [
            (serde_json::json!({}), "InvalidArgs"),
            (
                serde_json::json!({"prompt": "hi", "messages": []}),
                "InvalidArgs",
            ),
            (
                serde_json::json!({"prompt": "hi", "price_micro_usdc": 200}),
                "PriceCap",
            ),
            // Streaming can't back exactly-once, so a key is refused up front.
            (
                serde_json::json!({"prompt": "hi", "idempotency_key": "k"}),
                "InvalidArgs",
            ),
            // A stream has nothing to preview, so a dry run is refused up
            // front — before the budget is even weighed.
            (
                serde_json::json!({"prompt": "hi", "dry_run": true}),
                "InvalidArgs",
            ),
            // No capacity was ever granted to this payer.
            (serde_json::json!({"prompt": "hi"}), "NoCapacity"),
        ] {
            let err = run_stream_start_call(
                &state,
                accounting(&settlement, &audit, &budget, &issuer),
                identity.clone(),
                &payer,
                arguments.clone(),
            )
            .await
            .expect_err("refused before dispatch");
            let matched = match wants {
                "InvalidArgs" => matches!(err, ComputeError::InvalidArgs(_)),
                "PriceCap" => matches!(err, ComputeError::PriceCap { .. }),
                _ => matches!(err, ComputeError::NoCapacity),
            };
            assert!(matched, "arguments {arguments}: got {err}");
        }
        // The idempotency refusal names the idempotent tool to use instead.
        let err = run_stream_start_call(
            &state,
            accounting(&settlement, &audit, &budget, &issuer),
            identity.clone(),
            &payer,
            serde_json::json!({"prompt": "hi", "idempotency_key": "k"}),
        )
        .await
        .expect_err("a key is refused");
        assert!(
            matches!(&err, ComputeError::InvalidArgs(m) if m.contains("idempotency_key") && m.contains(INFER_TOOL)),
            "got: {err}"
        );
        assert_eq!(state.streams.active_committed(&payer.pubkey_base58()), 0);
        assert!(settlement.recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn stream_start_rolls_the_registration_back_when_submit_fails() {
        let (settlement, audit, budget) = subsystems();
        let payer = LocalIdentity::generate("payer@test").agent_id();
        budget.set_capacity(&payer, 1_000).await.unwrap();
        let identity = Arc::new(LocalIdentity::generate("daemon@test"));
        let issuer = identity.agent_id();
        let state = Arc::new(state(100));

        let err = run_stream_start_call(
            &state,
            accounting(&settlement, &audit, &budget, &issuer),
            identity,
            &payer,
            serde_json::json!({"prompt": "hi"}),
        )
        .await
        .expect_err("unroutable coordinator");
        assert!(matches!(err, ComputeError::Buyer(_)), "got: {err}");
        // The failed submit freed the slot and its committed credits.
        assert_eq!(state.streams.active_committed(&payer.pubkey_base58()), 0);
        assert!(settlement.recent(10).await.unwrap().is_empty());
        assert!(audit.recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn stream_start_enforces_the_per_payer_active_cap() {
        let (settlement, audit, budget) = subsystems();
        let payer = LocalIdentity::generate("payer@test").agent_id();
        budget.set_capacity(&payer, 1_000).await.unwrap();
        let identity = Arc::new(LocalIdentity::generate("daemon@test"));
        let issuer = identity.agent_id();
        let mut config = state(100).config;
        config.max_active_streams_per_payer = 1;
        let state = Arc::new(ComputeState::new(config));
        state
            .streams
            .try_start(&payer.pubkey_base58(), Uuid::new_v4(), 1)
            .unwrap();

        let err = run_stream_start_call(
            &state,
            accounting(&settlement, &audit, &budget, &issuer),
            identity,
            &payer,
            serde_json::json!({"prompt": "hi"}),
        )
        .await
        .expect_err("cap reached");
        assert!(
            matches!(
                err,
                ComputeError::Stream(StreamJobsError::ActiveCap { active: 1, cap: 1 })
            ),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn in_flight_stream_credits_count_against_the_sync_path_too() {
        let (settlement, audit, budget) = subsystems();
        let payer = LocalIdentity::generate("payer@test").agent_id();
        // Capacity for exactly one credit.
        budget.set_capacity(&payer, 1).await.unwrap();
        let identity = LocalIdentity::generate("daemon@test");
        let issuer = identity.agent_id();
        let ctx = SettlementContext {
            settlement: settlement.as_ref(),
            audit: audit.as_ref(),
            budget: budget.as_ref(),
            issuer: &issuer,
        };
        let state = state(100_000);
        // One running streaming job has already committed that credit.
        state
            .streams
            .try_start(&payer.pubkey_base58(), Uuid::new_v4(), 1)
            .unwrap();
        let err = run_infer_call(
            &state,
            &ctx,
            &identity,
            &payer,
            serde_json::json!({"prompt": "hi", "price_micro_usdc": 10_000}),
        )
        .await
        .expect_err("would overcommit across the two paths");
        assert!(matches!(err, ComputeError::BudgetExceeded), "got: {err}");
    }

    #[tokio::test]
    async fn stream_poll_is_owner_checked_and_cursor_driven() {
        let state = state(100);
        let alice = LocalIdentity::generate("alice@test").agent_id();
        let mallory = LocalIdentity::generate("mallory@test").agent_id();
        let job_id = Uuid::new_v4();
        state
            .streams
            .try_start(&alice.pubkey_base58(), job_id, 1)
            .unwrap();
        state.streams.append_chunk(job_id, "hel");
        state.streams.append_chunk(job_id, "lo");

        // A foreign payer and a bogus id read identically.
        for (payer, id) in [(&mallory, job_id), (&alice, Uuid::new_v4())] {
            let err = run_stream_poll_call(&state, payer, serde_json::json!({"job_id": id}))
                .await
                .expect_err("not this caller's job");
            assert!(
                matches!(err, ComputeError::Stream(StreamJobsError::UnknownJob(_))),
                "got: {err}"
            );
        }

        let page = run_stream_poll_call(&state, &alice, serde_json::json!({"job_id": job_id}))
            .await
            .unwrap();
        let Content::Json { value } = &page.content[0] else {
            panic!("poll returns a json block");
        };
        assert_eq!(value["status"], "streaming");
        assert_eq!(value["chunks"], serde_json::json!(["hel", "lo"]));
        assert_eq!(value["next_seq"], 2);

        // The cursor skips what the caller already saw.
        let page = run_stream_poll_call(
            &state,
            &alice,
            serde_json::json!({"job_id": job_id, "since": 2}),
        )
        .await
        .unwrap();
        let Content::Json { value } = &page.content[0] else {
            panic!("poll returns a json block");
        };
        assert_eq!(value["chunks"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn stream_poll_serves_the_terminal_payload_and_failures_repeatedly() {
        let state = state(100);
        let alice = LocalIdentity::generate("alice@test").agent_id();
        let done = Uuid::new_v4();
        state
            .streams
            .try_start(&alice.pubkey_base58(), done, 1)
            .unwrap();
        state.streams.append_chunk(done, "out");
        state.streams.conclude(
            done,
            Ok(vec![
                Content::text("out"),
                Content::json(serde_json::json!({"receipt_id": "r"})),
            ]),
        );
        for _ in 0..2 {
            let page = run_stream_poll_call(&state, &alice, serde_json::json!({"job_id": done}))
                .await
                .unwrap();
            assert_eq!(page.content.len(), 3, "bookkeeping + final blocks");
            let Content::Json { value } = &page.content[0] else {
                panic!("poll returns a json block first");
            };
            assert_eq!(value["status"], "completed");
            assert_eq!(page.content[1], Content::text("out"));
        }

        let failed = Uuid::new_v4();
        state
            .streams
            .try_start(&alice.pubkey_base58(), failed, 1)
            .unwrap();
        state
            .streams
            .conclude(failed, Err("job failed with status error".into()));
        let err = run_stream_poll_call(&state, &alice, serde_json::json!({"job_id": failed}))
            .await
            .expect_err("drain failure re-surfaces");
        assert!(
            matches!(&err, ComputeError::StreamFailed(m) if m == "job failed with status error"),
            "got: {err}"
        );
    }
}
