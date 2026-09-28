//! Buyer-side client for the Covenant compute network (codename
//! compute): sign a job envelope, submit it to a coordinator, poll the
//! receipt, and re-verify every operator commitment locally — the
//! receipt's own signature, the job-hash pin to the exact envelope
//! bytes the buyer signed, the output hash, and the price ceiling.
//!
//! One implementation, two consumers: `covenantd`'s native
//! `compute.infer`/`compute.run` capabilities (which add
//! budget/settlement/audit accounting on top) and the standalone
//! `covenant-compute-mcp` stdio server (the lowest-friction demand
//! path — any MCP client adds it and buys compute). Verification logic
//! this load-bearing must not fork between them.

#![deny(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;

use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency, A2ATaskStatus};
use covenant_compute_protocol::{
    canonical_model, chat_input, coordinator_reason, generation_input, output_hash_hex,
    parse_speech_input, payout_transaction_rpc_request, speech_input, tools_input,
    transcription_input, verify_payout_transaction, CancelRequest, CapabilityRequirement,
    ChatMessage, DisputeRequest, GenerationParams, JobEnvelopePayload, LeaseCloseRequest,
    LeaseView, SignedWorkReceipt, SpeechInput, ToolChoiceMode, TranscriptionInput,
    DEPOSIT_MEMO_PREFIX, MAX_AUDIO_B64_BYTES, MAX_DISPUTE_REASON_BYTES,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::{Content, ToolSpec};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Re-exported for consumers that pick a [`JobRequest::kind`] or hold
/// the envelope between [`submit_streaming`] and [`stream_and_verify`]
/// without depending on the protocol crate directly.
pub use covenant_compute_protocol::{
    CancelView, CapacityEntry, CapacityView, JobKind, PayoutProof, ResponseFormat,
    SignedJobEnvelope, SpeechResult, ToolChoice, ToolDefinition,
};

mod purchases;
pub use purchases::{PurchaseBook, PurchaseEntry, PurchaseError};

mod vault;
pub use vault::{vault_delete, vault_fetch, vault_list, vault_store, KeyringError, VaultKeyring};

mod stream_jobs;
pub use stream_jobs::{
    StreamJobPoll, StreamJobs, StreamJobsError, MAX_CONCLUDED_JOBS, STREAM_JOB_LINGER,
};

mod openai;
pub use openai::{openai_router, OpenAiState};
mod anthropic;
pub use anthropic::anthropic_router;
mod gemini;
pub use gemini::gemini_router;
mod responses;

pub const INFER_TOOL: &str = "compute.infer";
pub const EMBED_TOOL: &str = "compute.embed";
pub const TRANSCRIBE_TOOL: &str = "compute.transcribe";
pub const SPEAK_TOOL: &str = "compute.speak";
pub const RUN_TOOL: &str = "compute.run";
pub const RECEIPTS_TOOL: &str = "compute.receipts";
pub const DEPOSIT_TOOL: &str = "compute.deposit";
pub const BALANCE_TOOL: &str = "compute.balance";
pub const DISPUTE_TOOL: &str = "compute.dispute";
pub const VERIFY_TOOL: &str = "compute.verify";
pub const WITHDRAW_TOOL: &str = "compute.withdraw";
pub const WITHDRAWALS_TOOL: &str = "compute.withdrawals";
pub const STREAM_START_TOOL: &str = "compute.stream_start";
pub const STREAM_POLL_TOOL: &str = "compute.stream_poll";
pub const CAPACITY_TOOL: &str = "compute.capacity";
pub const CANCEL_TOOL: &str = "compute.cancel";
pub const OUTPUT_TOOL: &str = "compute.output";

/// The trailing clause of a "not served" message. A failed job's operator
/// cause (`detail`) is the precise "why" and supersedes the coordinator's
/// coarse refund `reason`; a coordinator-side refund has only the reason.
fn not_served_suffix(reason: Option<&str>, detail: Option<&str>) -> String {
    if let Some(detail) = detail {
        format!(": {detail}")
    } else if let Some(reason) = reason {
        format!(" ({reason})")
    } else {
        String::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BuyerError {
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("coordinator: {0}")]
    Coordinator(String),
    /// The coordinator answered the submission with a refusal that no
    /// retry of the same envelope heals by itself. No funds are held:
    /// a job the coordinator knows is echoed 200 before any admission
    /// gate, so an answered refusal proves this job never landed. A
    /// 402 heals when the buyer tops up; a 400 is the coordinator's
    /// verdict on the envelope bytes themselves and will repeat
    /// forever — a keyed purchase should free its key on it.
    #[error("coordinator refused the submission ({status}): {}", coordinator_reason(.body.as_str()))]
    SubmitRefused { status: u16, body: String },
    #[error("job {job_id} was not served: {status}{}", not_served_suffix(.reason.as_deref(), .detail.as_deref()))]
    NotServed {
        job_id: Uuid,
        status: String,
        /// The coordinator's `refund_reason` when it served one —
        /// `refunded (buyer_cancelled)` and `refunded
        /// (deadline_expired)` call for different retries, and the
        /// bare phase can't tell them apart.
        reason: Option<String>,
        /// The operator's own cause for an execution failure, taken from
        /// the failure receipt's signed output — "why", where `reason`
        /// is only "what". Present only for a `failed` job whose receipt
        /// and output re-verify locally; `None` for a coordinator-side
        /// refund (deadline, cancel, no operator), which carries no
        /// operator statement.
        detail: Option<String>,
    },
    #[error("no receipt for job {0} before its deadline")]
    ReceiptTimeout(Uuid),
    #[error("receipt verification failed: {0}")]
    Verification(String),
    /// A read against the buyer's own Solana RPC
    /// ([`fetch_payout_transaction`]) failed. That endpoint is the
    /// buyer's, set through [`BuyerConfig::rpc_url`] and deliberately not
    /// the coordinator's — the buyer picks it so the counterparty can't
    /// vouch for its own transfers. Named apart so a down or misconfigured
    /// RPC points at the RPC, not at a coordinator that is fine.
    #[error("solana rpc: {0}")]
    Rpc(String),
    /// The request never reached the coordinator — connection refused,
    /// timed out, or the host didn't resolve. Its own case so a surface
    /// diagnoses a down coordinator (the first thing a buyer hits when
    /// the URL is wrong or the service is offline) instead of relaying
    /// reqwest's "error sending request for url (…)", which repeats the
    /// full URL and reads like an internal stack trace.
    #[error(
        "coordinator at {url} {why} while trying to {doing} — it may be down, \
         or COVENANT_COMPUTE_COORDINATOR_URL may be wrong"
    )]
    Unreachable {
        doing: &'static str,
        url: String,
        why: &'static str,
    },
}

impl BuyerError {
    /// True when this dispatch failure proves the purchase concluded
    /// with no money moved, so a keyed purchase may free its key and
    /// let a retry honestly buy again. Two shapes qualify: a job that
    /// ran to an unpaid conclusion (refunded, rejected, failed), and a
    /// submission the coordinator refused with a verdict on the
    /// envelope itself — a known job is echoed 200 before any admission
    /// gate, so an answered refusal proves the job never landed, and
    /// re-submitting the same bytes would only repeat it. Funding-state
    /// refusals (402 underfunded, 409 subsidy exhausted) stay bound:
    /// the same envelope succeeds once the money side heals.
    pub fn concludes_purchase_unpaid(&self) -> bool {
        match self {
            BuyerError::NotServed { .. } => true,
            BuyerError::SubmitRefused { status, .. } => *status != 402 && *status != 409,
            _ => false,
        }
    }

    /// The 402 a coordinator answers when the buyer's balance cannot
    /// cover the hold — the first thing a new buyer hits on a prefunded
    /// deployment, before it has deposited anything. It heals the moment
    /// the buyer tops up, so a surface can name its own deposit path
    /// instead of leaving the raw shortfall as the last word.
    pub fn is_underfunded(&self) -> bool {
        matches!(self, BuyerError::SubmitRefused { status: 402, .. })
    }

    /// Classifies a `reqwest` send failure into an [`Unreachable`] naming
    /// the coordinator once, what the buyer was doing, and why the reach
    /// failed. Use it at every `.send().await` that talks to the
    /// coordinator; `doing` reads into "while trying to {doing}".
    ///
    /// [`Unreachable`]: BuyerError::Unreachable
    fn unreachable(coordinator_url: &str, doing: &'static str, e: &reqwest::Error) -> Self {
        let why = if e.is_timeout() {
            "timed out"
        } else if e.is_connect() {
            "refused the connection"
        } else {
            "could not be reached"
        };
        BuyerError::Unreachable {
            doing,
            url: coordinator_url.trim_end_matches('/').to_string(),
            why,
        }
    }
}

/// The HTTP client a compute buyer should hold: 10s connect / 30s
/// call timeouts sized for dispatch-and-poll traffic, and every
/// request stamped with this build's wire version — a coordinator
/// that raised its floor past a breaking change then refuses by name
/// ("upgrade this client") instead of failing a parse. The buyer
/// functions accept any `reqwest::Client`, but one built elsewhere
/// declares no version and reads as a pre-versioning client.
pub fn http_client() -> reqwest::Client {
    http_client_with_timeout(Duration::from_secs(30))
}

/// [`http_client`] with a caller-chosen per-request timeout. A caller
/// that runs several sequential coordinator calls inside one outer
/// deadline — the control plane opens a lease then reads it back within
/// its own request timeout — sizes this so the whole sequence resolves
/// in time to unwind its own state on a stall, instead of being cut off
/// mid-call when that outer deadline drops the task.
pub fn http_client_with_timeout(call_timeout: Duration) -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::HeaderName::from_static(
            covenant_compute_protocol::PROTOCOL_VERSION_HEADER,
        ),
        reqwest::header::HeaderValue::from(covenant_compute_protocol::PROTOCOL_VERSION),
    );
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(call_timeout)
        .default_headers(headers)
        .build()
        .expect("reqwest client builds with default TLS backend")
}

/// Coordinator endpoint + polling cadence — everything the network path
/// needs; spend policy stays with each consumer.
#[derive(Debug, Clone)]
pub struct BuyerConfig {
    pub coordinator_url: String,
    pub poll_interval: Duration,
    /// Demand-side partner attribution (C8): the code of the partner
    /// who brought this buyer, signed into every job envelope this
    /// config dispatches. Rev-share accrues out of the coordinator's
    /// fee, never on top of the buyer's price.
    pub referral_code: Option<String>,
    /// The buyer's own Solana RPC endpoint, used only to read payout
    /// transactions back off the chain ([`verify_payout`]). Deliberately
    /// not taken from the coordinator — an endpoint the counterparty
    /// picks could vouch for the counterparty's own transfers. `None`
    /// leaves every off-chain verification working and makes the
    /// on-chain fetch an explicit-config error.
    pub rpc_url: Option<String>,
}

/// The buyer-side spend guardrail a standalone front door enforces
/// without a full budget subsystem: a per-call price ceiling and an
/// optional cumulative session cap. The `covenant-compute-mcp` server
/// and the OpenAI-compatible endpoint each hold one, so the two refuse
/// an over-cap or over-budget buy through one path instead of forking
/// the arithmetic. `covenantd`'s native capability uses its own
/// budget/settlement accounting and does not go through here. Every
/// check refuses *before* a dispatch, so nothing ever spends past a
/// bound.
#[derive(Debug)]
pub struct SpendCaps {
    max_price_micro_usdc: u64,
    max_total_micro_usdc: Option<u64>,
    spent_micro_usdc: AtomicU64,
    /// Settled spend plus every outstanding [`SpendReservation`], i.e. the
    /// total that counts against the session cap right now. Admission is a
    /// single compare-and-swap on this one counter, so two concurrent
    /// callers on a shared server can't both read stale headroom and slip a
    /// buy past the cap: whichever loses the swap re-checks against the
    /// winner's committed total. Tracking spend and reservations as two
    /// separate counters and swapping on one of them cannot give that
    /// guarantee (the unswapped counter can move under the check).
    committed_micro_usdc: AtomicU64,
}

impl SpendCaps {
    /// `max_price_micro_usdc` bounds any one call; `max_total_micro_usdc`,
    /// when set, bounds the whole session's cumulative committed spend.
    pub fn new(max_price_micro_usdc: u64, max_total_micro_usdc: Option<u64>) -> Self {
        Self {
            max_price_micro_usdc,
            max_total_micro_usdc,
            spent_micro_usdc: AtomicU64::new(0),
            committed_micro_usdc: AtomicU64::new(0),
        }
    }

    /// The per-call ceiling, for the tool-spec descriptions and preview
    /// quotes that show a buyer the bound they buy under.
    pub fn max_price_micro_usdc(&self) -> u64 {
        self.max_price_micro_usdc
    }

    /// Refuse a call whose offered price is over the per-call ceiling.
    pub fn per_call_refusal(&self, price: u64) -> Option<String> {
        (price > self.max_price_micro_usdc).then(|| {
            format!(
                "offered price {price} micro-USDC exceeds the per-call ceiling {}",
                self.max_price_micro_usdc
            )
        })
    }

    /// Refuse a call that would push cumulative spend past the session
    /// cap. `in_flight_micro_usdc` is spend a caller tracks separately
    /// that is committed but not yet settled — a streaming offer still
    /// draining — on top of any [`SpendReservation`]s and settled spend
    /// this type already holds. Callers read their own in-flight figure
    /// first and pass it in, so settled spend is loaded last: a buy that
    /// settled in the gap is then always in at least one snapshot.
    ///
    /// For a server that dispatches concurrently, prefer
    /// [`SpendCaps::try_reserve`]: it makes the check and the hold one
    /// atomic step, which this stateless check cannot.
    pub fn session_refusal(&self, price: u64, in_flight_micro_usdc: u64) -> Option<String> {
        let total_cap = self.max_total_micro_usdc?;
        let committed = self.committed_micro_usdc.load(Ordering::SeqCst);
        let spent = self.spent_micro_usdc.load(Ordering::SeqCst);
        let reserved = committed.saturating_sub(spent);
        let in_flight = reserved.saturating_add(in_flight_micro_usdc);
        (spent.saturating_add(in_flight).saturating_add(price) > total_cap).then(|| {
            format!(
                "session spend cap reached: {spent} spent + {in_flight} in flight + {price} \
                 offered exceeds {total_cap} micro-USDC"
            )
        })
    }

    /// Atomically reserve `price` against the per-call ceiling and the
    /// session cap, counting settled spend plus every outstanding
    /// reservation. On success the returned guard holds the reservation
    /// against the cap until it is [settled] or dropped; on refusal
    /// nothing is reserved and the message names the bound that was hit.
    /// This is the concurrency-safe path a shared HTTP front door takes,
    /// where a plain [`SpendCaps::session_refusal`] then dispatch would
    /// let two requests both pass the check before either records spend.
    /// The guard owns a handle to these caps, so it can travel into the
    /// background task that drains a streaming buy and settle there.
    ///
    /// [settled]: SpendReservation::settle
    pub fn try_reserve(self: &Arc<Self>, price: u64) -> Result<SpendReservation, String> {
        self.reserve_amount(price)?;
        Ok(SpendReservation {
            caps: Arc::clone(self),
            reserved: price,
            active: true,
        })
    }

    /// The reservation arithmetic behind [`SpendCaps::try_reserve`]: on
    /// `Ok`, `price` has been added to the outstanding-reservation total
    /// and must be balanced by exactly one settle or release.
    fn reserve_amount(&self, price: u64) -> Result<(), String> {
        if let Some(msg) = self.per_call_refusal(price) {
            return Err(msg);
        }
        let Some(total_cap) = self.max_total_micro_usdc else {
            self.committed_micro_usdc.fetch_add(price, Ordering::SeqCst);
            return Ok(());
        };
        loop {
            let committed = self.committed_micro_usdc.load(Ordering::SeqCst);
            if committed.saturating_add(price) > total_cap {
                let spent = self.spent_micro_usdc.load(Ordering::SeqCst);
                let reserved = committed.saturating_sub(spent);
                return Err(format!(
                    "session spend cap reached: {spent} spent + {reserved} in flight + {price} \
                     offered exceeds {total_cap} micro-USDC"
                ));
            }
            // Swap the exact counter the check reads. If any concurrent
            // reserve, settle, or release moved `committed` since the load,
            // the swap fails and the check re-runs against the new total —
            // so a stale read can never admit a buy the current headroom
            // won't hold. Post-swap `committed` is `loaded + price`, which
            // this iteration proved is within the cap: the total never
            // crosses it.
            if self
                .committed_micro_usdc
                .compare_exchange_weak(
                    committed,
                    committed + price,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
            {
                return Ok(());
            }
        }
    }

    /// Turn a reservation into settled spend. The buyer is charged the
    /// offer it held — the coordinator escrows the envelope price and
    /// releases that same amount on success (`http.rs`'s release pays the
    /// held amount, not the receipt's claimed price), so the whole hold
    /// becomes settled spend and none returns to the session headroom. The
    /// committed total is unchanged: the amount moves from "reserved" to
    /// "spent" without ever crossing the cap.
    fn settle_reserved(&self, reserved: u64) {
        self.spent_micro_usdc.fetch_add(reserved, Ordering::SeqCst);
    }

    /// Release a reservation of `reserved` without charging — the buy
    /// failed, so the held amount returns to the session's headroom.
    fn release_reserved(&self, reserved: u64) {
        self.committed_micro_usdc
            .fetch_sub(reserved, Ordering::SeqCst);
    }

    /// Commit a settled spend against the running session total. For the
    /// concurrent path use [`SpendCaps::try_reserve`] instead, which
    /// holds the amount against the cap from the check onward.
    pub fn record_spend(&self, amount_micro_usdc: u64) {
        self.spent_micro_usdc
            .fetch_add(amount_micro_usdc, Ordering::SeqCst);
        self.committed_micro_usdc
            .fetch_add(amount_micro_usdc, Ordering::SeqCst);
    }

    /// Cumulative settled spend this session, in micro-USDC. Excludes
    /// outstanding reservations.
    pub fn spent_micro_usdc(&self) -> u64 {
        self.spent_micro_usdc.load(Ordering::SeqCst)
    }
}

/// A hold on the session cap, taken by [`SpendCaps::try_reserve`] and
/// counting against the cap until it is [settled] or dropped. Dropping
/// without settling releases the hold — the buy failed, so nothing is
/// charged. Settling converts it to spend at the offer it held, which is
/// what the coordinator charges the buyer. It owns a handle to its caps,
/// so it stays valid after the request that took it hands the work to a
/// background drain task.
///
/// [settled]: SpendReservation::settle
#[derive(Debug)]
#[must_use = "a reservation holds against the session cap until settled or dropped"]
pub struct SpendReservation {
    caps: Arc<SpendCaps>,
    reserved: u64,
    active: bool,
}

impl SpendReservation {
    /// Convert the hold into settled spend at the offer it held — the
    /// price the coordinator actually charges the buyer. The coordinator
    /// escrows and releases the envelope price, so a completing operator's
    /// receipt can never lower the charge, and it must not lower what
    /// counts against the session cap either: settling at a node-reported
    /// receipt price would let a node under-report its way past a buyer's
    /// session limit. Releases the reservation and commits the spend in
    /// one step.
    pub fn settle(mut self) {
        self.caps.settle_reserved(self.reserved);
        self.active = false;
    }
}

impl Drop for SpendReservation {
    fn drop(&mut self) {
        if self.active {
            self.caps.release_reserved(self.reserved);
        }
    }
}

/// One job, ready to sign and dispatch. `kind` decides which operators
/// can serve it: an `InferenceCall` runs `input` as a prompt (or packed
/// chat) against `model`, a `BatchJob` runs `input`'s first text block
/// as a command on a batch node — where and how isolated is the node's
/// executor configuration, not the buyer's choice.
///
/// `model`, `gpu_class`, and `min_vram_gb` are the hardware/model ask:
/// each narrows the operators the coordinator will match, and an absent
/// one places no constraint. `gpu_class` and `min_vram_gb` matter most
/// for a `BatchJob`, which has no model to route by — without them a
/// GPU workload can land on the cheapest node, CPU-only included.
#[derive(Debug, Clone)]
pub struct JobRequest {
    pub kind: JobKind,
    pub input: Vec<Content>,
    pub model: Option<String>,
    /// The GPU class an operator must declare to serve this job, matched
    /// exactly against a node's advertised hardware model (`"cpu"` for a
    /// CPU-only node). The requestable values are published per row by
    /// `/federation/capacity`.
    pub gpu_class: Option<String>,
    /// The minimum VRAM, in whole GB, an operator must declare.
    pub min_vram_gb: Option<u32>,
    /// The lowest operator reputation, in basis points (`8_000` = 80%),
    /// the coordinator may route this job to. Absent places no floor
    /// beyond the coordinator's own; the effective floor is the higher of
    /// the two. Rides the signed envelope, so it is the buyer's to set.
    pub min_reputation_bps: Option<u32>,
    pub price_micro_usdc: u64,
    pub deadline_ms: u64,
}

/// A completed job whose receipt and output survived local
/// re-verification.
#[derive(Debug)]
pub struct DispatchOutcome {
    pub envelope: SignedJobEnvelope,
    pub receipt: SignedWorkReceipt,
    pub output: Vec<Content>,
    /// The payout that honored the receipt, when it had already landed
    /// by the poll that served the receipt. `None` means the push is
    /// still in flight (the coordinator's retry sweep keeps at it) —
    /// re-poll the job or the history listing later for the signature.
    pub payout: Option<PayoutInfo>,
}

/// The coordinator's account of where a job's money went; mirrors its
/// `JobPayoutView`. `memo` is recomputable from the signed receipt via
/// [`SignedWorkReceipt::payout_memo`], and [`verify_payout_onchain`]
/// checks the chain actually carries it — nothing here needs to be
/// taken on the coordinator's word.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct PayoutInfo {
    pub amount_micro_usdc: u64,
    /// `None` until the transfer confirms, or forever for backends
    /// that record intent without touching a chain.
    pub tx_signature: Option<String>,
    pub memo: String,
    pub recorded_at_ms: u64,
}

/// The `compute.infer` MCP tool spec — the single source of truth for
/// its schema, shared by `covenantd`'s advertised tool list and the
/// standalone MCP server.
pub fn infer_tool_spec(max_price_micro_usdc: u64) -> ToolSpec {
    ToolSpec {
        name: INFER_TOOL.into(),
        description: format!(
            "Dispatch a paid inference job to the Covenant compute network and return its \
             output together with the operator's signed, hash-verified work receipt. \
             Price per call is capped at {max_price_micro_usdc} micro-USDC."
        ),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Raw completion input. Exactly one of prompt or messages."
                },
                "images": {
                    "type": "array",
                    "description": "Base64-encoded images to attach to `prompt`, making it a \
                                    vision request. Attach images per-message via `messages` for \
                                    a multi-turn conversation.",
                    "items": { "type": "string" }
                },
                "messages": {
                    "type": "array",
                    "description": "Chat conversation for the executing node's chat endpoint, \
                                    oldest first. Exactly one of prompt or messages.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "role": {
                                "type": "string",
                                "enum": ["system", "user", "assistant", "tool"]
                            },
                            "content": {
                                "type": "string",
                                "description": "The message text; omit on an assistant turn \
                                                that only calls tools."
                            },
                            "images": {
                                "type": "array",
                                "description": "Base64-encoded images attached to this message \
                                                for a vision model.",
                                "items": { "type": "string" }
                            },
                            "tool_calls": {
                                "type": "array",
                                "description": "On a replayed assistant turn, the tool calls it \
                                                made (id, function name, JSON arguments string).",
                                "items": { "type": "object" }
                            },
                            "tool_call_id": {
                                "type": "string",
                                "description": "On a tool message, which assistant tool call this \
                                                result answers."
                            }
                        },
                        "required": ["role"]
                    },
                    "minItems": 1
                },
                "model": {
                    "type": "string",
                    "description": "Model id the job requires; omit to accept any node"
                },
                "gpu_class": {
                    "type": "string",
                    "description": "Require a specific GPU class (e.g. rtx-4090, h100), or cpu \
                                    for a CPU-only node; the requestable values are what \
                                    compute.capacity lists. Omit to accept any hardware."
                },
                "min_vram_gb": {
                    "type": "integer",
                    "description": "Require at least this much VRAM, in whole GB. Omit for no floor."
                },
                "min_reputation_bps": {
                    "type": "integer",
                    "description": "Require operators rated at least this many basis points \
                                    (8000 = 80%); the coordinator routes only to the proven pool \
                                    above it, and the job refunds rather than run on a lesser \
                                    operator. Omit for no floor. 1..=10000."
                },
                "price_micro_usdc": {
                    "type": "integer",
                    "description": "Offered price in micro-USDC; defaults to the cheapest matching operator's ask"
                },
                "deadline_ms": {
                    "type": "integer",
                    "description": "Job deadline in milliseconds; defaults to the configured deadline"
                },
                "temperature": {
                    "type": "number",
                    "description": "Sampling temperature in [0, 2]; 0 picks the most likely \
                                    token every step. Omit for the model's default."
                },
                "top_p": {
                    "type": "number",
                    "description": "Nucleus sampling mass in (0, 1]. Omit for the model's default."
                },
                "max_tokens": {
                    "type": "integer",
                    "description": "Cap on generated tokens — bounds output length, cost and \
                                    latency. Omit for the model's default."
                },
                "seed": {
                    "type": "integer",
                    "description": "Sampling seed; with temperature 0 makes runs repeatable on \
                                    the same node."
                },
                "presence_penalty": {
                    "type": "number",
                    "description": "In [-2, 2]; positive values discourage reusing tokens \
                                    already in the text, pushing toward new topics. Omit for \
                                    the model's default."
                },
                "frequency_penalty": {
                    "type": "number",
                    "description": "In [-2, 2]; positive values scale down tokens by how often \
                                    they've already appeared, damping repetition. Omit for the \
                                    model's default."
                },
                "logprobs": {
                    "type": "integer",
                    "description": "Return per-token log probabilities: the number of \
                                    most-likely alternatives to report per token (0..=20; 0 = the \
                                    chosen token only). Omit to leave them off. The probabilities \
                                    ride the signed receipt's attested output."
                },
                "stop": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Up to 4 short sequences; generation halts before any of \
                                    them appears"
                },
                "response_format": {
                    "type": "object",
                    "description": "Constrain the reply's shape. {\"type\": \"json_object\"} asks \
                                    for any valid JSON; {\"type\": \"json_schema\", \"name\": \
                                    \"...\", \"schema\": {...}} constrains it to that JSON schema \
                                    (structured output). Omit for free-form text."
                },
                "tools": {
                    "type": "array",
                    "description": "Functions the model may call, OpenAI's tools shape \
                                    ({type: function, function: {name, description, parameters}}). \
                                    The reply may then be tool calls instead of prose; run them \
                                    and send the results back as tool messages to continue.",
                    "items": { "type": "object" }
                },
                "tool_choice": {
                    "description": "Whether the model may (\"auto\"), must (\"required\"), or \
                                    must not (\"none\") call a tool, or an object naming one \
                                    function to force. Ignored without tools."
                },
                "idempotency_key": {
                    "type": "string",
                    "description": "Makes the purchase exactly-once: calls repeating this key \
                                    return the first call's result and never pay twice, even \
                                    across a daemon restart. A key names one purchase — an \
                                    explicit argument that contradicts it is refused; use a \
                                    fresh key for new work. 1..=128 bytes."
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "Preview only. Resolve the price, routing and deadline this \
                                    call would use and return them without dispatching a job or \
                                    spending anything, so an agent can weigh the cost and \
                                    feasibility first. Refuses when no operator can serve the \
                                    job. Can't combine with idempotency_key — a preview reserves \
                                    no purchase."
                }
            }
        }),
    }
}

/// The `compute.infer` argument shape, shared by both consumers.
#[derive(Debug, Deserialize)]
pub struct InferArgs {
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub messages: Option<Vec<ChatMessage>>,
    /// Base64-encoded images to attach to a `prompt`, turning it into a
    /// vision request the executing node's chat endpoint serves. Pass
    /// images per-message via `messages` instead when building a multi-turn
    /// conversation.
    #[serde(default)]
    pub images: Option<Vec<String>>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub gpu_class: Option<String>,
    #[serde(default)]
    pub min_vram_gb: Option<u32>,
    #[serde(default)]
    pub min_reputation_bps: Option<u32>,
    #[serde(default)]
    pub price_micro_usdc: Option<u64>,
    #[serde(default)]
    pub deadline_ms: Option<u64>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub seed: Option<i64>,
    #[serde(default)]
    pub presence_penalty: Option<f64>,
    #[serde(default)]
    pub frequency_penalty: Option<f64>,
    /// Ask for per-token log probabilities: `Some(n)` returns them with
    /// the `n` most-likely alternatives per token (`0..=20`). The
    /// probabilities ride the signed receipt's attested output.
    #[serde(default)]
    pub logprobs: Option<u32>,
    #[serde(default)]
    pub stop: Option<Vec<String>>,
    /// Constrain the shape of generated output — JSON-object mode or a
    /// named JSON schema. When set, the job carries it in the signed
    /// generation block and the backend is asked to conform.
    #[serde(default)]
    pub response_format: Option<ResponseFormat>,
    /// Functions the model may call. When set, the job carries a signed
    /// tools block and the reply may be tool calls instead of prose.
    #[serde(default)]
    pub tools: Option<Vec<ToolDefinition>>,
    /// Whether the model may, must, or must not call a tool. Ignored
    /// unless `tools` is set.
    #[serde(default)]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
}

impl InferArgs {
    /// The job input these arguments describe — one text block for a
    /// raw prompt, the packed conversation for chat, plus the signed
    /// generation block when any sampling knob is set. Exactly one of
    /// prompt or messages must be present, and a meaningless knob is
    /// refused here; both callers refuse before signing anything
    /// otherwise.
    pub fn input(&self) -> Result<Vec<Content>, String> {
        let images = self.images.as_deref().unwrap_or_default();
        for image in images {
            base64::engine::general_purpose::STANDARD
                .decode(image)
                .map_err(|_| "an attached image is not valid base64".to_string())?;
        }
        let mut input = match (&self.prompt, &self.messages) {
            (Some(prompt), None) if images.is_empty() => vec![Content::text(prompt.clone())],
            (Some(prompt), None) => {
                // A prompt with images is a vision turn, so it travels as a
                // chat conversation rather than the raw-prompt block.
                chat_input(vec![ChatMessage::user_with_images(
                    prompt.clone(),
                    images.to_vec(),
                )])
            }
            (None, Some(messages)) => {
                if !images.is_empty() {
                    return Err(
                        "attach images to a message's own content when passing messages, \
                         not the top-level images field"
                            .into(),
                    );
                }
                if messages.is_empty() {
                    return Err("messages must not be empty".into());
                }
                chat_input(messages.clone())
            }
            (Some(_), Some(_)) => return Err("pass either prompt or messages, not both".into()),
            (None, None) => return Err("one of prompt or messages is required".into()),
        };
        let params = GenerationParams {
            temperature: self.temperature,
            top_p: self.top_p,
            max_tokens: self.max_tokens,
            seed: self.seed,
            presence_penalty: self.presence_penalty,
            frequency_penalty: self.frequency_penalty,
            // An empty stop list is a no-op — the dialects (OpenAI `stop: []`,
            // Anthropic `stop_sequences: []`) and a hand-built `compute.infer`
            // all treat it so — but the protocol refuses an empty one, so drop
            // it to None here rather than turn a valid request into a 400.
            stop: self.stop.clone().filter(|s| !s.is_empty()),
            response_format: self.response_format.clone(),
            logprobs: self.logprobs,
        };
        if !params.is_empty() {
            input.push(generation_input(params).map_err(|e| e.to_string())?);
        }
        match &self.tools {
            Some(tools) if !tools.is_empty() => input.push(
                tools_input(tools.clone(), self.tool_choice.clone()).map_err(|e| e.to_string())?,
            ),
            // No tools to offer. A `tool_choice` that forces a call
            // (`required` or a named function) cannot be honored with no
            // tool to bind, so refuse it rather than let the job settle as
            // a plain completion the buyer believes forced a call.
            // `auto`/`none` force nothing and drop quietly.
            _ => {
                if matches!(
                    self.tool_choice,
                    Some(ToolChoice::Mode(ToolChoiceMode::Required)) | Some(ToolChoice::Named(_))
                ) {
                    return Err("tool_choice forces a tool call but no tools were provided".into());
                }
            }
        }
        Ok(input)
    }
}

/// The `compute.run` MCP tool spec — `compute.infer`'s batch sibling:
/// one command in, its output out, executed to completion on a batch
/// node (subprocess- or container-isolated, the operator's choice) and
/// paid through the same verified-receipt pipeline.
pub fn run_tool_spec(max_price_micro_usdc: u64) -> ToolSpec {
    ToolSpec {
        name: RUN_TOOL.into(),
        description: format!(
            "Run one shell command to completion as a paid batch job on the Covenant \
             compute network and return its output together with the operator's signed, \
             hash-verified work receipt. The command executes on a stranger's machine \
             (isolated, no network egress on container nodes) — never send secrets in it. \
             Price per call is capped at {max_price_micro_usdc} micro-USDC."
        ),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command the batch node runs to completion"
                },
                "gpu_class": {
                    "type": "string",
                    "description": "Require a specific GPU class (e.g. rtx-4090, h100), or cpu \
                                    for a CPU-only node; the requestable values are what \
                                    compute.capacity lists. Omit to accept any hardware. A batch \
                                    job has no model to route by, so this is how a GPU workload \
                                    avoids landing on a CPU-only node."
                },
                "min_vram_gb": {
                    "type": "integer",
                    "description": "Require at least this much VRAM, in whole GB. Omit for no floor."
                },
                "min_reputation_bps": {
                    "type": "integer",
                    "description": "Require operators rated at least this many basis points \
                                    (8000 = 80%); the coordinator routes only to the proven pool \
                                    above it, and the job refunds rather than run on a lesser \
                                    operator. Omit for no floor. 1..=10000."
                },
                "price_micro_usdc": {
                    "type": "integer",
                    "description": "Offered price in micro-USDC; defaults to the cheapest matching operator's ask"
                },
                "deadline_ms": {
                    "type": "integer",
                    "description": "Job deadline in milliseconds; defaults to the configured deadline"
                },
                "idempotency_key": {
                    "type": "string",
                    "description": "Makes the purchase exactly-once: calls repeating this key \
                                    return the first call's result and never pay twice, even \
                                    across a daemon restart. A key names one purchase — an \
                                    explicit argument that contradicts it is refused; use a \
                                    fresh key for new work. 1..=128 bytes."
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "Preview only. Resolve the price, routing and deadline this \
                                    call would use and return them without dispatching a job or \
                                    spending anything. Refuses when no operator can serve the \
                                    job. Can't combine with idempotency_key — a preview reserves \
                                    no purchase."
                }
            },
            "required": ["command"]
        }),
    }
}

/// The `compute.run` argument shape, shared by both consumers.
#[derive(Debug, Deserialize)]
pub struct RunArgs {
    pub command: String,
    #[serde(default)]
    pub gpu_class: Option<String>,
    #[serde(default)]
    pub min_vram_gb: Option<u32>,
    #[serde(default)]
    pub min_reputation_bps: Option<u32>,
    #[serde(default)]
    pub price_micro_usdc: Option<u64>,
    #[serde(default)]
    pub deadline_ms: Option<u64>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
}

impl RunArgs {
    /// The job input these arguments describe — the command as the one
    /// text block a batch executor runs. Refused empty before anything
    /// is signed.
    pub fn input(&self) -> Result<Vec<Content>, String> {
        let command = self.command.trim();
        if command.is_empty() {
            return Err("command must not be empty".into());
        }
        Ok(vec![Content::text(command.to_string())])
    }
}

/// The `compute.embed` MCP tool spec — the retrieval/semantic-memory
/// slice: text in, an embedding vector out, on an embedding-serving
/// operator. Shared by `covenantd` and the standalone MCP server.
pub fn embed_tool_spec(max_price_micro_usdc: u64) -> ToolSpec {
    ToolSpec {
        name: EMBED_TOOL.into(),
        description: format!(
            "Embed text into a vector on the Covenant compute network and return it together \
             with the operator's signed, hash-verified work receipt — the retrieval and \
             semantic-memory primitive. Embed one `text`, or a `texts` batch bought as a single \
             paid job, one vector out per input in order (up to {MAX_EMBED_TEXTS}) — index a \
             corpus in one call and one payment. Name the embedding model so every vector in a \
             store comes from the same one; mixing models makes vectors incomparable. Price per \
             call is capped at {max_price_micro_usdc} micro-USDC."
        ),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "A single text to embed. Use texts for a batch, not both."
                },
                "texts": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "A batch of texts to embed in one paid job, one vector out per \
                                    entry in order. Use instead of text, not with it."
                },
                "model": {
                    "type": "string",
                    "description": "Embedding model the job requires (e.g. nomic-embed-text); \
                                    omit to accept any embedding operator"
                },
                "gpu_class": {
                    "type": "string",
                    "description": "Require a specific GPU class (e.g. rtx-4090, h100), or cpu \
                                    for a CPU-only node; the requestable values are what \
                                    compute.capacity lists. Omit to accept any hardware."
                },
                "min_vram_gb": {
                    "type": "integer",
                    "description": "Require at least this much VRAM, in whole GB. Omit for no floor."
                },
                "min_reputation_bps": {
                    "type": "integer",
                    "description": "Require operators rated at least this many basis points \
                                    (8000 = 80%); the coordinator routes only to the proven pool \
                                    above it, and the job refunds rather than run on a lesser \
                                    operator. Omit for no floor. 1..=10000."
                },
                "price_micro_usdc": {
                    "type": "integer",
                    "description": "Offered price in micro-USDC; defaults to the cheapest matching operator's ask"
                },
                "deadline_ms": {
                    "type": "integer",
                    "description": "Job deadline in milliseconds; defaults to the configured deadline"
                },
                "idempotency_key": {
                    "type": "string",
                    "description": "Makes the purchase exactly-once: calls repeating this key \
                                    return the first call's result and never pay twice, even \
                                    across a daemon restart. A key names one purchase — an \
                                    explicit argument that contradicts it is refused; use a \
                                    fresh key for new work. 1..=128 bytes."
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "Preview only. Resolve the price, routing and deadline this \
                                    call would use and return them without dispatching a job or \
                                    spending anything. Refuses when no operator can serve the \
                                    job. Can't combine with idempotency_key — a preview reserves \
                                    no purchase."
                }
            }
        }),
    }
}

/// The most texts one `compute.embed` call may carry. The whole batch
/// rides a single inline job envelope, so the bound keeps one request
/// under the network's frame limit; a larger corpus splits across calls.
pub const MAX_EMBED_TEXTS: usize = 256;

/// The `compute.embed` argument shape, shared by both consumers. Embed a
/// single `text`, or a `texts` batch bought as one paid job — one vector
/// out per input, in order.
#[derive(Debug, Deserialize)]
pub struct EmbedArgs {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub texts: Option<Vec<String>>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub gpu_class: Option<String>,
    #[serde(default)]
    pub min_vram_gb: Option<u32>,
    #[serde(default)]
    pub min_reputation_bps: Option<u32>,
    #[serde(default)]
    pub price_micro_usdc: Option<u64>,
    #[serde(default)]
    pub deadline_ms: Option<u64>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
}

impl EmbedArgs {
    /// The job input these arguments describe: one `Content::Text` block
    /// per text to embed, in order. Exactly one of `text` or `texts`
    /// carries the input; blank text and an over-long batch are refused
    /// before anything is signed.
    pub fn input(&self) -> Result<Vec<Content>, String> {
        let texts = match (&self.text, &self.texts) {
            (Some(_), Some(_)) => return Err("pass either text or texts, not both".into()),
            (Some(one), None) => {
                let text = one.trim();
                if text.is_empty() {
                    return Err("text must not be empty".into());
                }
                return Ok(vec![Content::text(text.to_string())]);
            }
            (None, Some(many)) => many,
            (None, None) => return Err("text or texts is required".into()),
        };
        if texts.is_empty() {
            return Err("texts must not be empty".into());
        }
        if texts.len() > MAX_EMBED_TEXTS {
            return Err(format!(
                "texts carries {} entries, past the {MAX_EMBED_TEXTS}-per-call limit; \
                 split the batch across calls",
                texts.len()
            ));
        }
        if texts.iter().any(|t| t.trim().is_empty()) {
            return Err("every text in the batch must be non-empty".into());
        }
        Ok(texts
            .iter()
            .map(|t| Content::text(t.trim().to_string()))
            .collect())
    }
}

/// The `compute.transcribe` MCP tool spec — speech to text on the
/// network, the same signed, hash-verified purchase every buying tool
/// makes, in the shape audio needs. The audio rides inline as base64 so
/// an MCP client with no filesystem access to this daemon can still buy
/// a transcription.
pub fn transcribe_tool_spec(max_price_micro_usdc: u64) -> ToolSpec {
    ToolSpec {
        name: TRANSCRIBE_TOOL.into(),
        description: format!(
            "Transcribe speech to text on the Covenant compute network and return the transcript \
             together with the operator's signed, hash-verified work receipt. Pass the audio \
             inline as base64 in `audio_base64`; a 16 kHz mono WAV decodes on every operator, \
             other containers depend on the operator's build. Set `language` to the spoken \
             language's ISO-639-1 code (e.g. en) to skip detection, or `translate` to render the \
             speech in English instead of its own language. Name a `model` (e.g. whisper-base.en) \
             to pin one, or omit it to accept any transcription operator. Price per call is capped \
             at {max_price_micro_usdc} micro-USDC."
        ),
        input_schema: serde_json::json!({
            "type": "object",
            "required": ["audio_base64"],
            "properties": {
                "audio_base64": {
                    "type": "string",
                    "description": "The audio to transcribe, base64-encoded. A 16 kHz mono WAV \
                                    reads on every operator."
                },
                "format": {
                    "type": "string",
                    "description": "Container hint for the operator's log and backend selection \
                                    (e.g. wav, mp3). Advisory: the backend reads the bytes, so a \
                                    wrong or absent hint never changes the result."
                },
                "language": {
                    "type": "string",
                    "description": "Spoken language as an ISO-639-1 code (e.g. en) to skip \
                                    detection. Omit, leave blank, or pass auto to let the model \
                                    detect it."
                },
                "translate": {
                    "type": "boolean",
                    "description": "Translate the speech into English instead of transcribing it \
                                    in its own language. Defaults to false."
                },
                "timestamps": {
                    "type": "boolean",
                    "description": "Return per-segment start/end timestamps alongside the \
                                    transcript, for captioning and alignment. Defaults to false."
                },
                "model": {
                    "type": "string",
                    "description": "Transcription model the job requires (e.g. whisper-base.en); \
                                    omit to accept any transcription operator"
                },
                "gpu_class": {
                    "type": "string",
                    "description": "Require a specific GPU class (e.g. rtx-4090, h100), or cpu \
                                    for a CPU-only node; the requestable values are what \
                                    compute.capacity lists. Omit to accept any hardware."
                },
                "min_vram_gb": {
                    "type": "integer",
                    "description": "Require at least this much VRAM, in whole GB. Omit for no floor."
                },
                "min_reputation_bps": {
                    "type": "integer",
                    "description": "Require operators rated at least this many basis points \
                                    (8000 = 80%); the coordinator routes only to the proven pool \
                                    above it, and the job refunds rather than run on a lesser \
                                    operator. Omit for no floor. 1..=10000."
                },
                "price_micro_usdc": {
                    "type": "integer",
                    "description": "Offered price in micro-USDC; defaults to the cheapest matching operator's ask"
                },
                "deadline_ms": {
                    "type": "integer",
                    "description": "Job deadline in milliseconds; defaults to the configured deadline"
                },
                "idempotency_key": {
                    "type": "string",
                    "description": "Makes the purchase exactly-once: calls repeating this key \
                                    return the first call's result and never pay twice, even \
                                    across a daemon restart. A key names one purchase — an \
                                    explicit argument that contradicts it is refused; use a \
                                    fresh key for new work. 1..=128 bytes."
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "Preview only. Resolve the price, routing and deadline this \
                                    call would use and return them without dispatching a job or \
                                    spending anything. Refuses when no operator can serve the \
                                    job. Can't combine with idempotency_key — a preview reserves \
                                    no purchase."
                }
            }
        }),
    }
}

/// The `compute.transcribe` argument shape, shared by both consumers:
/// one base64 audio clip plus the routing and spend knobs every buying
/// tool takes.
#[derive(Debug, Deserialize)]
pub struct TranscribeArgs {
    pub audio_base64: String,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub translate: bool,
    #[serde(default)]
    pub timestamps: bool,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub gpu_class: Option<String>,
    #[serde(default)]
    pub min_vram_gb: Option<u32>,
    #[serde(default)]
    pub min_reputation_bps: Option<u32>,
    #[serde(default)]
    pub price_micro_usdc: Option<u64>,
    #[serde(default)]
    pub deadline_ms: Option<u64>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
}

impl TranscribeArgs {
    /// The job input these arguments describe: one transcription block
    /// carrying the base64 audio and its options. An empty clip, one
    /// past [`MAX_AUDIO_B64_BYTES`], or one that is not valid base64 is
    /// refused here — the buyer's own error, caught before anything is
    /// signed, so it never becomes an operator's reputation fault on
    /// input the buyer controls. A blank or `auto` language is detection,
    /// matching the network's other transcription surfaces.
    pub fn input(&self) -> Result<Vec<Content>, String> {
        let audio = self.audio_base64.trim();
        if audio.is_empty() {
            return Err(
                "audio_base64 is required and must carry the base64 audio to \
                        transcribe"
                    .into(),
            );
        }
        if audio.len() > MAX_AUDIO_B64_BYTES {
            return Err(format!(
                "audio is {} base64 bytes, over the {MAX_AUDIO_B64_BYTES}-byte per-call limit; \
                 send a shorter clip",
                audio.len()
            ));
        }
        base64::engine::general_purpose::STANDARD
            .decode(audio)
            .map_err(|_| "audio_base64 is not valid base64".to_string())?;
        let language = self
            .language
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto"))
            .map(str::to_string);
        let format = self
            .format
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        Ok(transcription_input(TranscriptionInput {
            audio_base64: audio.to_string(),
            format,
            language,
            translate: self.translate,
            timestamps: self.timestamps,
        }))
    }
}

/// The `compute.speak` argument shape, shared by the CLI and the MCP tool:
/// the text to synthesize plus the same voicing and spend knobs every
/// buying tool takes. The mirror of [`TranscribeArgs`], one direction over.
#[derive(Debug, Deserialize)]
pub struct SpeakArgs {
    pub text: String,
    #[serde(default)]
    pub voice: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub speed: Option<f32>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub gpu_class: Option<String>,
    #[serde(default)]
    pub min_vram_gb: Option<u32>,
    #[serde(default)]
    pub min_reputation_bps: Option<u32>,
    #[serde(default)]
    pub price_micro_usdc: Option<u64>,
    #[serde(default)]
    pub deadline_ms: Option<u64>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
}

impl SpeakArgs {
    /// The job input these arguments describe: one speech block carrying
    /// the text and its voicing. Empty text, text past the character cap,
    /// or a speed outside the accepted window is refused here — the
    /// buyer's own error, caught before anything is signed, so it never
    /// becomes an operator's reputation fault on input the buyer controls.
    /// A blank voice or container is dropped rather than sent as an empty
    /// string the backend would have to interpret.
    pub fn input(&self) -> Result<Vec<Content>, String> {
        let packed = speech_input(SpeechInput {
            text: self.text.clone(),
            voice: trimmed_option(self.voice.as_deref()),
            format: trimmed_option(self.format.as_deref()),
            speed: self.speed,
        });
        parse_speech_input(&packed).map_err(|e| e.to_string())?;
        Ok(packed)
    }
}

fn trimmed_option(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Decodes a synthesized clip and writes it into `dir`, named for its job,
/// returning the path and byte count. The operator's audio rides back
/// base64 inside the receipt-verified output; a caller — a terminal user
/// or an agent — wants the raw bytes on disk to play or attach, not a wall
/// of base64 in their output. Shared by the CLI's `speak` and the MCP
/// `compute.speak` tool so both write a clip the same way.
pub fn save_speech_clip(
    dir: &Path,
    speech: &SpeechResult,
    job_id: Uuid,
) -> Result<(PathBuf, usize), String> {
    let audio = base64::engine::general_purpose::STANDARD
        .decode(speech.audio_base64.as_bytes())
        .map_err(|_| "the operator's audio did not base64-decode".to_string())?;
    let path = dir.join(format!("speech-{job_id}.{}", speech.format));
    std::fs::write(&path, &audio).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok((path, audio.len()))
}

/// The `compute.speak` tool spec, shared by the CLI help and the MCP
/// server's `tools/list`. The synthesis mirror of [`transcribe_tool_spec`].
pub fn speak_tool_spec(max_price_micro_usdc: u64) -> ToolSpec {
    ToolSpec {
        name: SPEAK_TOOL.into(),
        description: format!(
            "Synthesize speech from text on the Covenant compute network and return the saved \
             audio clip's path together with the operator's signed, hash-verified work receipt. \
             The clip is written to disk (base64 audio does not belong in an agent's context), so \
             the result names the file, its size and format. Pass the words to speak in `text` (up \
             to {MAX_SPEECH_TEXT_CHARS} characters). Optionally name a `voice` the operator's \
             backend knows (advisory — an unknown voice falls back to the operator's default), a \
             `format` (wav or aiff; wav is the default every client decodes), and a `speed` \
             multiple of the natural rate ({MIN_SPEECH_SPEED}..={MAX_SPEECH_SPEED}). Name a \
             `model` (e.g. say-1) to pin one, or omit it to accept any speech operator. Price per \
             call is capped at {max_price_micro_usdc} micro-USDC.",
            MAX_SPEECH_TEXT_CHARS = covenant_compute_protocol::MAX_SPEECH_TEXT_CHARS,
            MIN_SPEECH_SPEED = covenant_compute_protocol::MIN_SPEECH_SPEED,
            MAX_SPEECH_SPEED = covenant_compute_protocol::MAX_SPEECH_SPEED,
        ),
        input_schema: serde_json::json!({
            "type": "object",
            "required": ["text"],
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The words to speak, at most 4096 characters."
                },
                "voice": {
                    "type": "string",
                    "description": "A named voice for the operator's backend (e.g. Alex, alloy). \
                                    Advisory: an operator that does not know the voice uses its \
                                    default rather than failing. Omit to accept the default."
                },
                "format": {
                    "type": "string",
                    "description": "Audio container to return: wav (default, decodes everywhere) \
                                    or aiff. An operator that cannot produce it fails the job \
                                    rather than return a different container."
                },
                "speed": {
                    "type": "number",
                    "description": "Playback rate as a multiple of the voice's natural speed \
                                    (1.0 = normal), between 0.25 and 4.0. Omit for natural speed."
                },
                "model": {
                    "type": "string",
                    "description": "Speech model the job requires (e.g. say-1); omit to accept \
                                    any speech operator."
                },
                "gpu_class": {
                    "type": "string",
                    "description": "Require a specific GPU class (e.g. rtx-4090, h100), or cpu \
                                    for a CPU-only node; the requestable values are what \
                                    compute.capacity lists. Omit to accept any hardware."
                },
                "min_vram_gb": {
                    "type": "integer",
                    "description": "Require at least this much VRAM, in whole GB. Omit for no floor."
                },
                "min_reputation_bps": {
                    "type": "integer",
                    "description": "Require operators rated at least this many basis points \
                                    (8000 = 80%); the coordinator routes only to the proven pool \
                                    above it, and the job refunds rather than run on a lesser \
                                    operator. Omit for no floor. 1..=10000."
                },
                "price_micro_usdc": {
                    "type": "integer",
                    "description": "Offered price in micro-USDC; defaults to the cheapest matching operator's ask"
                },
                "deadline_ms": {
                    "type": "integer",
                    "description": "Job deadline in milliseconds; defaults to the configured deadline"
                },
                "idempotency_key": {
                    "type": "string",
                    "description": "Makes the purchase exactly-once: calls repeating this key \
                                    return the first call's result and never pay twice, even \
                                    across a daemon restart. A key names one purchase — an \
                                    explicit argument that contradicts it is refused; use a \
                                    fresh key for new work. 1..=128 bytes."
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "Preview only. Resolve the price, routing and deadline this \
                                    call would use and return them without dispatching a job or \
                                    spending anything. Refuses when no operator can serve the \
                                    job. Can't combine with idempotency_key — a preview reserves \
                                    no purchase."
                }
            }
        }),
    }
}

/// The `compute.receipts` MCP tool spec — the buyer's paid-for view
/// (A4): list this identity's jobs from the coordinator with every
/// receipt re-verified locally.
pub fn receipts_tool_spec() -> ToolSpec {
    ToolSpec {
        name: RECEIPTS_TOOL.into(),
        description: "List this buyer's compute jobs and their signed work receipts, each \
                      re-verified locally (signature, job binding, price ceiling) — what was \
                      bought, what it cost, and whether the operator's commitments check out. \
                      Unpaid rows carry the refund_reason (deadline_expired, buyer_cancelled, \
                      operator_rejected, execution_failed, admission_failed)."
            .into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "limit": {
                    "type": "integer",
                    "description": "Most recent jobs to return; defaults to 20"
                }
            }
        }),
    }
}

/// The `compute.deposit` MCP tool spec (A3): claim an on-chain payment
/// so it credits this buyer's pre-funded balance.
pub fn deposit_tool_spec() -> ToolSpec {
    ToolSpec {
        name: DEPOSIT_TOOL.into(),
        description: format!(
            "Claim a confirmed on-chain deposit so it credits this buyer's pre-funded \
             balance with the coordinator. Fund first: SPL-transfer USDC to the \
             coordinator's deposit account (compute.balance returns the account, mint and \
             exact instructions) with the transaction memo \
             `{DEPOSIT_MEMO_PREFIX}<this buyer's pubkey>`, then pass the transaction \
             signature here. Claiming is idempotent — re-claiming the same signature \
             never double-credits."
        ),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "deposit_id": {
                    "type": "string",
                    "description": "The confirmed deposit transaction's signature"
                }
            },
            "required": ["deposit_id"]
        }),
    }
}

/// The `compute.balance` MCP tool spec (A3): the buyer's funds — how
/// much is deposited, charged and available — plus the deployment's
/// deposit instructions for topping up.
pub fn balance_tool_spec() -> ToolSpec {
    ToolSpec {
        name: BALANCE_TOOL.into(),
        description: "This buyer's pre-funded balance with the coordinator (deposited, \
                      charged, available micro-USDC) and how to top it up on this \
                      deployment's payment rail."
            .into(),
        input_schema: serde_json::json!({ "type": "object", "properties": {} }),
    }
}

/// The `compute.capacity` MCP tool spec (Track A discovery): what the
/// network can serve right now, so an agent picks a real model id and
/// a workable price instead of guessing.
pub fn capacity_tool_spec() -> ToolSpec {
    ToolSpec {
        name: CAPACITY_TOOL.into(),
        description: "What compute is purchasable right now: live (kind, model) rows \
                      aggregated over matchable operators, each with its operator count, \
                      ask range in micro-USDC (min_ask is the price floor a job must offer \
                      to match), hardware summary, and the deployment's trust floors. \
                      Check here before compute.infer or compute.run to pick a served \
                      model and a price that will actually match."
            .into(),
        input_schema: serde_json::json!({ "type": "object", "properties": {} }),
    }
}

/// The `compute.withdraw` MCP tool spec (A3): move unspent balance
/// back out to a wallet the buyer names.
pub fn withdraw_tool_spec() -> ToolSpec {
    ToolSpec {
        name: WITHDRAW_TOOL.into(),
        description: "Withdraw unspent pre-funded balance to a wallet address of this \
                      buyer's choosing. The signed request is its own authorization; the \
                      withdrawal id makes retries idempotent, and the transfer's on-chain \
                      memo names it. Overdrawing the available balance is refused. A \
                      response with pushed=false means the debit is committed and the \
                      coordinator's retry sweep is still pushing the transfer — check \
                      compute.balance or retry with the same withdrawal_id to see it land."
            .into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "amount_micro_usdc": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "How much of the available balance to withdraw"
                },
                "recipient_address_b58": {
                    "type": "string",
                    "description": "The wallet (32-byte base58 owner address) the transfer pays"
                },
                "withdrawal_id": {
                    "type": "string",
                    "description": "Optional UUID idempotency key; omit to mint a fresh one"
                }
            },
            "required": ["amount_micro_usdc", "recipient_address_b58"]
        }),
    }
}

/// Arguments for [`withdraw_tool_spec`]'s tool.
#[derive(Debug, serde::Deserialize)]
pub struct WithdrawArgs {
    pub amount_micro_usdc: u64,
    pub recipient_address_b58: String,
    #[serde(default)]
    pub withdrawal_id: Option<Uuid>,
}

/// The `compute.withdrawals` MCP tool spec (A4): this buyer's
/// withdrawal history, so an agent that moved money out with
/// `compute.withdraw` can confirm the transfer actually landed.
pub fn withdrawals_tool_spec() -> ToolSpec {
    ToolSpec {
        name: WITHDRAWALS_TOOL.into(),
        description: "This buyer's withdrawal history, newest first: amount, recipient, and \
                      the on-chain memo, with pushed=true and a tx_signature once the transfer \
                      has landed, or pushed=false while the coordinator's retry sweep is still \
                      pushing it. The audit companion to compute.withdraw."
            .into(),
        input_schema: serde_json::json!({ "type": "object", "properties": {} }),
    }
}

/// The `compute.dispute` MCP tool spec (C4): the buyer's signed
/// counter-attestation that a completed, paid job's output was not the
/// work.
pub fn dispute_tool_spec() -> ToolSpec {
    ToolSpec {
        name: DISPUTE_TOOL.into(),
        description: format!(
            "Dispute a completed compute job whose output was not the work paid for. No \
             refund — the operator's receipt verified and the money moved — but the \
             dispute lands durably as a reputation fault against the operator, signed by \
             this buyer and kept as evidence. One dispute per job, only for jobs this \
             identity bought, only within the coordinator's dispute window. Reason is \
             capped at {MAX_DISPUTE_REASON_BYTES} bytes."
        ),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "The completed job to dispute (from compute.infer or compute.receipts)"
                },
                "reason": {
                    "type": "string",
                    "description": "What was wrong with the output — recorded verbatim on the books"
                }
            },
            "required": ["job_id", "reason"]
        }),
    }
}

/// The `compute.dispute` argument shape, shared by both consumers.
#[derive(Debug, Deserialize)]
pub struct DisputeArgs {
    pub job_id: Uuid,
    pub reason: String,
}

/// The `compute.cancel` argument shape, shared by both consumers.
#[derive(Debug, Deserialize)]
pub struct CancelArgs {
    pub job_id: Uuid,
}

/// The `compute.cancel` MCP tool spec: withdraw a job no operator has
/// accepted yet and take the refund now instead of waiting out the
/// deadline sweep.
pub fn cancel_tool_spec() -> ToolSpec {
    ToolSpec {
        name: CANCEL_TOOL.into(),
        description: "Cancel a job of this buyer's that no operator has accepted yet: the \
                      escrow hold comes back in full and the job stops counting against the \
                      per-buyer in-flight ceiling. Strictly pre-accept — once an operator \
                      has committed, the job settles by its result or its deadline \
                      (dispute a bad result instead of cancelling). Retrying answers the \
                      same refunded fact, and a purchase made under an idempotency_key is \
                      voided so the key may honestly buy again."
            .into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "The unaccepted job to withdraw (from compute.receipts or a dispatch that never concluded)"
                }
            },
            "required": ["job_id"]
        }),
    }
}

/// The `compute.verify` MCP tool spec — the zero-homework close of the
/// verifiable loop: one call answers "did the chain really pay for this
/// job?" against the chain's own record.
pub fn verify_tool_spec() -> ToolSpec {
    ToolSpec {
        name: VERIFY_TOOL.into(),
        description: "Verify one of this buyer's jobs end to end: re-verify the operator's \
                      signed receipt locally, then check the chain's own record of the payout \
                      transaction — it must carry exactly this receipt's memo and have moved \
                      exactly the amount the books claim. Read-only; a not-yet-paid job \
                      reports how far the money trail goes instead of failing. A verified \
                      payout includes a clickable explorer link to the transaction."
            .into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "The job to verify (from compute.infer, compute.run or compute.receipts)"
                }
            },
            "required": ["job_id"]
        }),
    }
}

/// The `compute.verify` argument shape, shared by both consumers.
#[derive(Debug, Deserialize)]
pub struct VerifyArgs {
    pub job_id: Uuid,
}

/// The `compute.output` MCP tool spec — re-read a past job's output and
/// its locally re-verified receipt, so an answer bought once is
/// recoverable later without paying again.
pub fn output_tool_spec() -> ToolSpec {
    ToolSpec {
        name: OUTPUT_TOOL.into(),
        description: "Re-read one of this buyer's past jobs: the output the operator produced, \
                      plus its receipt re-verified locally (signature, operator key, output \
                      hash). Read-only and idempotent — the coordinator returns a job's output \
                      only to the buyer who signed it. A job still in flight has no output yet; \
                      a refunded, rejected or failed one never will."
            .into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "The job to re-read (from compute.infer, compute.run or compute.receipts)"
                }
            },
            "required": ["job_id"]
        }),
    }
}

/// The `compute.output` argument shape.
#[derive(Debug, Deserialize)]
pub struct OutputArgs {
    pub job_id: Uuid,
}

/// The `compute.stream_start` MCP tool spec — `compute.infer` for
/// callers that want the output as it generates: same arguments, same
/// money path, but the call returns the job id immediately and the
/// output arrives through `compute.stream_poll`.
pub fn stream_start_tool_spec(max_price_micro_usdc: u64) -> ToolSpec {
    // Streaming returns a job_id before the background drain settles the
    // purchase, so it cannot back compute.infer's exactly-once idempotency
    // key; and a live feed is nothing to price ahead of time, so it has no
    // dry-run preview. Advertise infer's arguments minus both rather than
    // promise guarantees this surface can't keep; the handler rejects
    // either field if it is passed one anyway.
    let mut input_schema = infer_tool_spec(max_price_micro_usdc).input_schema;
    if let Some(properties) = input_schema
        .get_mut("properties")
        .and_then(|value| value.as_object_mut())
    {
        properties.remove("idempotency_key");
        properties.remove("dry_run");
    }
    ToolSpec {
        name: STREAM_START_TOOL.into(),
        description: format!(
            "Start a paid inference job on the Covenant compute network and return its \
             job_id immediately; read the output as it generates with compute.stream_poll. \
             The live feed is an unsigned preview: payment happens only after the \
             operator's signed receipt over the final output verifies locally, exactly as \
             compute.infer, and a failed or refunded job never charges. Not idempotent, \
             unlike compute.infer. Price per call is capped at {max_price_micro_usdc} \
             micro-USDC."
        ),
        input_schema,
    }
}

/// The `compute.stream_poll` MCP tool spec — the read half of the
/// pair. Poll while status is `streaming`; the concluding poll carries
/// the verified output and receipt exactly as `compute.infer` returns
/// them.
pub fn stream_poll_tool_spec() -> ToolSpec {
    ToolSpec {
        name: STREAM_POLL_TOOL.into(),
        description: "Read a streaming job's live output from the `since` cursor on. While \
                      the job runs, returns new text chunks, next_seq (pass it back as \
                      `since`), and status \"streaming\" — poll again after a short pause. \
                      The concluding poll returns the verified final output and signed \
                      receipt exactly as compute.infer would have, and stays readable for a \
                      few minutes. Only the caller that started the job may poll it."
            .into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "The job to read (from compute.stream_start)"
                },
                "since": {
                    "type": "integer",
                    "description": "Chunk cursor: the next_seq of the previous poll; 0 or \
                                    omitted reads from the start"
                }
            },
            "required": ["job_id"]
        }),
    }
}

/// The `compute.stream_poll` argument shape, shared by both consumers.
#[derive(Debug, Deserialize)]
pub struct StreamPollArgs {
    pub job_id: Uuid,
    #[serde(default)]
    pub since: u64,
}

/// What the coordinator recorded for a dispute.
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct DisputeOutcomeView {
    pub job_id: Uuid,
    pub operator_pubkey_b58: String,
    pub disputed: bool,
}

/// Signs and submits a dispute of one of this buyer's completed jobs.
/// The coordinator enforces everything contextual — buyer match, the
/// job actually concluded, the window is open, no prior dispute — and
/// records the signed request verbatim.
pub async fn dispute_job(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
    job_id: Uuid,
    reason: String,
) -> Result<DisputeOutcomeView, BuyerError> {
    let req = DisputeRequest::sign(
        buyer_identity.agent_id(),
        job_id,
        reason,
        epoch_ms(),
        buyer_identity,
    )
    .map_err(|e| BuyerError::Protocol(e.to_string()))?;
    let base = config.coordinator_url.trim_end_matches('/');
    let url = format!("{base}/federation/jobs/{job_id}/dispute");
    let resp =
        http.post(&url).json(&req).send().await.map_err(|e| {
            BuyerError::unreachable(&config.coordinator_url, "dispute this job", &e)
        })?;
    let view = json_or_error("dispute", resp).await?;
    serde_json::from_value(view)
        .map_err(|e| BuyerError::Coordinator(format!("dispute decode: {e}")))
}

/// Signs and submits a cancellation of one of this buyer's jobs. The
/// coordinator enforces everything contextual — buyer match, the job
/// still sitting unaccepted — refunds the hold in full, and answers an
/// honest retry with the same refunded fact. A job an operator already
/// committed to refuses: committed work settles by result or deadline.
pub async fn cancel_job(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
    job_id: Uuid,
) -> Result<CancelView, BuyerError> {
    let req = CancelRequest::sign(
        buyer_identity.agent_id(),
        job_id,
        epoch_ms(),
        buyer_identity,
    )
    .map_err(|e| BuyerError::Protocol(e.to_string()))?;
    let base = config.coordinator_url.trim_end_matches('/');
    let url = format!("{base}/federation/jobs/{job_id}/cancel");
    let resp = http
        .post(&url)
        .json(&req)
        .send()
        .await
        .map_err(|e| BuyerError::unreachable(&config.coordinator_url, "cancel this job", &e))?;
    let view = json_or_error("cancel", resp).await?;
    serde_json::from_value(view).map_err(|e| BuyerError::Coordinator(format!("cancel decode: {e}")))
}

/// Closes a running lease session on the buyer's signed instruction and
/// returns the settled view. The mirror of [`cancel_job`]: a cancel
/// refunds a job before it commits, a close stops a live session's meter
/// after it started. Recording is idempotent, so an honest retry is
/// safe — the coordinator answers the lease's real state either way.
pub async fn close_lease(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
    job_id: Uuid,
) -> Result<LeaseView, BuyerError> {
    let req = LeaseCloseRequest::sign(
        buyer_identity.agent_id(),
        job_id,
        epoch_ms(),
        buyer_identity,
    )
    .map_err(|e| BuyerError::Protocol(e.to_string()))?;
    let base = config.coordinator_url.trim_end_matches('/');
    let url = format!("{base}/federation/jobs/{job_id}/close");
    let resp =
        http.post(&url).json(&req).send().await.map_err(|e| {
            BuyerError::unreachable(&config.coordinator_url, "close this lease", &e)
        })?;
    let view = json_or_error("lease close", resp).await?;
    serde_json::from_value(view)
        .map_err(|e| BuyerError::Coordinator(format!("lease close decode: {e}")))
}

/// The buyer's live view of a lease: where the machine is, how long it
/// has run, and what it has cost so far. A signed read — the access grant
/// names the operator's running machine, reachable only with the buyer's
/// own key, so the endpoint returns only to the buyer who signed the
/// lease. The meter figures are the same ones settlement uses, derived
/// from the signed terms and the coordinator's own clock.
pub async fn lease_view(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
    job_id: Uuid,
) -> Result<LeaseView, BuyerError> {
    let base = config.coordinator_url.trim_end_matches('/');
    let path = format!("/federation/jobs/{job_id}/lease");
    let signed_at_ms = epoch_ms();
    let signature = covenant_compute_protocol::sign_read(buyer_identity, &path, signed_at_ms)
        .map_err(|e| BuyerError::Protocol(e.to_string()))?;
    let resp = http
        .get(format!("{base}{path}"))
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            signed_at_ms.to_string(),
        )
        .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, signature)
        .send()
        .await
        .map_err(|e| BuyerError::unreachable(&config.coordinator_url, "read this lease", &e))?;
    let view = json_or_error("lease view", resp).await?;
    serde_json::from_value(view)
        .map_err(|e| BuyerError::Coordinator(format!("lease view decode: {e}")))
}

/// What a deposit claim settled to: `credited: false` is an honest
/// retry of an already-applied deposit id, not an error. Deposit facts
/// only — the running balance lives behind the signed balance read,
/// since deposit signatures are public on-chain and the open claim
/// endpoint must not double as a balance oracle.
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct DepositOutcomeView {
    pub deposit_id: String,
    pub credited: bool,
    pub amount_micro_usdc: u64,
}

/// Claims a confirmed on-chain payment for this buyer's balance. The
/// rail, not this call, decides whose deposit it is and how much —
/// claiming a transaction that names someone else's buyer pubkey in
/// its memo is refused upstream.
pub async fn claim_deposit(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
    deposit_id: &str,
) -> Result<DepositOutcomeView, BuyerError> {
    let base = config.coordinator_url.trim_end_matches('/');
    let resp = http
        .post(format!("{base}/federation/buyers/deposit"))
        .json(&serde_json::json!({
            "buyer_pubkey_b58": buyer_identity.agent_id().pubkey_base58(),
            "deposit_id": deposit_id,
        }))
        .send()
        .await
        .map_err(|e| BuyerError::unreachable(&config.coordinator_url, "claim this deposit", &e))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(BuyerError::Coordinator(format!(
            "deposit claim returned {status}: {}",
            coordinator_reason(&body)
        )));
    }
    resp.json()
        .await
        .map_err(|e| BuyerError::Coordinator(format!("deposit claim decode: {e}")))
}

/// The coordinator's echo of one withdrawal: the debit facts plus the
/// transfer outcome so far. `pushed: false` means the debit is
/// committed but the backend transfer hasn't landed yet — the
/// coordinator's retry sweep keeps pushing; re-read
/// [`list_withdrawals`] for the outcome.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct WithdrawalOutcomeView {
    pub withdrawal_id: Uuid,
    pub buyer_pubkey_b58: String,
    pub recipient_address_b58: String,
    pub amount_micro_usdc: u64,
    pub requested_at_ms: u64,
    pub pushed: bool,
    pub tx_signature: Option<String>,
    /// The memo the transfer carries on-chain — re-derivable from
    /// (buyer pubkey, withdrawal id) by anyone holding this view.
    pub memo: String,
}

/// Withdraws `amount_micro_usdc` of this buyer's available balance to
/// a wallet of the buyer's choosing (A3's money-out verb). The signed
/// request is its own authorization; `withdrawal_id` is the
/// idempotency key end to end (request, books debit, on-chain memo),
/// so retrying with the same id never debits twice. Overdrawing the
/// available balance is refused by the coordinator's books.
pub async fn withdraw(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
    withdrawal_id: Uuid,
    amount_micro_usdc: u64,
    recipient_address_b58: &str,
) -> Result<WithdrawalOutcomeView, BuyerError> {
    let request = covenant_compute_protocol::WithdrawalRequest::sign(
        buyer_identity.agent_id(),
        withdrawal_id,
        amount_micro_usdc,
        recipient_address_b58.to_string(),
        epoch_ms(),
        buyer_identity,
    )
    .map_err(|e| BuyerError::Protocol(e.to_string()))?;

    let base = config.coordinator_url.trim_end_matches('/');
    let url = format!("{base}/federation/buyers/withdraw");
    let resp = http.post(&url).json(&request).send().await.map_err(|e| {
        BuyerError::unreachable(&config.coordinator_url, "submit this withdrawal", &e)
    })?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(BuyerError::Coordinator(format!(
            "withdraw returned {status}: {}",
            coordinator_reason(&body)
        )));
    }
    resp.json()
        .await
        .map_err(|e| BuyerError::Coordinator(format!("withdraw decode: {e}")))
}

/// This buyer's withdrawal history, newest first — a signed read, same
/// posture as the balance.
pub async fn list_withdrawals(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
) -> Result<Vec<WithdrawalOutcomeView>, BuyerError> {
    let base = config.coordinator_url.trim_end_matches('/');
    let buyer_key = buyer_identity.agent_id().pubkey_base58();
    let path = format!("/federation/buyers/{buyer_key}/withdrawals");
    let signed_at_ms = epoch_ms();
    let signature = covenant_compute_protocol::sign_read(buyer_identity, &path, signed_at_ms)
        .map_err(|e| BuyerError::Protocol(e.to_string()))?;
    let url = format!("{base}{path}");
    let resp = http
        .get(&url)
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            signed_at_ms.to_string(),
        )
        .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, signature)
        .send()
        .await
        .map_err(|e| {
            BuyerError::unreachable(&config.coordinator_url, "read this buyer's withdrawals", &e)
        })?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(BuyerError::Coordinator(format!(
            "withdrawals returned {status}: {}",
            coordinator_reason(&body)
        )));
    }
    resp.json()
        .await
        .map_err(|e| BuyerError::Coordinator(format!("withdrawals decode: {e}")))
}

async fn json_or_error(
    action: &str,
    resp: reqwest::Response,
) -> Result<serde_json::Value, BuyerError> {
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(BuyerError::Coordinator(format!(
            "{action} returned {status}: {}",
            coordinator_reason(&body)
        )));
    }
    resp.json()
        .await
        .map_err(|e| BuyerError::Coordinator(format!("{action} decode: {e}")))
}

/// The coordinator's live-capacity directory — what is purchasable
/// right now and at what ask. A public, unsigned read (the directory
/// is anonymous aggregates), parsed into the protocol's `CapacityView`
/// so a coordinator drifting from the shape fails loudly here instead
/// of feeding a half-parsed view to the tool surfaces.
pub async fn capacity(
    http: &reqwest::Client,
    config: &BuyerConfig,
) -> Result<CapacityView, BuyerError> {
    capacity_filtered(http, config, None).await
}

/// [`capacity`] narrowed to operators clearing `min_reputation_bps` — the
/// read a buyer pricing a reputation-floored buy makes, so the ask range
/// it sees is the one its own job would pay. `None` is the plain directory
/// read. The coordinator applies the higher of this and its own standing
/// floor, so a value below the coordinator's never widens the pool.
pub async fn capacity_filtered(
    http: &reqwest::Client,
    config: &BuyerConfig,
    min_reputation_bps: Option<u32>,
) -> Result<CapacityView, BuyerError> {
    let base = config.coordinator_url.trim_end_matches('/');
    let url = match min_reputation_bps {
        Some(bps) => format!("{base}/federation/capacity?min_reputation_bps={bps}"),
        None => format!("{base}/federation/capacity"),
    };
    let resp = http.get(&url).send().await.map_err(|e| {
        BuyerError::unreachable(&config.coordinator_url, "read the network's capacity", &e)
    })?;
    let value = json_or_error("capacity", resp).await?;
    serde_json::from_value(value)
        .map_err(|e| BuyerError::Coordinator(format!("capacity decode: {e}")))
}

/// The cheapest ask, in micro-USDC, among the operators the matcher
/// would currently consider for a job with these constraints — the price
/// at which the request first finds supply. `None` when nothing matchable
/// serves it, which a caller reads as "no operator to buy from right
/// now". A `None` model matches any operator of that kind; a named model
/// matches operators serving it or a generic `"any"` node, the same
/// widening the matcher applies. `gpu_class`/`min_vram_gb` narrow to rows
/// whose supply could satisfy the hardware ask, and `min_reputation_bps`
/// narrows to the pool a reputation-floored job would actually match, so
/// the ask it returns is one that job would truly pay. This is the right
/// default offer: paying the ceiling to an operator that asked far less is
/// money handed away, since settlement charges the envelope's price, not
/// the ask.
///
/// The capacity rows aggregate operators, so `gpu_classes`/`max_vram_gb`
/// are the row's union/max: a row kept for the ask can carry a cheaper
/// ask from a sibling operator that would not itself satisfy it, making
/// the returned figure a lower bound on the constrained ask, not a
/// guarantee. It never over-offers, and it turns an impossible ask into a
/// clean pre-dispatch `None` (no capable row) instead of a dispatch that
/// only refunds — a buyer wanting an exact figure pins `price`.
pub async fn cheapest_matching_ask(
    http: &reqwest::Client,
    config: &BuyerConfig,
    kind: JobKind,
    model: Option<&str>,
    gpu_class: Option<&str>,
    min_vram_gb: Option<u32>,
    min_reputation_bps: Option<u32>,
) -> Result<Option<u64>, BuyerError> {
    let view = capacity_filtered(http, config, min_reputation_bps).await?;
    let want = model.map(canonical_model);
    Ok(view
        .entries
        .iter()
        .filter(|e| e.kind == kind)
        .filter(|e| match want {
            None => true,
            Some(m) => e.model == "any" || canonical_model(&e.model) == m,
        })
        .filter(|e| gpu_class.is_none_or(|c| e.gpu_classes.iter().any(|g| g == c)))
        .filter(|e| min_vram_gb.is_none_or(|v| e.max_vram_gb >= v))
        .map(|e| e.min_ask_micro_usdc)
        .min())
}

/// The price a previewed buy would offer, and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceQuote {
    /// The buyer's explicit price, held under the per-call ceiling.
    Explicit(u64),
    /// The cheapest matching ask — the default when no price is named.
    CheapestAsk(u64),
}

impl PriceQuote {
    /// The offered price in micro-USDC.
    pub fn micro_usdc(self) -> u64 {
        match self {
            PriceQuote::Explicit(p) | PriceQuote::CheapestAsk(p) => p,
        }
    }

    /// A stable tag for the price's origin, shared by every preview surface.
    pub fn source(self) -> &'static str {
        match self {
            PriceQuote::Explicit(_) => "explicit",
            PriceQuote::CheapestAsk(_) => "cheapest_matching_ask",
        }
    }
}

/// Why a preview could not name a price a buy would truly pay.
#[derive(Debug, thiserror::Error)]
pub enum QuoteError {
    /// No operator currently serves the job's kind, model and hardware, so
    /// there is no ask to quote. A real agent-side dispatch would offer the
    /// ceiling and let the coordinator refund the no-match; a preview says
    /// so up front, having dispatched nothing.
    #[error("no operator is serving {0} right now; nothing was dispatched")]
    NoCapableOperator(String),
    /// An explicit price above the per-call ceiling — a buy would refuse it.
    #[error("offered price {price} micro-USDC exceeds the per-call ceiling {cap}")]
    PriceCeiling { price: u64, cap: u64 },
    /// The cheapest matching ask sits above the per-call ceiling. Naming the
    /// gap lets the caller raise the ceiling or pin a price it will authorize.
    #[error(
        "the cheapest matching ask is {ask} micro-USDC, above the per-call ceiling {cap}; \
         nothing was dispatched"
    )]
    AskAboveCeiling { ask: u64, cap: u64 },
    /// The market read itself failed; a preview can't quote a price it never
    /// learned.
    #[error(transparent)]
    Market(#[from] BuyerError),
}

/// The price a buy would offer for one job, resolved with no side effect
/// and — unlike a real agent-side dispatch — no silent fallback to the
/// per-call ceiling: an explicit `price` (ceiling-checked) or the cheapest
/// matching ask. When no operator can serve the job, or the cheapest ask is
/// over the ceiling, it refuses rather than quote a figure a buy would not
/// actually pay.
///
/// This is the honest resolver behind `dry_run` on the agent surfaces. The
/// buyer CLI's own `resolve_price_source` mirrors this logic for the
/// terminal preview; the two must move together.
#[allow(clippy::too_many_arguments)]
pub async fn quote_price(
    http: &reqwest::Client,
    config: &BuyerConfig,
    kind: JobKind,
    model: Option<&str>,
    gpu_class: Option<&str>,
    min_vram_gb: Option<u32>,
    min_reputation_bps: Option<u32>,
    price: Option<u64>,
    cap: u64,
) -> Result<PriceQuote, QuoteError> {
    match price {
        Some(p) if p > cap => Err(QuoteError::PriceCeiling { price: p, cap }),
        Some(p) => Ok(PriceQuote::Explicit(p)),
        None => {
            match cheapest_matching_ask(
                http,
                config,
                kind,
                model,
                gpu_class,
                min_vram_gb,
                min_reputation_bps,
            )
            .await?
            {
                None => Err(QuoteError::NoCapableOperator(describe_constraints(
                    kind,
                    model,
                    gpu_class,
                    min_vram_gb,
                ))),
                Some(ask) if ask > cap => Err(QuoteError::AskAboveCeiling { ask, cap }),
                Some(ask) => Ok(PriceQuote::CheapestAsk(ask)),
            }
        }
    }
}

/// The structured `dry_run` record every preview surface returns — the
/// price a buy would offer, where it came from, and the routing and
/// deadline it would carry — so the CLI, the MCP server and the native
/// covenantd capability describe a previewed buy identically.
#[allow(clippy::too_many_arguments)]
pub fn preview_value(
    kind: JobKind,
    model: Option<&str>,
    gpu_class: Option<&str>,
    min_vram_gb: Option<u32>,
    min_reputation_bps: Option<u32>,
    quote: PriceQuote,
    deadline_ms: u64,
    max_price_micro_usdc: u64,
    input_blocks: usize,
) -> serde_json::Value {
    serde_json::json!({
        "dry_run": true,
        "kind": kind_label(kind),
        "model": model,
        "gpu_class": gpu_class,
        "min_vram_gb": min_vram_gb,
        "min_reputation_bps": min_reputation_bps,
        "price_micro_usdc": quote.micro_usdc(),
        "price_source": quote.source(),
        "deadline_ms": deadline_ms,
        "max_price_micro_usdc": max_price_micro_usdc,
        "input_blocks": input_blocks,
    })
}

/// Names a job's routing constraints for a "nothing serves this" refusal,
/// so the caller sees exactly which of kind, model and hardware went unmet.
fn describe_constraints(
    kind: JobKind,
    model: Option<&str>,
    gpu_class: Option<&str>,
    min_vram_gb: Option<u32>,
) -> String {
    let mut desc = match model {
        Some(m) => format!("model {m:?} for {}", kind_label(kind)),
        None => format!("{} jobs", kind_label(kind)),
    };
    if let Some(class) = gpu_class {
        desc.push_str(&format!(" on gpu-class {class:?}"));
    }
    if let Some(vram) = min_vram_gb {
        desc.push_str(&format!(" with >={vram}GB VRAM"));
    }
    desc
}

/// The wire label for a job kind — the `snake_case` form the protocol
/// serializes, restated so a preview names the kind without a serde round
/// trip.
fn kind_label(kind: JobKind) -> &'static str {
    match kind {
        JobKind::InferenceCall => "inference_call",
        JobKind::BatchJob => "batch_job",
        JobKind::LeaseSession => "lease_session",
        JobKind::Embedding => "embedding",
        JobKind::Transcription => "transcription",
        JobKind::SpeechSynthesis => "speech_synthesis",
    }
}

/// This buyer's funds plus the deployment's deposit instructions —
/// everything an agent needs to decide "can I afford this job, and if
/// not, how do I top up". The balance is a signed read (it is this
/// buyer's spend pattern); the deposit instructions are public.
pub async fn funds_with_deposit_info(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
) -> Result<serde_json::Value, BuyerError> {
    let base = config.coordinator_url.trim_end_matches('/');
    let buyer_key = buyer_identity.agent_id().pubkey_base58();

    let balance_path = format!("/federation/buyers/{buyer_key}/balance");
    let signed_at_ms = epoch_ms();
    let signature =
        covenant_compute_protocol::sign_read(buyer_identity, &balance_path, signed_at_ms)
            .map_err(|e| BuyerError::Protocol(e.to_string()))?;
    let balance_url = format!("{base}{balance_path}");
    let resp = http
        .get(&balance_url)
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            signed_at_ms.to_string(),
        )
        .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, signature)
        .send()
        .await
        .map_err(|e| {
            BuyerError::unreachable(&config.coordinator_url, "read this buyer's balance", &e)
        })?;
    let balance = json_or_error("balance", resp).await?;

    let info_url = format!("{base}/federation/deposit-info");
    let resp = http.get(&info_url).send().await.map_err(|e| {
        BuyerError::unreachable(&config.coordinator_url, "read the deposit instructions", &e)
    })?;
    let deposit_info = json_or_error("deposit info", resp).await?;

    Ok(serde_json::json!({
        "balance": balance,
        "deposit_info": deposit_info,
    }))
}

/// One row of the buyer's job history after local re-verification.
#[derive(Debug, serde::Serialize)]
pub struct VerifiedJobRow {
    pub job_id: Uuid,
    pub status: String,
    /// Why an unpaid row's money came back, verbatim from the
    /// coordinator's books — `None` on live and served rows.
    pub refund_reason: Option<String>,
    /// The gross price the buyer committed and the escrow held — the whole
    /// price for a fixed job, a lease's window ceiling.
    pub price_micro_usdc: u64,
    /// What actually settled: the whole price for a fixed job, the metered
    /// draw for a lease, zero once refunded. `None` while the job is still in
    /// flight, and on a row from a coordinator too old to report it.
    pub charged_micro_usdc: Option<u64>,
    pub funding_source: String,
    pub issued_at_ms: u64,
    pub operator_pubkey_b58: Option<String>,
    pub result_hash_hex: Option<String>,
    /// `None` when no receipt exists yet (in-flight or refunded);
    /// `Some(Err)` is a receipt that FAILED local verification — worth
    /// alarming on, never silently dropped.
    pub receipt_verified: Option<bool>,
    pub verification_error: Option<String>,
    pub payout: Option<PayoutInfo>,
}

#[derive(Debug, Deserialize)]
struct BuyerJobRowWire {
    job_id: Uuid,
    status: String,
    #[serde(default)]
    refund_reason: Option<String>,
    price_micro_usdc: u64,
    #[serde(default)]
    charged_micro_usdc: Option<u64>,
    funding_source: serde_json::Value,
    issued_at_ms: u64,
    envelope: SignedJobEnvelope,
    receipt: Option<SignedWorkReceipt>,
    #[serde(default)]
    payout: Option<PayoutInfo>,
}

/// The signed history read both [`list_verified_jobs`] and
/// [`verify_payout`] start from. The listing returns this buyer's own
/// inputs, so the coordinator demands a signed read proving possession
/// of the key being asked about — sign the canonical path, not the
/// full URL.
async fn fetch_job_rows(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
) -> Result<Vec<BuyerJobRowWire>, BuyerError> {
    let base = config.coordinator_url.trim_end_matches('/');
    let buyer_key = buyer_identity.agent_id().pubkey_base58();
    let path = format!("/federation/buyers/{buyer_key}/jobs");
    let signed_at_ms = epoch_ms();
    let signature = covenant_compute_protocol::sign_read(buyer_identity, &path, signed_at_ms)
        .map_err(|e| BuyerError::Protocol(e.to_string()))?;
    let resp = http
        .get(format!("{base}{path}"))
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            signed_at_ms.to_string(),
        )
        .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, signature)
        .send()
        .await
        .map_err(|e| {
            BuyerError::unreachable(&config.coordinator_url, "read this buyer's jobs", &e)
        })?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(BuyerError::Coordinator(format!(
            "job list returned {status}: {}",
            coordinator_reason(&body)
        )));
    }
    resp.json()
        .await
        .map_err(|e| BuyerError::Coordinator(format!("job list decode: {e}")))
}

/// Fetches this buyer's job history and re-verifies every receipt
/// locally: the receipt's own signature, that it names the listed
/// envelope's job and binds to those exact envelope bytes, and that
/// its price never exceeds the offer. The relayed envelope is also
/// checked to be one this identity actually signed — a coordinator
/// cannot slip a foreign job into the history unnoticed.
pub async fn list_verified_jobs(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
    limit: usize,
) -> Result<Vec<VerifiedJobRow>, BuyerError> {
    let buyer_key = buyer_identity.agent_id().pubkey_base58();
    let rows = fetch_job_rows(http, config, buyer_identity).await?;

    Ok(rows
        .into_iter()
        .take(limit)
        .map(|row| {
            let verification = row
                .receipt
                .as_ref()
                .map(|receipt| verify_listed_receipt(&row.envelope, receipt, &buyer_key));
            let (receipt_verified, verification_error) = match verification {
                None => (None, None),
                Some(Ok(())) => (Some(true), None),
                Some(Err(e)) => (Some(false), Some(e.to_string())),
            };
            VerifiedJobRow {
                job_id: row.job_id,
                status: row.status,
                refund_reason: row.refund_reason,
                price_micro_usdc: row.price_micro_usdc,
                charged_micro_usdc: row.charged_micro_usdc,
                funding_source: row.funding_source.as_str().unwrap_or_default().to_string(),
                issued_at_ms: row.issued_at_ms,
                operator_pubkey_b58: row
                    .receipt
                    .as_ref()
                    .map(|r| r.receipt.operator.pubkey_base58()),
                result_hash_hex: row
                    .receipt
                    .as_ref()
                    .map(|r| r.receipt.result_hash_hex.clone()),
                receipt_verified,
                verification_error,
                payout: row.payout,
            }
        })
        .collect())
}

/// [`verify_receipt`] minus the output check (the list endpoint does
/// not carry outputs), plus the buyer-attribution check that only
/// matters when the envelope arrives over the relay instead of from
/// this process's own memory.
fn verify_listed_receipt(
    envelope: &SignedJobEnvelope,
    receipt: &SignedWorkReceipt,
    expected_buyer_pubkey_b58: &str,
) -> Result<(), BuyerError> {
    envelope
        .verify()
        .map_err(|e| BuyerError::Verification(format!("envelope signature: {e}")))?;
    if envelope.payload.buyer.pubkey_base58() != expected_buyer_pubkey_b58 {
        return Err(BuyerError::Verification(
            "listed envelope was not signed by this buyer".into(),
        ));
    }
    receipt
        .verify()
        .map_err(|e| BuyerError::Verification(format!("receipt signature: {e}")))?;
    if receipt.receipt.job_id != envelope.payload.job_id {
        return Err(BuyerError::Verification(format!(
            "receipt is for job {}, not {}",
            receipt.receipt.job_id, envelope.payload.job_id
        )));
    }
    if receipt.receipt.job_hash_hex != sha256_hex(envelope.payload_json.as_bytes()) {
        return Err(BuyerError::Verification(
            "receipt job_hash_hex does not match the listed envelope".into(),
        ));
    }
    if receipt.receipt.price_micro_usdc > envelope.payload.price_micro_usdc {
        return Err(BuyerError::Verification(format!(
            "receipt claims {} micro-USDC, more than the {} offered",
            receipt.receipt.price_micro_usdc, envelope.payload.price_micro_usdc
        )));
    }
    Ok(())
}

/// A past job's output, re-read from the coordinator and re-verified
/// locally. `output` is empty until the job completes; `receipt_verified`
/// is `Some(false)` on a receipt that failed the check — surfaced, never
/// silently dropped.
#[derive(Debug, serde::Serialize)]
pub struct JobOutputView {
    pub job_id: Uuid,
    pub status: String,
    pub refund_reason: Option<String>,
    pub output: Vec<Content>,
    pub receipt: Option<SignedWorkReceipt>,
    pub receipt_verified: Option<bool>,
    pub verification_error: Option<String>,
    pub payout: Option<PayoutInfo>,
    /// What the settled hold actually charged: the whole price for a fixed
    /// job, the metered draw for a lease, zero once refunded. `None` while the
    /// job is still in flight, and on a receipt from a coordinator too old to
    /// report it — the receipt's own `price_micro_usdc` is the window ceiling,
    /// which over-reports a lease closed before its window ran out.
    pub charged_micro_usdc: Option<u64>,
}

impl JobOutputView {
    /// The operator's own signed verdict says this job failed, whatever the
    /// coordinator labelled it. A failure receipt hashes to its own failure
    /// text, so `receipt_verified` alone calls it clean and a reader takes the
    /// cause for the answer; the buy path guards this by refusing a non-`Ok`
    /// receipt as `NotServed`, and a re-read must judge success the same way
    /// rather than trusting the relay's `status`/`refund_reason`. `false` when
    /// no receipt was returned yet — there is no verdict to disbelieve.
    pub fn receipt_reports_failure(&self) -> bool {
        self.receipt
            .as_ref()
            .is_some_and(|r| r.receipt.status != A2ATaskStatus::Ok)
    }
}

/// Re-reads a past job's output from the coordinator and proves it
/// locally, so a buyer who lost the answer at buy time — a closed
/// terminal, an un-redirected run — can recover the work it paid for.
/// The read is signed: the coordinator returns a job's output only to
/// the buyer who signed its envelope. What is provable without holding
/// that envelope is proven — the operator's own signature, that the
/// receipt names this job, and that the delivered output hashes to what
/// the operator signed. The envelope-binding and price checks
/// [`verify_receipt`] adds need the signed envelope a re-read by id
/// alone does not carry. A job still in flight has no output yet; a
/// refunded, rejected or failed one never will.
pub async fn fetch_job_output(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
    job_id: Uuid,
) -> Result<JobOutputView, BuyerError> {
    let base = config.coordinator_url.trim_end_matches('/');
    let path = format!("/federation/jobs/{job_id}/receipt");
    let url = format!("{base}{path}");
    let signed_at_ms = epoch_ms();
    let signature = covenant_compute_protocol::sign_read(buyer_identity, &path, signed_at_ms)
        .map_err(|e| BuyerError::Protocol(e.to_string()))?;
    let resp = http
        .get(&url)
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            signed_at_ms.to_string(),
        )
        .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, signature)
        .send()
        .await
        .map_err(|e| {
            BuyerError::unreachable(&config.coordinator_url, "read this job's output", &e)
        })?;
    let value = json_or_error("job output", resp).await?;
    let job: JobStatusResponse = serde_json::from_value(value)
        .map_err(|e| BuyerError::Coordinator(format!("job output decode: {e}")))?;

    let output = job.output.unwrap_or_default();
    let (receipt_verified, verification_error) = match &job.receipt {
        Some(receipt) => match verify_fetched_receipt(receipt, job_id, &output) {
            Ok(()) => (Some(true), None),
            Err(e) => (Some(false), Some(e.to_string())),
        },
        None => (None, None),
    };
    Ok(JobOutputView {
        job_id,
        status: job.status,
        refund_reason: job.refund_reason,
        output,
        receipt: job.receipt,
        receipt_verified,
        verification_error,
        payout: job.payout,
        charged_micro_usdc: job.charged_micro_usdc,
    })
}

/// The subset of [`verify_receipt`]'s checks provable from the receipt
/// endpoint alone: the operator's signature, that the receipt is for
/// this job, and that the output hashes to what was signed. The two
/// envelope-bound checks (exact-bytes binding, price ceiling) need the
/// signed envelope, absent on a re-read by job id.
fn verify_fetched_receipt(
    receipt: &SignedWorkReceipt,
    job_id: Uuid,
    output: &[Content],
) -> Result<(), BuyerError> {
    receipt
        .verify()
        .map_err(|e| BuyerError::Verification(format!("receipt signature: {e}")))?;
    if receipt.receipt.job_id != job_id {
        return Err(BuyerError::Verification(format!(
            "receipt is for job {}, not {job_id}",
            receipt.receipt.job_id
        )));
    }
    if output_hash_hex(output) != receipt.receipt.result_hash_hex {
        return Err(BuyerError::Verification(
            "output does not hash to the receipt's result_hash_hex".into(),
        ));
    }
    Ok(())
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

/// What the coordinator's receipt poll returns; mirrors the
/// coordinator's `JobStatusView`.
#[derive(Debug, Deserialize)]
struct JobStatusResponse {
    status: String,
    #[serde(default)]
    refund_reason: Option<String>,
    receipt: Option<SignedWorkReceipt>,
    #[serde(default)]
    output: Option<Vec<Content>>,
    #[serde(default)]
    payout: Option<PayoutInfo>,
    #[serde(default)]
    charged_micro_usdc: Option<u64>,
}

/// Buyer-side re-verification of a completed job. Fail-closed on every
/// commitment the buyer can check without trusting the coordinator's
/// relay: the receipt's own signature, that it names this exact job,
/// that it binds to the exact envelope bytes this buyer signed, that
/// the delivered output hashes to what the operator signed, and that
/// the metered price never exceeds what was offered.
pub fn verify_receipt(
    envelope: &SignedJobEnvelope,
    receipt: &SignedWorkReceipt,
    output: &[Content],
) -> Result<(), BuyerError> {
    receipt
        .verify()
        .map_err(|e| BuyerError::Verification(format!("receipt signature: {e}")))?;
    if receipt.receipt.job_id != envelope.payload.job_id {
        return Err(BuyerError::Verification(format!(
            "receipt is for job {}, not {}",
            receipt.receipt.job_id, envelope.payload.job_id
        )));
    }
    let expected_job_hash = sha256_hex(envelope.payload_json.as_bytes());
    if receipt.receipt.job_hash_hex != expected_job_hash {
        return Err(BuyerError::Verification(
            "receipt job_hash_hex does not match the submitted envelope".into(),
        ));
    }
    if output_hash_hex(output) != receipt.receipt.result_hash_hex {
        return Err(BuyerError::Verification(
            "output does not hash to the receipt's result_hash_hex".into(),
        ));
    }
    if receipt.receipt.price_micro_usdc > envelope.payload.price_micro_usdc {
        return Err(BuyerError::Verification(format!(
            "receipt claims {} micro-USDC, more than the {} offered",
            receipt.receipt.price_micro_usdc, envelope.payload.price_micro_usdc
        )));
    }
    Ok(())
}

/// The operator's own signed cause for a failed job, or `None` when the
/// coordinator relayed no receipt+output that verifies. The failure
/// receipt hashes its output, so re-verifying it here is what lets a
/// buyer trust operator-authored text the coordinator merely passed
/// along. Control characters are stripped and the text bounded because
/// the buyer renders it and it is not the buyer's own.
fn verified_failure_detail(
    envelope: &SignedJobEnvelope,
    receipt: Option<&SignedWorkReceipt>,
    output: Option<&Vec<Content>>,
) -> Option<String> {
    let (receipt, output) = (receipt?, output?);
    verify_receipt(envelope, receipt, output).ok()?;
    let text: String = output
        .iter()
        .filter_map(|c| match c {
            Content::Text { text } => Some(text.as_str()),
            Content::Json { .. } => None,
        })
        .collect();
    let cleaned: String = text
        .chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim();
    (!cleaned.is_empty()).then(|| cleaned.chars().take(1024).collect())
}

/// Checks that a `getTransaction` result (`jsonParsed` encoding, the
/// shape [`fetch_payout_transaction`] returns and any Solana RPC
/// serves) is the payout for exactly `receipt` — the buyer's face of
/// [`verify_payout_transaction`], which owns the checks: transaction
/// succeeded, exactly one compute payout memo, that memo is the one
/// this receipt derives, and exactly one wallet's balance actually
/// grew.
///
/// Pure — no RPC, no solana dependency, no trust in the coordinator.
/// The memo can't be forged for different work (it embeds the
/// operator's signature over the whole receipt), so a match proves the
/// chain paid for this job; the returned [`PayoutProof`] says how
/// much, in what mint, to whom, all read from the transaction itself.
pub fn verify_payout_onchain(
    receipt: &SignedWorkReceipt,
    tx: &serde_json::Value,
) -> Result<PayoutProof, BuyerError> {
    verify_payout_transaction(&receipt.payout_memo(), tx)
        .map_err(|e| BuyerError::Verification(e.to_string()))
}

/// Fetches `signature` from a Solana RPC in the `jsonParsed` shape
/// [`verify_payout_onchain`] reads. Split from verification so callers
/// without RPC access (or who fetched the transaction any other way —
/// CLI, explorer API) can still verify.
pub async fn fetch_payout_transaction(
    http: &reqwest::Client,
    rpc_url: &str,
    signature: &str,
) -> Result<serde_json::Value, BuyerError> {
    let resp = http
        .post(rpc_url)
        .json(&payout_transaction_rpc_request(signature))
        .send()
        .await
        .map_err(|e| BuyerError::Rpc(format!("could not reach {rpc_url}: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(BuyerError::Rpc(format!("{rpc_url} returned {status}")));
    }
    let envelope: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| BuyerError::Rpc(format!("response from {rpc_url} did not decode: {e}")))?;
    if let Some(err) = envelope.get("error") {
        return Err(BuyerError::Rpc(format!(
            "{rpc_url} returned an error: {err}"
        )));
    }
    Ok(envelope
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

/// The confirmation level `signature` has reached on `rpc_url`'s
/// recent-status cache (`getSignatureStatuses`), or `None` when that RPC
/// has never seen it. Lets [`verify_payout`] tell a payout still
/// propagating to the buyer's own RPC apart from a signature the chain
/// has no record of at all.
async fn fetch_signature_status(
    http: &reqwest::Client,
    rpc_url: &str,
    signature: &str,
) -> Result<Option<String>, BuyerError> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getSignatureStatuses",
        "params": [[signature], { "searchTransactionHistory": true }],
    });
    let resp = http
        .post(rpc_url)
        .json(&body)
        .send()
        .await
        .map_err(|e| BuyerError::Rpc(format!("could not reach {rpc_url}: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(BuyerError::Rpc(format!("{rpc_url} returned {status}")));
    }
    let envelope: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| BuyerError::Rpc(format!("response from {rpc_url} did not decode: {e}")))?;
    if let Some(err) = envelope.get("error") {
        return Err(BuyerError::Rpc(format!(
            "{rpc_url} returned an error: {err}"
        )));
    }
    Ok(envelope
        .pointer("/result/value/0/confirmationStatus")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string))
}

/// Where [`verify_payout`]'s one call landed. `verified_onchain` is the
/// only verdict backed by the chain itself; the not-yet states report
/// exactly how far the money trail goes today so an agent re-checks
/// later instead of mistaking "not yet" for "proven". Anything that
/// contradicts a commitment — a receipt that fails re-verification, a
/// transaction that doesn't carry this receipt's memo, books that claim
/// a different amount than the chain moved — is an error, never a soft
/// verdict.
#[derive(Debug, serde::Serialize)]
pub struct PayoutVerification {
    pub job_id: Uuid,
    /// `verified_onchain` | `payout_unconfirmed` | `offchain_record_only`
    /// | `payout_pending` | `no_payout_due` | `no_receipt`.
    pub verdict: &'static str,
    /// The memo this job's payout carries (or will carry), derived
    /// locally from the operator's signed receipt — never taken from
    /// the coordinator's echo.
    pub memo: Option<String>,
    pub tx_signature: Option<String>,
    /// What the chain's own record says moved; only on
    /// `verified_onchain`.
    pub proof: Option<PayoutProof>,
    /// Clickable Solana Explorer link for `tx_signature`, only on
    /// `verified_onchain` — the human-checkable half of the proof. The
    /// verification itself never reads the explorer; it reads the
    /// buyer's own RPC.
    pub explorer_url: Option<String>,
    pub detail: String,
}

/// Explorer link for a payout transaction, cluster-tagged from the same
/// RPC endpoint the verification read so the link shows the transaction
/// the proof was checked against. Solana Explorer defaults to mainnet
/// and needs an explicit `?cluster=` for devnet/testnet.
fn explorer_tx_url(rpc_url: &str, tx_signature: &str) -> String {
    let cluster = if rpc_url.contains("devnet") {
        "?cluster=devnet"
    } else if rpc_url.contains("testnet") {
        "?cluster=testnet"
    } else {
        ""
    };
    format!("https://explorer.solana.com/tx/{tx_signature}{cluster}")
}

/// One call, the whole money trail: fetch this buyer's own record of
/// `job_id` from the coordinator, re-verify the operator's receipt
/// locally, then hold the chain to it — [`fetch_payout_transaction`]
/// against `config.rpc_url` plus [`verify_payout_onchain`], with the
/// on-chain amount cross-checked against the coordinator's books.
/// Every link before the chain fetch verifies without any RPC
/// configured.
pub async fn verify_payout(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
    job_id: Uuid,
) -> Result<PayoutVerification, BuyerError> {
    let buyer_key = buyer_identity.agent_id().pubkey_base58();
    let row = fetch_job_rows(http, config, buyer_identity)
        .await?
        .into_iter()
        .find(|row| row.job_id == job_id)
        .ok_or_else(|| {
            BuyerError::Coordinator(format!("job {job_id} is not in this buyer's history"))
        })?;

    let Some(receipt) = row.receipt else {
        return Ok(PayoutVerification {
            job_id,
            verdict: "no_receipt",
            memo: None,
            tx_signature: None,
            proof: None,
            explorer_url: None,
            detail: format!(
                "job status is {}: no operator receipt exists, so no payout can be verified",
                row.status
            ),
        });
    };
    verify_listed_receipt(&row.envelope, &receipt, &buyer_key)?;
    let memo = receipt.payout_memo();

    let Some(payout) = row.payout else {
        if row.status != "completed" {
            return Ok(PayoutVerification {
                job_id,
                verdict: "no_payout_due",
                memo: Some(memo),
                tx_signature: None,
                proof: None,
                explorer_url: None,
                detail: format!(
                    "job status is {}: the receipt is the operator's own signed evidence, \
                     the escrow hold was refunded, and no payout is due",
                    row.status
                ),
            });
        }
        return Ok(PayoutVerification {
            job_id,
            verdict: "payout_pending",
            memo: Some(memo),
            tx_signature: None,
            proof: None,
            explorer_url: None,
            detail: "the receipt verified but no payout is recorded yet; the coordinator's \
                     retry sweep keeps pushing — re-check later"
                .into(),
        });
    };
    let Some(tx_signature) = payout.tx_signature else {
        return Ok(PayoutVerification {
            job_id,
            verdict: "offchain_record_only",
            memo: Some(memo),
            tx_signature: None,
            proof: None,
            explorer_url: None,
            detail: format!(
                "the {} micro-USDC payout is recorded off-chain only (the coordinator's \
                 payout backend submitted no transaction); there is nothing on chain to verify",
                payout.amount_micro_usdc
            ),
        });
    };

    let Some(rpc_url) = config.rpc_url.as_deref() else {
        return Err(BuyerError::Verification(format!(
            "payout transaction {tx_signature} exists but no RPC endpoint is configured to \
             read it back; set the buyer's own rpc_url to verify on-chain"
        )));
    };
    // A schemeless rpc_url fails deep in reqwest with an opaque error;
    // name the fix here instead, the same posture the coordinator URL
    // takes at the CLI's front door.
    if !(rpc_url.starts_with("http://") || rpc_url.starts_with("https://")) {
        return Err(BuyerError::Verification(format!(
            "rpc_url must start with http:// or https:// to read the payout back, got {rpc_url:?}"
        )));
    }
    let tx = fetch_payout_transaction(http, rpc_url, &tx_signature).await?;
    if tx.is_null() {
        // The coordinator records a payout signature only after its own
        // RPC confirms the transfer, so a null read here is the buyer's
        // RPC not having served it back yet, not a failed payout. Tell
        // that apart from a signature the chain has no record of: the
        // first is "re-check shortly", the second is a real contradiction
        // worth failing loudly on.
        return match fetch_signature_status(http, rpc_url, &tx_signature).await? {
            Some(level) => Ok(PayoutVerification {
                job_id,
                verdict: "payout_unconfirmed",
                memo: Some(memo),
                tx_signature: Some(tx_signature),
                proof: None,
                explorer_url: None,
                detail: format!(
                    "the payout transaction is on chain ({level}) but your RPC hasn't served \
                     it back yet; re-check shortly to confirm the amount and recipient"
                ),
            }),
            None => Err(BuyerError::Verification(format!(
                "the coordinator recorded payout transaction {tx_signature}, but your RPC has \
                 no record of it; re-check against another RPC before trusting the payout"
            ))),
        };
    }
    let proof = verify_payout_onchain(&receipt, &tx)?;
    if proof.amount_micro_usdc != payout.amount_micro_usdc {
        return Err(BuyerError::Verification(format!(
            "the chain moved {} base units but the coordinator's books claim {}",
            proof.amount_micro_usdc, payout.amount_micro_usdc
        )));
    }
    Ok(PayoutVerification {
        job_id,
        verdict: "verified_onchain",
        memo: Some(memo),
        explorer_url: Some(explorer_tx_url(rpc_url, &tx_signature)),
        tx_signature: Some(tx_signature),
        detail: format!(
            "the chain's own record carries this receipt's memo and paid {} base units of \
             mint {} to {}",
            proof.amount_micro_usdc, proof.mint_b58, proof.recipient_owner_b58
        ),
        proof: Some(proof),
    })
}

/// Signs and submits one job, polls until the coordinator serves a
/// receipt or the job's own deadline (plus one grace poll) passes, and
/// re-verifies everything locally. Network-only — spend accounting is
/// the caller's concern.
pub async fn dispatch_and_verify(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
    request: JobRequest,
) -> Result<DispatchOutcome, BuyerError> {
    let envelope =
        sign_and_submit(http, config, buyer_identity, request, false, Uuid::new_v4()).await?;
    poll_receipt_and_verify(http, config, buyer_identity, envelope).await
}

/// Signs and submits one streaming job under a caller-chosen id and
/// returns the accepted envelope without waiting for chunks or the
/// receipt — the front half of [`dispatch_streaming`], for a surface
/// that must hand the job id back immediately (a start/poll tool pair)
/// and drain the feed from a background task. Choosing the id before
/// submission lets such a surface register the job under its own
/// bookkeeping first and roll back if this call fails.
pub async fn submit_streaming(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
    job_id: Uuid,
    request: JobRequest,
) -> Result<SignedJobEnvelope, BuyerError> {
    sign_and_submit(http, config, buyer_identity, request, true, job_id).await
}

/// Like [`dispatch_and_verify`], additionally draining the job's live
/// output feed while it runs: every relayed text delta reaches
/// `on_chunk` in order, and the final receipt is then verified exactly
/// as the non-streaming path does — the feed is a preview, the signed
/// receipt over the final output is the artifact. Degrades cleanly: a
/// node that cannot stream (or a relay that failed mid-job) just means
/// fewer or no chunks before the same verified outcome.
pub async fn dispatch_streaming(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
    request: JobRequest,
    on_chunk: impl FnMut(&str) + Send,
) -> Result<StreamingDispatchOutcome, BuyerError> {
    let envelope = submit_streaming(http, config, buyer_identity, Uuid::new_v4(), request).await?;
    stream_and_verify(http, config, buyer_identity, envelope, on_chunk).await
}

/// The back half of [`dispatch_streaming`]: drain a submitted job's
/// live feed into `on_chunk`, then poll and re-verify the receipt
/// exactly as the non-streaming path does.
pub async fn stream_and_verify(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
    envelope: SignedJobEnvelope,
    mut on_chunk: impl FnMut(&str) + Send,
) -> Result<StreamingDispatchOutcome, BuyerError> {
    let job_id = envelope.payload.job_id;
    let deadline_at = epoch_ms()
        .saturating_add(envelope.payload.deadline_ms)
        .saturating_add(5_000);
    // Tokens land at conversational cadence, so poll faster than the
    // receipt loop's interval when that one is long.
    let stream_interval = config.poll_interval.min(Duration::from_millis(250));

    let mut assembled = String::new();
    let mut since = 0u64;
    loop {
        // A failed poll must not fail the dispatch — the feed is a
        // preview and the receipt loop below is the authority on the
        // outcome. An unreachable coordinator is the deploy window (its
        // job book is journal-durable even though its stream buffer is
        // not), so keep ticking to the same deadline; an answered
        // refusal is final for this reader, so hand straight over to
        // the receipt poll, which turns the same condition into the
        // real verdict.
        match poll_stream_once(http, config, buyer_identity, job_id, since).await {
            StreamPoll::View(view) => {
                for chunk in &view.chunks {
                    assembled.push_str(&chunk.text);
                    on_chunk(&chunk.text);
                }
                since = view.next_seq;
                // Terminal phase or a closed feed: the receipt is next,
                // and the receipt loop below owns every terminal outcome
                // (error wording included) so the two paths cannot drift.
                match view.status.as_str() {
                    "completed" | "failed" | "refunded" | "rejected" => break,
                    _ => {}
                }
                if view.done {
                    break;
                }
            }
            StreamPoll::Unreachable(e) => {
                tracing::debug!(%job_id, error = %e, "stream poll failed; retrying until the deadline")
            }
            StreamPoll::Refused(e) => {
                tracing::debug!(%job_id, error = %e, "stream read refused; the receipt poll owns the outcome");
                break;
            }
        }
        if epoch_ms() > deadline_at {
            break;
        }
        tokio::time::sleep(stream_interval).await;
    }

    let outcome = poll_receipt_and_verify(http, config, buyer_identity, envelope).await?;
    let final_text: String = outcome
        .output
        .iter()
        .filter_map(|c| match c {
            Content::Text { text } => Some(text.as_str()),
            Content::Json { .. } => None,
        })
        .collect();
    let stream_matched_output = !assembled.is_empty() && assembled == final_text;
    Ok(StreamingDispatchOutcome {
        outcome,
        stream_matched_output,
    })
}

/// A streamed dispatch's result: the usual verified outcome plus
/// whether the live feed, assembled, equals the receipt-verified final
/// output — `false` both when the relay clipped or lost chunks and
/// when no chunk ever arrived (a node that couldn't stream). Either
/// way the verified output stands; this flag only grades the preview.
#[derive(Debug)]
pub struct StreamingDispatchOutcome {
    pub outcome: DispatchOutcome,
    pub stream_matched_output: bool,
}

/// One page of a streaming job's live feed, mirror of the
/// coordinator's stream view. `status` is the job phase, so a poller
/// knows chunks stopped because settlement is next.
#[derive(Debug, Deserialize)]
pub struct StreamPollView {
    pub job_id: Uuid,
    pub status: String,
    pub chunks: Vec<covenant_compute_protocol::StreamChunk>,
    pub next_seq: u64,
    pub done: bool,
    pub truncated: bool,
}

/// One stream poll's fate, split by what a caller mid-dispatch should
/// do about it: an unreachable coordinator is worth retrying (a deploy
/// window ends), an answered refusal is authoritative and means the
/// feed is over for this reader.
enum StreamPoll {
    View(StreamPollView),
    Unreachable(String),
    Refused(BuyerError),
}

async fn poll_stream_once(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
    job_id: Uuid,
    since: u64,
) -> StreamPoll {
    let base = config.coordinator_url.trim_end_matches('/');
    let path = format!("/federation/jobs/{job_id}/stream");
    let signed_at_ms = epoch_ms();
    let signature = match covenant_compute_protocol::sign_read(buyer_identity, &path, signed_at_ms)
    {
        Ok(s) => s,
        Err(e) => return StreamPoll::Refused(BuyerError::Protocol(e.to_string())),
    };
    let url = format!("{base}{path}?since={since}");
    let resp = match http
        .get(&url)
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            signed_at_ms.to_string(),
        )
        .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, signature)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return StreamPoll::Unreachable(format!("stream poll: {e}")),
    };
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let msg = format!(
            "stream poll returned {status}: {}",
            coordinator_reason(&body)
        );
        if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return StreamPoll::Unreachable(msg);
        }
        return StreamPoll::Refused(BuyerError::Coordinator(msg));
    }
    match resp.json().await {
        Ok(view) => StreamPoll::View(view),
        Err(e) => StreamPoll::Refused(BuyerError::Coordinator(format!("stream poll decode: {e}"))),
    }
}

/// Reads a streaming job's chunks from `since` on. A signed read like
/// the receipt poll — chunk text is job output; holding the job id
/// must not be enough.
pub async fn poll_stream(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
    job_id: Uuid,
    since: u64,
) -> Result<StreamPollView, BuyerError> {
    match poll_stream_once(http, config, buyer_identity, job_id, since).await {
        StreamPoll::View(view) => Ok(view),
        StreamPoll::Unreachable(msg) => Err(BuyerError::Coordinator(msg)),
        StreamPoll::Refused(e) => Err(e),
    }
}

/// Signs one non-streaming job envelope under a fresh id without
/// submitting it — for a caller that must durably remember the
/// purchase before any money can move (see [`PurchaseBook`]). Drive it
/// with [`dispatch_signed`], today or after any number of restarts.
pub fn sign_envelope(
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
    request: JobRequest,
) -> Result<SignedJobEnvelope, BuyerError> {
    build_envelope(config, buyer_identity, request, false, Uuid::new_v4())
}

/// The smallest deadline worth signing. A remote job has to be matched,
/// delivered over the operator's long-poll, admitted, executed, and
/// reported; nothing legitimate happens in under a second. Below this a
/// job is signed only to be refused ("already past its deadline") or
/// deadline-swept the instant it lands, so the buyer paid the round trip
/// for nothing. The floor also catches the common unit slip — a deadline
/// typed in seconds (`60`) instead of milliseconds.
pub const MIN_DEADLINE_MS: u64 = 1_000;

/// The metered window a lease is floored and settled against, read back
/// from its own signed terms. `None` for any other kind, or a lease whose
/// input carries no readable terms, both of which fall back to the deadline
/// window. Clamped into `u32` for the requirement field; a lease window is
/// capped well below that by `MAX_LEASE_DURATION_SECS`.
fn lease_window_secs(kind: JobKind, input: &[Content]) -> Option<u32> {
    if kind != JobKind::LeaseSession {
        return None;
    }
    let terms = covenant_compute_protocol::parse_lease_terms(input).ok()??;
    Some(terms.max_duration_secs.clamp(1, u64::from(u32::MAX)) as u32)
}

fn build_envelope(
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
    request: JobRequest,
    stream: bool,
    job_id: Uuid,
) -> Result<SignedJobEnvelope, BuyerError> {
    if request.deadline_ms < MIN_DEADLINE_MS {
        return Err(BuyerError::Protocol(format!(
            "deadline_ms {} is below the {MIN_DEADLINE_MS}ms floor — deadlines are in \
             milliseconds, and a remote job needs at least a second to be matched and served",
            request.deadline_ms
        )));
    }
    // A hardware ask that can never be satisfied is refused here, before
    // a hold: an empty `gpu_class` matches no declared hardware, and a
    // `Some(0)` VRAM floor asks for nothing yet reads as a real
    // constraint — both are the caller passing an empty flag by mistake.
    let gpu_class = match request.gpu_class {
        Some(class) => {
            let class = class.trim();
            if class.is_empty() {
                return Err(BuyerError::Protocol(
                    "gpu_class is empty — name a GPU class from `capacity` (e.g. rtx-4090, \
                     h100) or cpu, or omit it to accept any hardware"
                        .into(),
                ));
            }
            Some(class.to_string())
        }
        None => None,
    };
    if request.min_vram_gb == Some(0) {
        return Err(BuyerError::Protocol(
            "min_vram_gb is 0 — give a real floor in whole GB, or omit it to accept any VRAM"
                .into(),
        ));
    }
    // A reputation floor is in basis points, 1..=10_000. A `Some(0)` reads
    // as a real constraint that constrains nothing (every score clears it),
    // and a value past 10_000 asks for a reputation no operator can reach —
    // both are a mistaken flag, refused before a hold rather than turned
    // into a silent no-match.
    match request.min_reputation_bps {
        Some(0) => {
            return Err(BuyerError::Protocol(
                "min_reputation_bps is 0 — give a real floor in basis points (8000 = 80%), or \
                 omit it to accept any reputation"
                    .into(),
            ));
        }
        Some(bps) if bps > 10_000 => {
            return Err(BuyerError::Protocol(format!(
                "min_reputation_bps {bps} is above the 10000 maximum — reputation is a percentage \
                 in basis points, so 8000 means 80%"
            )));
        }
        _ => {}
    }
    // The matcher floors a metered ask against this window. A lease is
    // metered against its signed terms and settlement caps at that window,
    // so the ask must cover the metered window, not the deadline — the
    // deadline carries scheduling slack a lease is never billed for, and
    // scaling a per-hour ask by it would floor an operator above what the
    // escrow could ever pay it. Every other kind floors on the whole job,
    // whose window is the deadline.
    let max_duration_secs = lease_window_secs(request.kind, &request.input)
        .unwrap_or_else(|| (request.deadline_ms / 1_000).max(1) as u32);
    let payload = JobEnvelopePayload {
        job_id,
        buyer: buyer_identity.agent_id(),
        kind: request.kind,
        capability_requirement: CapabilityRequirement {
            gpu_class,
            min_vram_gb: request.min_vram_gb,
            model_id: request.model,
            kind: request.kind,
            max_duration_secs,
            min_reputation_bps: request.min_reputation_bps,
        },
        input: request.input,
        price_micro_usdc: request.price_micro_usdc,
        deadline_ms: request.deadline_ms,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, job_id.to_string()),
        issued_at_ms: epoch_ms(),
        referral_code: config.referral_code.clone(),
        stream,
    };
    SignedJobEnvelope::sign(payload, buyer_identity)
        .map_err(|e| BuyerError::Protocol(e.to_string()))
}

/// Submits an already-signed envelope — verbatim, so the coordinator
/// sees the same bytes on every call — and polls to the verified
/// outcome exactly as [`dispatch_and_verify`] does. Safe to repeat
/// with the same envelope any number of times: a job the coordinator
/// already knows (even one long past its deadline) is acknowledged
/// with its real phase instead of re-held, and the receipt poll then
/// returns the original verified result. Money can only move on the
/// first delivery.
pub async fn dispatch_signed(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
    envelope: SignedJobEnvelope,
) -> Result<DispatchOutcome, BuyerError> {
    submit_envelope(http, config, &envelope).await?;
    poll_receipt_and_verify(http, config, buyer_identity, envelope).await
}

async fn sign_and_submit(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
    request: JobRequest,
    stream: bool,
    job_id: Uuid,
) -> Result<SignedJobEnvelope, BuyerError> {
    let envelope = build_envelope(config, buyer_identity, request, stream, job_id)?;
    submit_envelope(http, config, &envelope).await?;
    Ok(envelope)
}

async fn submit_envelope(
    http: &reqwest::Client,
    config: &BuyerConfig,
    envelope: &SignedJobEnvelope,
) -> Result<(), BuyerError> {
    let job_id = envelope.payload.job_id;
    // Re-submitting the same signed envelope is safe by design — the
    // coordinator refuses a duplicate job_id under its escrow lock and
    // acks with the job's real phase — so transport-class failures
    // retry it instead of giving up. The failure this heals is the ack
    // lost to a coordinator deploy mid-flight: without the retry the
    // buyer walks away believing the job never landed while its funds
    // sit held behind a job that may well complete. An answered
    // refusal (402, 400, ...) is a verdict and stays fatal.
    let base = config.coordinator_url.trim_end_matches('/');
    let url = format!("{base}/federation/jobs");
    let mut backoff = SUBMIT_RETRY_BASE;
    let mut attempt = 0u32;
    loop {
        let failure = match http.post(&url).json(envelope).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return Ok(());
                }
                let body = resp.text().await.unwrap_or_default();
                if !status.is_server_error() && status != reqwest::StatusCode::TOO_MANY_REQUESTS {
                    return Err(BuyerError::SubmitRefused {
                        status: status.as_u16(),
                        body,
                    });
                }
                format!("submit returned {status}: {}", coordinator_reason(&body))
            }
            // The last transport failure names the down coordinator
            // rather than reqwest's URL echo; a lingering 5xx stays a
            // Coordinator error, since the service did answer.
            Err(e) if attempt + 1 >= SUBMIT_TRANSPORT_ATTEMPTS => {
                return Err(BuyerError::unreachable(
                    &config.coordinator_url,
                    "submit this job",
                    &e,
                ));
            }
            Err(e) => format!("submit: {e}"),
        };
        attempt += 1;
        if attempt >= SUBMIT_TRANSPORT_ATTEMPTS {
            return Err(BuyerError::Coordinator(failure));
        }
        tracing::debug!(%job_id, attempt, error = %failure, "submit failed transiently; retrying the same envelope");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(SUBMIT_RETRY_MAX_BACKOFF);
    }
}

/// How hard [`submit_envelope`] tries before conceding the coordinator
/// is down: 5 attempts spanning ~4s of backoff — enough to straddle a
/// deploy blip, short enough that a genuinely dead coordinator errors
/// while the caller still cares.
const SUBMIT_TRANSPORT_ATTEMPTS: u32 = 5;
const SUBMIT_RETRY_BASE: Duration = Duration::from_millis(250);
const SUBMIT_RETRY_MAX_BACKOFF: Duration = Duration::from_secs(4);

/// Polls the receipt to the buyer's own deadline plus one poll of
/// grace — the sweep refunds anything the coordinator never completes,
/// so give an in-flight receipt one last chance to land before giving
/// up. The receipt returns the job's output, so every poll is a signed
/// read (fresh timestamp each attempt — the loop can outlive the skew
/// window).
async fn poll_receipt_and_verify(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer_identity: &LocalIdentity,
    envelope: SignedJobEnvelope,
) -> Result<DispatchOutcome, BuyerError> {
    let base = config.coordinator_url.trim_end_matches('/');
    let job_id = envelope.payload.job_id;
    let issued_at = epoch_ms();
    let deadline_at = issued_at
        .saturating_add(envelope.payload.deadline_ms)
        .saturating_add(5_000);
    let receipt_path = format!("/federation/jobs/{job_id}/receipt");
    loop {
        let signed_at_ms = epoch_ms();
        let signature =
            covenant_compute_protocol::sign_read(buyer_identity, &receipt_path, signed_at_ms)
                .map_err(|e| BuyerError::Protocol(e.to_string()))?;
        // A transport error or a 5xx/429 is the coordinator restarting
        // (or shedding load) behind its stable address, not a verdict
        // on the job — the job itself is journal-durable, so keep
        // polling to the same deadline an in-flight job gets. Any other
        // refusal (bad signature, unknown job) is a real answer and
        // stays fatal.
        let answered = match http
            .get(format!("{base}{receipt_path}"))
            .header(
                covenant_compute_protocol::READ_SIGNED_AT_HEADER,
                signed_at_ms.to_string(),
            )
            .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, signature)
            .send()
            .await
        {
            Ok(poll) => {
                let status = poll.status();
                if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    tracing::debug!(%job_id, %status, "receipt poll refused transiently; retrying until the deadline");
                    None
                } else if !status.is_success() {
                    let body = poll.text().await.unwrap_or_default();
                    return Err(BuyerError::Coordinator(format!(
                        "receipt poll returned {status}: {}",
                        coordinator_reason(&body)
                    )));
                } else {
                    Some(poll)
                }
            }
            Err(e) => {
                tracing::debug!(%job_id, error = %e, "receipt poll failed; retrying until the deadline");
                None
            }
        };
        if let Some(poll) = answered {
            let job: JobStatusResponse = poll
                .json()
                .await
                .map_err(|e| BuyerError::Coordinator(format!("receipt poll decode: {e}")))?;

            match job.status.as_str() {
                "completed" => {
                    let receipt = job.receipt.ok_or_else(|| {
                        BuyerError::Coordinator("completed job carries no receipt".into())
                    })?;
                    let output = job.output.unwrap_or_default();
                    verify_receipt(&envelope, &receipt, &output)?;
                    // The operator's own signed status is the last word on
                    // whether the work succeeded. An honest coordinator only
                    // ever completes a status-Ok receipt (it refunds and
                    // fails a non-Ok one), so this never fires against one —
                    // but the buyer re-checks every signed commitment locally
                    // rather than trust the coordinator's relay, and a
                    // non-Ok receipt reported as completed means a
                    // misbehaving coordinator would have it book spend for
                    // work the operator disclaimed. Surface it as a failure,
                    // as the "failed" phase and covenantd's native path do.
                    if receipt.receipt.status != A2ATaskStatus::Ok {
                        return Err(BuyerError::NotServed {
                            job_id,
                            status: "failed".into(),
                            reason: job.refund_reason,
                            detail: verified_failure_detail(
                                &envelope,
                                Some(&receipt),
                                Some(&output),
                            ),
                        });
                    }
                    return Ok(DispatchOutcome {
                        envelope,
                        receipt,
                        output,
                        payout: job.payout,
                    });
                }
                // "failed" is a real execution failure the operator signed
                // an Error receipt for; the coordinator refunded the hold.
                // All three mean the same to the ledger — not served,
                // nothing owed — but a `failed` job carries the operator's
                // signed cause as its output, so surface that "why" once it
                // re-verifies locally. `refunded`/`rejected` are
                // coordinator-side refunds with no operator statement.
                "failed" => {
                    let detail = verified_failure_detail(
                        &envelope,
                        job.receipt.as_ref(),
                        job.output.as_ref(),
                    );
                    return Err(BuyerError::NotServed {
                        job_id,
                        status: job.status,
                        reason: job.refund_reason,
                        detail,
                    });
                }
                "refunded" | "rejected" => {
                    return Err(BuyerError::NotServed {
                        job_id,
                        status: job.status,
                        reason: job.refund_reason,
                        detail: None,
                    })
                }
                _ => {}
            }
        }
        if epoch_ms() > deadline_at {
            return Err(BuyerError::ReceiptTimeout(job_id));
        }
        tokio::time::sleep(config.poll_interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_a2a::A2ATaskStatus;
    use covenant_compute_protocol::{JobMeter, WorkReceiptPayload};

    #[test]
    fn spend_caps_refuse_a_call_over_the_per_call_ceiling() {
        let caps = SpendCaps::new(1_000, None);
        assert!(
            caps.per_call_refusal(1_000).is_none(),
            "at the ceiling buys"
        );
        let refusal = caps
            .per_call_refusal(1_001)
            .expect("over the ceiling refuses");
        assert_eq!(
            refusal,
            "offered price 1001 micro-USDC exceeds the per-call ceiling 1000"
        );
    }

    #[test]
    fn spend_caps_with_no_session_cap_never_refuse_on_the_total() {
        let caps = SpendCaps::new(1_000, None);
        caps.record_spend(u64::MAX - 1);
        assert!(caps.session_refusal(1_000, 1_000).is_none());
    }

    #[test]
    fn spend_caps_count_settled_spend_and_in_flight_against_the_session_cap() {
        let caps = SpendCaps::new(1_000, Some(500));
        // Fresh: an offer within the cap clears.
        assert!(caps.session_refusal(200, 0).is_none());
        // 400 in flight + 200 offered > 500: refused, naming each part.
        let refusal = caps
            .session_refusal(200, 400)
            .expect("over the cap refuses");
        assert_eq!(
            refusal,
            "session spend cap reached: 0 spent + 400 in flight + 200 offered exceeds 500 \
             micro-USDC"
        );
        // Settle 400: now even a 0-in-flight offer of 200 crosses.
        caps.record_spend(400);
        assert_eq!(caps.spent_micro_usdc(), 400);
        assert!(caps.session_refusal(200, 0).is_some());
        // Exactly-full headroom still buys — the cap is a ceiling.
        assert!(caps.session_refusal(100, 0).is_none());
    }

    #[test]
    fn spend_caps_saturate_rather_than_overflow_the_session_math() {
        let caps = SpendCaps::new(u64::MAX, Some(u64::MAX));
        caps.record_spend(u64::MAX);
        // spent + in_flight + price would overflow u64; saturating math
        // pins it at MAX, which is not strictly greater than the MAX cap,
        // so this does not spuriously refuse.
        assert!(caps.session_refusal(10, 10).is_none());
    }

    #[test]
    fn a_reservation_holds_against_the_cap_until_it_settles() {
        let caps = Arc::new(SpendCaps::new(1_000, Some(500)));
        let first = caps.try_reserve(300).expect("300 fits");
        // The reservation counts against the cap the moment it's taken: a
        // second offer that would cross 500 refuses, even though nothing
        // has settled yet.
        let refusal = caps.try_reserve(300).expect_err("300 + 300 > 500");
        assert!(refusal.contains("300 in flight"), "got: {refusal}");
        // A stateless session check sees the reservation too.
        assert!(caps.session_refusal(300, 0).is_some());
        // Settle the first buy; the whole hold becomes settled spend and
        // only that figure stays counted.
        first.settle();
        assert_eq!(caps.spent_micro_usdc(), 300);
        // Now 200 more fits (300 spent + 200 = 500), 201 does not.
        let second = caps.try_reserve(200).expect("200 fits at the edge");
        second.settle();
        assert_eq!(caps.spent_micro_usdc(), 500);
        assert!(caps.try_reserve(1).is_err());
    }

    #[test]
    fn a_dropped_reservation_releases_its_hold() {
        let caps = Arc::new(SpendCaps::new(1_000, Some(500)));
        {
            let _held = caps.try_reserve(400).expect("400 fits");
            assert!(caps.try_reserve(200).is_err(), "400 held + 200 > 500");
        }
        // The guard dropped without settling — the buy failed, nothing is
        // charged, and the full headroom is back.
        assert_eq!(caps.spent_micro_usdc(), 0);
        let retry = caps.try_reserve(500).expect("full headroom restored");
        retry.settle();
        assert_eq!(caps.spent_micro_usdc(), 500);
    }

    #[test]
    fn a_reservation_settles_at_the_offer_it_held() {
        // The coordinator escrows and releases the envelope price, so the
        // whole hold becomes settled spend — a completing operator's lower
        // receipt price cannot reduce what counts against the session cap.
        // Settling at a node-reported price would let a node under-report
        // its way past a buyer's session limit while the buyer is charged
        // the full offer.
        let caps = Arc::new(SpendCaps::new(1_000, Some(1_000)));
        let held = caps.try_reserve(300).expect("300 fits");
        held.settle();
        assert_eq!(caps.spent_micro_usdc(), 300);
    }

    #[test]
    fn try_reserve_refuses_over_the_per_call_ceiling_before_touching_the_cap() {
        let caps = Arc::new(SpendCaps::new(100, Some(u64::MAX)));
        let refusal = caps
            .try_reserve(101)
            .expect_err("over the per-call ceiling");
        assert!(refusal.contains("per-call ceiling"), "got: {refusal}");
        // Nothing was reserved, so a within-ceiling buy still has the cap.
        assert!(caps.try_reserve(100).is_ok());
    }

    #[test]
    fn try_reserve_with_no_session_cap_only_bounds_per_call() {
        let caps = Arc::new(SpendCaps::new(100, None));
        let a = caps.try_reserve(100).expect("at the ceiling");
        let b = caps.try_reserve(100).expect("no session cap to hit");
        a.settle();
        b.settle();
        assert_eq!(caps.spent_micro_usdc(), 200);
    }

    #[test]
    fn concurrent_reservations_never_exceed_the_cap() {
        use std::sync::Arc;
        let caps = Arc::new(SpendCaps::new(1, Some(100)));
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let caps = Arc::clone(&caps);
                std::thread::spawn(move || {
                    let mut taken = 0u64;
                    // Keep buying until the cap is *settled* full rather than
                    // for a fixed number of tries: a reservation another thread
                    // holds over the last unit makes `try_reserve` refuse while
                    // settled spend is still below the cap, so a fixed budget
                    // could quit early and leave the cap under-filled.
                    loop {
                        match caps.try_reserve(1) {
                            Ok(r) => {
                                r.settle();
                                taken += 1;
                            }
                            Err(_) if caps.spent_micro_usdc() >= 100 => break,
                            Err(_) => std::thread::yield_now(),
                        }
                    }
                    taken
                })
            })
            .collect();
        let total: u64 = threads.into_iter().map(|t| t.join().unwrap()).sum();
        // 16 threads race for a cap of 100 one unit at a time: exactly the
        // cap's worth settle, never one more, and no reservation leaks.
        assert_eq!(total, 100);
        assert_eq!(caps.spent_micro_usdc(), 100);
        assert!(caps.try_reserve(1).is_err());
    }

    /// [`http_client`] is what makes every consumer's traffic
    /// version-declared: a coordinator that only answers requests
    /// declaring the current wire version serves it, and refuses a
    /// bare `reqwest::Client` (which reads as pre-versioning).
    #[tokio::test]
    async fn the_shared_client_declares_the_wire_version() {
        async fn handler(headers: axum::http::HeaderMap) -> axum::response::Response {
            use axum::response::IntoResponse;
            let declared = headers
                .get(covenant_compute_protocol::PROTOCOL_VERSION_HEADER)
                .and_then(|v| v.to_str().ok());
            if declared
                == Some(
                    covenant_compute_protocol::PROTOCOL_VERSION
                        .to_string()
                        .as_str(),
                )
            {
                axum::Json(CapacityView {
                    registered_operators: 0,
                    matchable_operators: 0,
                    liveness_window_ms: 0,
                    min_score_bps: 0,
                    min_bond_micro_usdc: 0,
                    entries: vec![],
                })
                .into_response()
            } else {
                (axum::http::StatusCode::UPGRADE_REQUIRED, "version 0").into_response()
            }
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let router =
                axum::Router::new().route("/federation/capacity", axum::routing::get(handler));
            axum::serve(listener, router).await.unwrap();
        });
        let config = BuyerConfig {
            coordinator_url: format!("http://{addr}"),
            poll_interval: Duration::from_millis(10),
            referral_code: None,
            rpc_url: None,
        };

        capacity(&http_client(), &config)
            .await
            .expect("the shared client declares the current version");
        let refused = capacity(&reqwest::Client::new(), &config)
            .await
            .expect_err("a bare client declares nothing and reads as version 0");
        assert!(refused.to_string().contains("version 0"));
    }

    #[tokio::test]
    async fn cheapest_matching_ask_narrows_to_the_hardware_that_could_serve() {
        use covenant_compute_protocol::PriceUnit;
        let entry = |model: &str, ask, vram, class: &str| CapacityEntry {
            kind: JobKind::BatchJob,
            model: model.into(),
            operators: 1,
            min_ask_micro_usdc: ask,
            min_ask_unit: PriceUnit::PerJob,
            max_ask_micro_usdc: ask,
            max_vram_gb: vram,
            gpu_classes: vec![class.into()],
            tee_capable: false,
        };
        let view = CapacityView {
            registered_operators: 2,
            matchable_operators: 2,
            liveness_window_ms: 45_000,
            min_score_bps: 0,
            min_bond_micro_usdc: 0,
            entries: vec![
                entry("cpu-batch", 100, 0, "cpu"),
                entry("gpu-batch", 5_000, 80, "h100"),
            ],
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let router = axum::Router::new().route(
                "/federation/capacity",
                axum::routing::get(move || {
                    let view = view.clone();
                    async move { axum::Json(view) }
                }),
            );
            axum::serve(listener, router).await.unwrap();
        });
        let config = BuyerConfig {
            coordinator_url: format!("http://{addr}"),
            poll_interval: Duration::from_millis(10),
            referral_code: None,
            rpc_url: None,
        };
        let http = http_client();
        let ask = |gpu, vram| {
            cheapest_matching_ask(&http, &config, JobKind::BatchJob, None, gpu, vram, None)
        };

        // No constraint: the cheapest batch node of any kind.
        assert_eq!(ask(None, None).await.unwrap(), Some(100));
        // A class narrows to the row that declares it.
        assert_eq!(ask(Some("h100"), None).await.unwrap(), Some(5_000));
        assert_eq!(ask(Some("cpu"), None).await.unwrap(), Some(100));
        // A VRAM floor excludes the CPU row.
        assert_eq!(ask(None, Some(80)).await.unwrap(), Some(5_000));
        // An ask nothing serves is a clean None, before any dispatch.
        assert_eq!(ask(Some("a100"), None).await.unwrap(), None);
    }

    #[test]
    fn a_sub_second_deadline_is_refused_before_signing() {
        let buyer = LocalIdentity::generate("buyer@deadline");
        let config = BuyerConfig {
            coordinator_url: "http://127.0.0.1:0".into(),
            poll_interval: Duration::from_millis(10),
            referral_code: None,
            rpc_url: None,
        };
        let request = |deadline_ms| JobRequest {
            kind: JobKind::InferenceCall,
            input: vec![Content::text("hi")],
            model: None,
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps: None,
            price_micro_usdc: 10,
            deadline_ms,
        };
        // A deadline typed in seconds (60) reads as 60ms: refused with the
        // unit named, before anything is signed or held.
        let err = sign_envelope(&config, &buyer, request(60)).expect_err("below floor");
        assert!(matches!(err, BuyerError::Protocol(_)), "{err:?}");
        assert!(err.to_string().contains("milliseconds"), "{err}");
        assert!(
            sign_envelope(&config, &buyer, request(0)).is_err(),
            "a zero deadline is refused"
        );
        // Exactly at the floor signs.
        assert!(sign_envelope(&config, &buyer, request(MIN_DEADLINE_MS)).is_ok());
    }

    #[test]
    fn a_hardware_ask_rides_the_envelope_and_an_empty_one_is_refused() {
        let buyer = LocalIdentity::generate("buyer@hardware");
        let config = BuyerConfig {
            coordinator_url: "http://127.0.0.1:0".into(),
            poll_interval: Duration::from_millis(10),
            referral_code: None,
            rpc_url: None,
        };
        let request = |gpu_class: Option<&str>, min_vram_gb| JobRequest {
            kind: JobKind::BatchJob,
            input: vec![Content::text("nvidia-smi")],
            model: None,
            gpu_class: gpu_class.map(str::to_string),
            min_vram_gb,
            min_reputation_bps: None,
            price_micro_usdc: 10,
            deadline_ms: 30_000,
        };

        // The ask round-trips into the signed requirement, trimmed.
        let signed =
            sign_envelope(&config, &buyer, request(Some("  h100  "), Some(80))).expect("signs");
        let req = &signed.payload.capability_requirement;
        assert_eq!(req.gpu_class.as_deref(), Some("h100"));
        assert_eq!(req.min_vram_gb, Some(80));

        // An empty (or whitespace-only) class names no hardware.
        let err = sign_envelope(&config, &buyer, request(Some("  "), None)).expect_err("empty");
        assert!(matches!(err, BuyerError::Protocol(_)), "{err:?}");
        assert!(err.to_string().contains("gpu_class"), "{err}");

        // A zero VRAM floor reads as a real constraint that asks nothing.
        let err = sign_envelope(&config, &buyer, request(None, Some(0))).expect_err("zero vram");
        assert!(err.to_string().contains("min_vram_gb"), "{err}");

        // Omitting both is the unconstrained default.
        let signed = sign_envelope(&config, &buyer, request(None, None)).expect("signs");
        let req = &signed.payload.capability_requirement;
        assert!(req.gpu_class.is_none() && req.min_vram_gb.is_none());
    }

    #[test]
    fn a_reputation_floor_rides_the_envelope_and_a_bogus_one_is_refused() {
        let buyer = LocalIdentity::generate("buyer@reputation");
        let config = BuyerConfig {
            coordinator_url: "http://127.0.0.1:0".into(),
            poll_interval: Duration::from_millis(10),
            referral_code: None,
            rpc_url: None,
        };
        let request = |min_reputation_bps| JobRequest {
            kind: JobKind::InferenceCall,
            input: vec![Content::text("hi")],
            model: None,
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps,
            price_micro_usdc: 10,
            deadline_ms: 30_000,
        };

        // A real floor round-trips into the signed requirement.
        let signed = sign_envelope(&config, &buyer, request(Some(8_000))).expect("signs");
        assert_eq!(
            signed.payload.capability_requirement.min_reputation_bps,
            Some(8_000)
        );

        // Zero reads as a real floor that constrains nothing.
        let err = sign_envelope(&config, &buyer, request(Some(0))).expect_err("zero floor");
        assert!(err.to_string().contains("min_reputation_bps"), "{err}");

        // Above the 10000 basis-point maximum is a mistaken flag, not a
        // reputation no operator can reach.
        let err = sign_envelope(&config, &buyer, request(Some(10_001))).expect_err("over max");
        assert!(err.to_string().contains("10000"), "{err}");

        // Omitting it places no floor.
        let signed = sign_envelope(&config, &buyer, request(None)).expect("signs");
        assert!(signed
            .payload
            .capability_requirement
            .min_reputation_bps
            .is_none());
    }

    fn envelope_and_receipt(
        price: u64,
        output: &[Content],
    ) -> (SignedJobEnvelope, SignedWorkReceipt, LocalIdentity) {
        let buyer = LocalIdentity::generate("buyer@test");
        let operator = LocalIdentity::generate("operator@test");
        let job_id = Uuid::new_v4();
        let payload = JobEnvelopePayload {
            job_id,
            buyer: buyer.agent_id(),
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
            price_micro_usdc: price,
            deadline_ms: 30_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "verify-test"),
            issued_at_ms: 1,
            referral_code: None,
            stream: false,
        };
        let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
        let receipt = SignedWorkReceipt::sign(
            WorkReceiptPayload {
                job_id,
                operator: operator.agent_id(),
                job_hash_hex: sha256_hex(envelope.payload_json.as_bytes()),
                result_hash_hex: output_hash_hex(output),
                meter: JobMeter {
                    wall_ms: 5,
                    tokens_in: None,
                    tokens_out: None,
                    gpu_seconds: None,
                    finish_reason: None,
                },
                price_micro_usdc: price,
                status: A2ATaskStatus::Ok,
                executed_at_ms: 2,
                node_audit_root_hex: "cc".repeat(32),
            },
            &operator,
        )
        .unwrap();
        (envelope, receipt, operator)
    }

    #[test]
    fn verify_receipt_accepts_a_consistent_receipt() {
        let output = vec![Content::text("result")];
        let (envelope, receipt, _) = envelope_and_receipt(1_000, &output);
        verify_receipt(&envelope, &receipt, &output).expect("consistent receipt verifies");
    }

    #[test]
    fn fetched_receipt_verifies_by_signature_job_and_output_hash() {
        let output = vec![Content::text("result")];
        let (envelope, receipt, _) = envelope_and_receipt(1_000, &output);
        let job_id = envelope.payload.job_id;
        verify_fetched_receipt(&receipt, job_id, &output).expect("a consistent re-read verifies");
        // A receipt that names a different job is caught.
        let err = verify_fetched_receipt(&receipt, Uuid::new_v4(), &output).unwrap_err();
        assert!(err.to_string().contains("not"), "got: {err}");
        // Output that does not hash to the receipt is caught — the whole
        // point of the re-read is proving the bytes are the paid-for work.
        let tampered = vec![Content::text("tampered")];
        let err = verify_fetched_receipt(&receipt, job_id, &tampered).unwrap_err();
        assert!(err.to_string().contains("result_hash_hex"), "got: {err}");
    }

    #[test]
    fn a_re_read_judges_success_by_the_operators_signed_status_not_the_relay() {
        // A failure receipt hashes to its own failure text, so it passes the
        // signature/job/hash checks and `receipt_verified` reads clean. Success
        // must still turn on the operator's signed status: a coordinator that
        // serves an Error receipt as a completed, un-refunded job must not have
        // its failure text taken for the paid answer — the buy path refuses the
        // same shape as NotServed.
        let output = vec![Content::text("the model refused to answer")];
        let (envelope, ok_receipt, operator) = envelope_and_receipt(1_000, &output);
        let job_id = envelope.payload.job_id;
        let error_receipt = SignedWorkReceipt::sign(
            WorkReceiptPayload {
                status: A2ATaskStatus::Error,
                ..ok_receipt.receipt.clone()
            },
            &operator,
        )
        .unwrap();
        let view = |receipt: Option<SignedWorkReceipt>| JobOutputView {
            job_id,
            status: "completed".into(),
            refund_reason: None,
            output: output.clone(),
            receipt,
            receipt_verified: Some(true),
            verification_error: None,
            payout: None,
            charged_micro_usdc: None,
        };
        assert!(view(Some(error_receipt)).receipt_reports_failure());
        assert!(!view(Some(ok_receipt)).receipt_reports_failure());
        assert!(!view(None).receipt_reports_failure(), "no verdict yet");
    }

    #[test]
    fn a_buyer_row_parses_the_settled_charge_and_defaults_it_when_absent() {
        let (envelope, _receipt, _operator) =
            envelope_and_receipt(3_600_000, &[Content::text("x")]);
        let base = serde_json::json!({
            "job_id": envelope.payload.job_id,
            "status": "completed",
            "price_micro_usdc": 3_600_000,
            "funding_source": "organic",
            "issued_at_ms": 1,
            "envelope": serde_json::to_value(&envelope).unwrap(),
            "receipt": null,
        });
        // A current coordinator reports the metered charge alongside the ceiling.
        let mut with_charge = base.clone();
        with_charge["charged_micro_usdc"] = serde_json::json!(60_000);
        let row: BuyerJobRowWire = serde_json::from_value(with_charge).unwrap();
        assert_eq!(row.price_micro_usdc, 3_600_000);
        assert_eq!(row.charged_micro_usdc, Some(60_000));
        // A coordinator too old to report it parses to None, not a decode error.
        let row: BuyerJobRowWire = serde_json::from_value(base).unwrap();
        assert_eq!(row.charged_micro_usdc, None);
    }

    #[test]
    fn a_reread_parses_the_settled_charge_and_defaults_it_when_absent() {
        // The `output` re-read carries the same authoritative charge the job
        // list does, so a lease read back by id shows the seconds it ran, not
        // its window ceiling.
        let base = serde_json::json!({ "status": "completed", "receipt": null });
        let mut with_charge = base.clone();
        with_charge["charged_micro_usdc"] = serde_json::json!(60_000);
        let job: JobStatusResponse = serde_json::from_value(with_charge).unwrap();
        assert_eq!(job.charged_micro_usdc, Some(60_000));
        // A coordinator too old to report it parses to None, not a decode error.
        let job: JobStatusResponse = serde_json::from_value(base).unwrap();
        assert_eq!(job.charged_micro_usdc, None);
    }

    #[test]
    fn an_empty_stop_list_is_dropped_rather_than_refused() {
        // OpenAI `stop: []`, Anthropic `stop_sequences: []`, and a hand-built
        // compute.infer all treat an empty stop list as a no-op, but the
        // protocol refuses an empty one. InferArgs::input drops it to None, so
        // a valid request is not turned into a 400 for every dialect at once.
        let with_empty: InferArgs = serde_json::from_value(serde_json::json!({
            "prompt": "hi",
            "stop": [],
        }))
        .unwrap();
        let input = with_empty
            .input()
            .expect("an empty stop list is a no-op, not a refusal");
        assert!(
            !input
                .iter()
                .any(|c| matches!(c, Content::Json { value } if value.get("generation").is_some())),
            "an empty stop alone carries no generation block"
        );

        // A populated stop still rides through into the signed generation block.
        let with_stop: InferArgs = serde_json::from_value(serde_json::json!({
            "prompt": "hi",
            "stop": ["\n\n"],
        }))
        .unwrap();
        let input = with_stop.input().expect("a real stop is carried");
        assert!(
            input
                .iter()
                .any(|c| matches!(c, Content::Json { value } if value.get("generation").is_some())),
            "a populated stop produces a generation block"
        );
    }

    #[test]
    fn only_a_402_refusal_reads_as_underfunded() {
        assert!(BuyerError::SubmitRefused {
            status: 402,
            body: "insufficient funds".into()
        }
        .is_underfunded());
        // A 400 verdict on the bytes and a 409 subsidy-exhausted are
        // real refusals a deposit does not heal — they must not be
        // dressed up as "just top up".
        assert!(!BuyerError::SubmitRefused {
            status: 400,
            body: "malformed".into()
        }
        .is_underfunded());
        assert!(!BuyerError::SubmitRefused {
            status: 409,
            body: "subsidy exhausted".into()
        }
        .is_underfunded());
        assert!(!BuyerError::Coordinator("down".into()).is_underfunded());
    }

    #[test]
    fn a_coordinator_refusal_reads_as_the_reason_not_the_json_envelope() {
        // The coordinator answers every refusal as {"error": "<reason>"};
        // the buyer must show the reason, not the braces-and-quotes around
        // it. A submission over the payout cap, under the cheapest ask, or
        // past its deadline all arrive this way.
        let refused = BuyerError::SubmitRefused {
            status: 400,
            body: r#"{"error":"job 7f: the offer of 900 micro-USDC is under the cheapest ask (1000); raise the price"}"#.into(),
        };
        let msg = refused.to_string();
        assert!(
            msg.contains("raise the price"),
            "surfaces the reason: {msg}"
        );
        assert!(
            !msg.contains(r#"{"error""#),
            "drops the JSON envelope: {msg}"
        );
    }

    #[test]
    fn an_unreachable_coordinator_reads_as_a_diagnosis_not_a_reqwest_echo() {
        let e = BuyerError::Unreachable {
            doing: "read this buyer's balance",
            url: "http://coordinator.example:8080".into(),
            why: "refused the connection",
        };
        let msg = e.to_string();
        // One line, the URL once, and it names both what failed and the
        // knob to check — the opposite of reqwest's twice-repeated
        // "error sending request for url (…)".
        assert_eq!(msg.lines().count(), 1, "single line: {msg}");
        assert_eq!(
            msg.matches("http://coordinator.example:8080").count(),
            1,
            "url named once: {msg}"
        );
        assert!(msg.contains("read this buyer's balance"), "{msg}");
        assert!(msg.contains("COVENANT_COMPUTE_COORDINATOR_URL"), "{msg}");
        assert!(!msg.contains("error sending request"), "{msg}");
    }

    #[test]
    fn verify_receipt_rejects_a_receipt_for_a_different_envelope() {
        let output = vec![Content::text("result")];
        let (_, receipt, _) = envelope_and_receipt(1_000, &output);
        let (other_envelope, _, _) = envelope_and_receipt(1_000, &output);
        let err = verify_receipt(&other_envelope, &receipt, &output).unwrap_err();
        assert!(err.to_string().contains("job"), "got: {err}");
    }

    #[test]
    fn verify_receipt_rejects_output_that_does_not_match_the_hash() {
        let output = vec![Content::text("result")];
        let (envelope, receipt, _) = envelope_and_receipt(1_000, &output);
        let tampered = vec![Content::text("tampered")];
        let err = verify_receipt(&envelope, &receipt, &tampered).unwrap_err();
        assert!(err.to_string().contains("result_hash_hex"), "got: {err}");
    }

    #[test]
    fn verify_receipt_rejects_a_price_above_the_offer() {
        let output = vec![Content::text("result")];
        let (envelope, _, operator) = envelope_and_receipt(1_000, &output);
        let inflated = SignedWorkReceipt::sign(
            WorkReceiptPayload {
                job_id: envelope.payload.job_id,
                operator: operator.agent_id(),
                job_hash_hex: sha256_hex(envelope.payload_json.as_bytes()),
                result_hash_hex: output_hash_hex(&output),
                meter: JobMeter {
                    wall_ms: 5,
                    tokens_in: None,
                    tokens_out: None,
                    gpu_seconds: None,
                    finish_reason: None,
                },
                price_micro_usdc: 2_000,
                status: A2ATaskStatus::Ok,
                executed_at_ms: 2,
                node_audit_root_hex: "cc".repeat(32),
            },
            &operator,
        )
        .unwrap();
        let err = verify_receipt(&envelope, &inflated, &output).unwrap_err();
        assert!(err.to_string().contains("more than"), "got: {err}");
    }

    #[test]
    fn a_failed_jobs_detail_is_the_operators_signed_cause_only_when_it_verifies() {
        let output = vec![Content::text(
            "backend returned 400: this model cannot process the request",
        )];
        let (envelope, receipt, _op) = envelope_and_receipt(1_000, &output);
        // The receipt hashes this output, so it re-verifies and the cause
        // surfaces — the buyer reads "why", not a bare "execution_failed".
        let detail = verified_failure_detail(&envelope, Some(&receipt), Some(&output))
            .expect("a consistent failure receipt yields its cause");
        assert!(
            detail.contains("this model cannot process the request"),
            "got: {detail}"
        );
        // A coordinator that swaps the output for its own text fails the
        // hash check: the buyer surfaces nothing rather than unsigned text.
        let forged = vec![Content::text("blame the operator")];
        assert!(verified_failure_detail(&envelope, Some(&receipt), Some(&forged)).is_none());
        // Missing pieces (an old coordinator, a refund with no receipt) are
        // simply no detail, never a panic.
        assert!(verified_failure_detail(&envelope, None, Some(&output)).is_none());
        assert!(verified_failure_detail(&envelope, Some(&receipt), None).is_none());

        // Control bytes a hostile operator might smuggle into text the buyer
        // renders are stripped — after the hash check, over the same signed
        // bytes.
        let sneaky = vec![Content::text("cause\u{7}\u{1b}[31m with escapes")];
        let (env2, rec2, _) = envelope_and_receipt(1_000, &sneaky);
        let cleaned = verified_failure_detail(&env2, Some(&rec2), Some(&sneaky)).unwrap();
        assert!(
            !cleaned.contains('\u{7}') && !cleaned.contains('\u{1b}'),
            "control bytes stripped: {cleaned:?}"
        );
    }

    /// The link is a convenience for a human checking the proof; it must
    /// point at the cluster the verification actually read, or a devnet
    /// payout "verifies" against a mainnet 404.
    #[test]
    fn explorer_links_are_cluster_tagged_from_the_rpc_endpoint() {
        assert_eq!(
            explorer_tx_url("https://api.devnet.solana.com", "5ig"),
            "https://explorer.solana.com/tx/5ig?cluster=devnet"
        );
        assert_eq!(
            explorer_tx_url("https://api.testnet.solana.com", "5ig"),
            "https://explorer.solana.com/tx/5ig?cluster=testnet"
        );
        assert_eq!(
            explorer_tx_url("https://api.mainnet-beta.solana.com", "5ig"),
            "https://explorer.solana.com/tx/5ig"
        );
    }

    #[test]
    fn infer_tool_spec_offers_prompt_and_messages_and_states_the_ceiling() {
        let spec = infer_tool_spec(123_456);
        assert_eq!(spec.name, INFER_TOOL);
        assert!(spec.description.contains("123456"));
        assert!(spec.input_schema["properties"]["prompt"].is_object());
        assert_eq!(
            spec.input_schema["properties"]["messages"]["items"]["required"][0],
            "role"
        );
    }

    #[test]
    fn verify_tool_spec_requires_only_a_job_id() {
        let spec = verify_tool_spec();
        assert_eq!(spec.name, VERIFY_TOOL);
        assert!(spec.input_schema["properties"]["job_id"].is_object());
        assert_eq!(spec.input_schema["required"][0], "job_id");
        let args: VerifyArgs =
            serde_json::from_value(serde_json::json!({"job_id": Uuid::new_v4()})).unwrap();
        let _ = args.job_id;
        assert!(
            serde_json::from_value::<VerifyArgs>(serde_json::json!({"job_id": "not-a-uuid"}))
                .is_err()
        );
    }

    #[test]
    fn stream_start_spec_takes_infer_arguments_minus_the_idempotency_key() {
        let spec = stream_start_tool_spec(777);
        assert_eq!(spec.name, STREAM_START_TOOL);
        assert!(spec.description.contains("777"));
        // Streaming cannot back exactly-once and has nothing to preview, so
        // it must advertise neither the key nor dry_run. Everything else is
        // infer's surface verbatim.
        let props = &spec.input_schema["properties"];
        assert!(props["prompt"].is_object());
        assert!(props["messages"].is_object());
        assert!(props["price_micro_usdc"].is_object());
        assert!(
            props.get("idempotency_key").is_none(),
            "streaming must not advertise an idempotency key it ignores"
        );
        assert!(
            props.get("dry_run").is_none(),
            "streaming must not advertise a preview it can't give"
        );
        let mut infer_schema = infer_tool_spec(777).input_schema;
        let infer_props = infer_schema["properties"].as_object_mut().unwrap();
        infer_props.remove("idempotency_key");
        infer_props.remove("dry_run");
        assert_eq!(spec.input_schema, infer_schema);
    }

    #[test]
    fn stream_poll_args_default_the_cursor_to_zero() {
        let spec = stream_poll_tool_spec();
        assert_eq!(spec.name, STREAM_POLL_TOOL);
        assert_eq!(spec.input_schema["required"][0], "job_id");
        let args: StreamPollArgs =
            serde_json::from_value(serde_json::json!({"job_id": Uuid::new_v4()})).unwrap();
        assert_eq!(args.since, 0);
        let args: StreamPollArgs =
            serde_json::from_value(serde_json::json!({"job_id": Uuid::new_v4(), "since": 42}))
                .unwrap();
        assert_eq!(args.since, 42);
        assert!(serde_json::from_value::<StreamPollArgs>(
            serde_json::json!({"job_id": "not-a-uuid"})
        )
        .is_err());
    }

    #[test]
    fn infer_args_map_a_prompt_to_one_text_block() {
        let args: InferArgs = serde_json::from_value(serde_json::json!({"prompt": "hi"})).unwrap();
        assert_eq!(args.input().unwrap(), vec![Content::text("hi")]);
    }

    #[test]
    fn infer_args_attach_images_to_the_prompt_as_a_vision_message() {
        let args: InferArgs = serde_json::from_value(serde_json::json!({
            "prompt": "what is this?",
            "images": ["aGVsbG8="],
        }))
        .unwrap();
        let messages = covenant_compute_protocol::parse_chat_input(&args.input().unwrap())
            .expect("well-formed")
            .expect("images make it a chat job");
        assert_eq!(
            messages,
            vec![ChatMessage::user_with_images(
                "what is this?",
                vec!["aGVsbG8=".into()]
            )]
        );
    }

    #[test]
    fn infer_args_refuse_images_alongside_messages() {
        let args: InferArgs = serde_json::from_value(serde_json::json!({
            "messages": [{"role": "user", "content": "hi"}],
            "images": ["aGVsbG8="],
        }))
        .unwrap();
        let err = args.input().expect_err("images belong on a message here");
        assert!(err.contains("top-level images"), "got: {err}");
    }

    #[test]
    fn infer_args_refuse_an_image_that_is_not_base64() {
        let args: InferArgs = serde_json::from_value(serde_json::json!({
            "prompt": "what is this?",
            "images": ["not base64!!"],
        }))
        .unwrap();
        let err = args.input().expect_err("bad base64");
        assert!(err.contains("valid base64"), "got: {err}");
    }

    #[test]
    fn infer_args_pack_messages_as_chat_input() {
        let args: InferArgs = serde_json::from_value(serde_json::json!({
            "messages": [
                {"role": "system", "content": "one word"},
                {"role": "user", "content": "sky color?"}
            ]
        }))
        .unwrap();
        let input = args.input().unwrap();
        let parsed = covenant_compute_protocol::parse_chat_input(&input)
            .expect("well-formed")
            .expect("chat-shaped");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].content, "sky color?");
    }

    #[test]
    fn infer_args_refuse_both_neither_and_empty_inputs() {
        let both: InferArgs =
            serde_json::from_value(serde_json::json!({"prompt": "p", "messages": []})).unwrap();
        assert!(both.input().unwrap_err().contains("not both"));
        let neither: InferArgs = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(neither.input().unwrap_err().contains("required"));
        let empty: InferArgs = serde_json::from_value(serde_json::json!({"messages": []})).unwrap();
        assert!(empty.input().unwrap_err().contains("empty"));
    }

    #[test]
    fn infer_args_pack_sampling_knobs_as_a_signed_generation_block() {
        let args: InferArgs = serde_json::from_value(serde_json::json!({
            "prompt": "2 + 2?",
            "temperature": 0.0,
            "seed": 7,
            "max_tokens": 64
        }))
        .unwrap();
        let input = args.input().unwrap();
        assert_eq!(input[0], Content::text("2 + 2?"));
        let params = covenant_compute_protocol::parse_generation_params(&input)
            .expect("well-formed")
            .expect("present");
        assert_eq!(params.temperature, Some(0.0));
        assert_eq!(params.seed, Some(7));
        assert_eq!(params.max_tokens, Some(64));
        assert_eq!(params.top_p, None);
    }

    #[test]
    fn infer_args_pack_a_response_format_into_the_generation_block() {
        // The flat protocol shape an MCP client or covenantd sends.
        let args: InferArgs = serde_json::from_value(serde_json::json!({
            "prompt": "list three colors",
            "response_format": {
                "type": "json_schema",
                "name": "colors",
                "schema": { "type": "object" },
                "strict": true
            }
        }))
        .unwrap();
        let input = args.input().unwrap();
        let params = covenant_compute_protocol::parse_generation_params(&input)
            .expect("well-formed")
            .expect("present");
        assert_eq!(
            params.response_format,
            Some(ResponseFormat::JsonSchema {
                name: "colors".into(),
                schema: serde_json::json!({ "type": "object" }),
                strict: Some(true),
            })
        );
    }

    #[test]
    fn infer_args_response_format_alone_packs_a_generation_block() {
        let args: InferArgs = serde_json::from_value(serde_json::json!({
            "prompt": "reply in json",
            "response_format": { "type": "json_object" }
        }))
        .unwrap();
        let input = args.input().unwrap();
        let params = covenant_compute_protocol::parse_generation_params(&input)
            .expect("well-formed")
            .expect("present");
        assert_eq!(params.response_format, Some(ResponseFormat::JsonObject));
    }

    #[test]
    fn infer_args_pack_sampling_penalties_into_the_generation_block() {
        // The flat shape the MCP server, covenantd, and the OpenAI front
        // door all funnel into these shared args.
        let args: InferArgs = serde_json::from_value(serde_json::json!({
            "prompt": "write something new",
            "presence_penalty": 1.5,
            "frequency_penalty": -0.75
        }))
        .unwrap();
        let input = args.input().unwrap();
        let params = covenant_compute_protocol::parse_generation_params(&input)
            .expect("well-formed")
            .expect("present");
        assert_eq!(params.presence_penalty, Some(1.5));
        assert_eq!(params.frequency_penalty, Some(-0.75));
    }

    #[test]
    fn infer_args_reject_an_out_of_range_penalty_before_signing() {
        let args: InferArgs = serde_json::from_value(serde_json::json!({
            "prompt": "hi",
            "presence_penalty": 2.5
        }))
        .unwrap();
        let err = args.input().expect_err("out-of-range penalty is refused");
        assert!(err.contains("presence_penalty"), "got: {err}");
    }

    #[test]
    fn infer_args_refuse_a_forcing_tool_choice_that_offers_no_tools() {
        // A buyer who forces a call (`required` or a named function) with
        // no tools to bind is refused before pricing, not billed a plain
        // completion as if it had forced a call.
        for choice in [
            serde_json::json!("required"),
            serde_json::json!({"type": "function", "function": {"name": "get_weather"}}),
        ] {
            let args: InferArgs = serde_json::from_value(serde_json::json!({
                "prompt": "hi",
                "tool_choice": choice,
            }))
            .unwrap();
            let err = args
                .input()
                .expect_err("a forcing tool_choice with no tools is refused");
            assert!(err.contains("tool_choice"), "got: {err}");
        }
    }

    #[test]
    fn infer_args_drop_a_nonforcing_tool_choice_that_offers_no_tools() {
        // `auto` and `none` force nothing, so with no tools they are a
        // harmless no-op: the job packs no tools block and still runs.
        for choice in [serde_json::json!("auto"), serde_json::json!("none")] {
            let args: InferArgs = serde_json::from_value(serde_json::json!({
                "prompt": "hi",
                "tool_choice": choice,
            }))
            .unwrap();
            let input = args.input().expect("a non-forcing choice drops quietly");
            assert!(
                covenant_compute_protocol::parse_tools_input(&input)
                    .expect("well-formed")
                    .is_none(),
                "no tools block is packed"
            );
        }
    }

    #[test]
    fn infer_args_without_knobs_add_no_generation_block() {
        let args: InferArgs = serde_json::from_value(serde_json::json!({"prompt": "hi"})).unwrap();
        let input = args.input().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(
            covenant_compute_protocol::parse_generation_params(&input).unwrap(),
            None
        );
    }

    #[test]
    fn infer_args_refuse_meaningless_knobs_before_anything_is_signed() {
        let args: InferArgs = serde_json::from_value(serde_json::json!({
            "prompt": "hi",
            "top_p": 1.5
        }))
        .unwrap();
        assert!(args.input().unwrap_err().contains("top_p"));
        let zero_cap: InferArgs = serde_json::from_value(serde_json::json!({
            "prompt": "hi",
            "max_tokens": 0
        }))
        .unwrap();
        assert!(zero_cap.input().unwrap_err().contains("no output"));
    }

    #[test]
    fn run_args_map_a_command_to_one_text_block_and_refuse_blank_ones() {
        let args: RunArgs =
            serde_json::from_value(serde_json::json!({"command": " echo hi "})).unwrap();
        assert_eq!(args.input().unwrap(), vec![Content::text("echo hi")]);
        let blank: RunArgs = serde_json::from_value(serde_json::json!({"command": "  "})).unwrap();
        assert!(blank.input().unwrap_err().contains("empty"));
        assert!(serde_json::from_value::<RunArgs>(serde_json::json!({})).is_err());
    }

    #[test]
    fn embed_args_map_one_text_or_a_batch_in_order() {
        // A single text trims to one block, the pre-batch behaviour.
        let one: EmbedArgs = serde_json::from_value(serde_json::json!({"text": " hi "})).unwrap();
        assert_eq!(one.input().unwrap(), vec![Content::text("hi")]);

        // A batch is one block per text, order preserved.
        let batch: EmbedArgs =
            serde_json::from_value(serde_json::json!({"texts": ["first", "second", "third"]}))
                .unwrap();
        assert_eq!(
            batch.input().unwrap(),
            vec![
                Content::text("first"),
                Content::text("second"),
                Content::text("third"),
            ]
        );
    }

    #[test]
    fn embed_args_refuse_both_neither_blank_and_overlong_batches() {
        let both: EmbedArgs =
            serde_json::from_value(serde_json::json!({"text": "a", "texts": ["b"]})).unwrap();
        assert!(both.input().unwrap_err().contains("not both"));

        let neither: EmbedArgs = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(neither.input().unwrap_err().contains("required"));

        let empty: EmbedArgs = serde_json::from_value(serde_json::json!({"texts": []})).unwrap();
        assert!(empty.input().unwrap_err().contains("empty"));

        let blank: EmbedArgs =
            serde_json::from_value(serde_json::json!({"texts": ["ok", "  "]})).unwrap();
        assert!(blank.input().unwrap_err().contains("non-empty"));

        let over = serde_json::json!({ "texts": vec!["x"; MAX_EMBED_TEXTS + 1] });
        let over: EmbedArgs = serde_json::from_value(over).unwrap();
        assert!(over.input().unwrap_err().contains("per-call limit"));
    }

    #[test]
    fn transcribe_args_pack_the_audio_and_normalize_options() {
        // A plain clip trims to one transcription block, no language or
        // format, translate off.
        let plain: TranscribeArgs =
            serde_json::from_value(serde_json::json!({"audio_base64": " YWJj "})).unwrap();
        assert_eq!(
            plain.input().unwrap(),
            transcription_input(TranscriptionInput::new("YWJj"))
        );

        // A blank or `auto` language and a blank format normalize to
        // detection, the same reading the CLI and OpenAI surfaces take.
        let auto: TranscribeArgs = serde_json::from_value(serde_json::json!({
            "audio_base64": "YWJj",
            "language": "auto",
            "format": "  ",
            "translate": true,
        }))
        .unwrap();
        assert_eq!(
            auto.input().unwrap(),
            transcription_input(TranscriptionInput {
                audio_base64: "YWJj".into(),
                format: None,
                language: None,
                translate: true,
                timestamps: false,
            })
        );

        // An explicit language and format ride through unchanged.
        let en: TranscribeArgs = serde_json::from_value(serde_json::json!({
            "audio_base64": "YWJj",
            "language": "en",
            "format": "wav",
        }))
        .unwrap();
        assert_eq!(
            en.input().unwrap(),
            transcription_input(TranscriptionInput {
                audio_base64: "YWJj".into(),
                format: Some("wav".into()),
                language: Some("en".into()),
                translate: false,
                timestamps: false,
            })
        );

        // Timestamps ride through to the request when asked for.
        let timed: TranscribeArgs = serde_json::from_value(serde_json::json!({
            "audio_base64": "YWJj",
            "timestamps": true,
        }))
        .unwrap();
        assert_eq!(
            timed.input().unwrap(),
            transcription_input(TranscriptionInput {
                audio_base64: "YWJj".into(),
                format: None,
                language: None,
                translate: false,
                timestamps: true,
            })
        );
    }

    #[test]
    fn transcribe_args_refuse_empty_oversized_and_non_base64_clips() {
        let empty: TranscribeArgs =
            serde_json::from_value(serde_json::json!({"audio_base64": "   "})).unwrap();
        assert!(empty.input().unwrap_err().contains("required"));

        let garbage: TranscribeArgs =
            serde_json::from_value(serde_json::json!({"audio_base64": "not base64!"})).unwrap();
        assert!(garbage.input().unwrap_err().contains("valid base64"));

        let over = serde_json::json!({ "audio_base64": "A".repeat(MAX_AUDIO_B64_BYTES + 1) });
        let over: TranscribeArgs = serde_json::from_value(over).unwrap();
        assert!(over.input().unwrap_err().contains("per-call limit"));
    }

    #[test]
    fn transcribe_tool_spec_requires_audio_and_states_the_ceiling() {
        let spec = transcribe_tool_spec(750_000);
        assert_eq!(spec.name, TRANSCRIBE_TOOL);
        assert!(spec.input_schema["properties"]["audio_base64"].is_object());
        assert_eq!(
            spec.input_schema["required"],
            serde_json::json!(["audio_base64"])
        );
        // The live ceiling is stated, not a stale constant, and the
        // language/translate controls are discoverable.
        assert!(spec.description.contains("750000"), "{}", spec.description);
        assert!(spec.input_schema["properties"]["language"].is_object());
        assert!(spec.input_schema["properties"]["translate"].is_object());
        assert!(spec.input_schema["properties"]["timestamps"].is_object());
    }

    #[test]
    fn speak_args_pack_the_text_and_normalize_options() {
        // Plain text packs to one speech block, no voice, format or speed.
        let plain: SpeakArgs =
            serde_json::from_value(serde_json::json!({ "text": "hello there" })).unwrap();
        assert_eq!(
            plain.input().unwrap(),
            speech_input(SpeechInput::new("hello there"))
        );

        // A blank voice or format drops to the operator's default rather
        // than an empty string the backend would have to interpret.
        let blank: SpeakArgs = serde_json::from_value(serde_json::json!({
            "text": "hello",
            "voice": "   ",
            "format": "",
        }))
        .unwrap();
        assert_eq!(
            blank.input().unwrap(),
            speech_input(SpeechInput::new("hello"))
        );

        // An explicit voice, format and speed ride through unchanged.
        let full: SpeakArgs = serde_json::from_value(serde_json::json!({
            "text": "hello",
            "voice": "Alex",
            "format": "aiff",
            "speed": 1.5,
        }))
        .unwrap();
        assert_eq!(
            full.input().unwrap(),
            speech_input(SpeechInput {
                text: "hello".into(),
                voice: Some("Alex".into()),
                format: Some("aiff".into()),
                speed: Some(1.5),
            })
        );
    }

    #[test]
    fn speak_args_refuse_empty_oversized_text_and_a_bad_speed() {
        let empty: SpeakArgs =
            serde_json::from_value(serde_json::json!({ "text": "   " })).unwrap();
        assert!(empty.input().unwrap_err().contains("no text"));

        let over = serde_json::json!({
            "text": "a".repeat(covenant_compute_protocol::MAX_SPEECH_TEXT_CHARS + 1),
        });
        let over: SpeakArgs = serde_json::from_value(over).unwrap();
        assert!(over.input().unwrap_err().contains("over the"));

        let fast: SpeakArgs =
            serde_json::from_value(serde_json::json!({ "text": "hi", "speed": 9.0 })).unwrap();
        assert!(fast.input().unwrap_err().contains("outside"));
    }

    #[test]
    fn speak_tool_spec_requires_text_and_states_the_ceiling() {
        let spec = speak_tool_spec(500_000);
        assert_eq!(spec.name, SPEAK_TOOL);
        assert!(spec.input_schema["properties"]["text"].is_object());
        assert_eq!(spec.input_schema["required"], serde_json::json!(["text"]));
        assert!(spec.description.contains("500000"), "{}", spec.description);
        assert!(spec.input_schema["properties"]["voice"].is_object());
        assert!(spec.input_schema["properties"]["speed"].is_object());
    }

    #[test]
    fn save_speech_clip_writes_the_decoded_audio_named_for_the_job() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"RIFF\0\0\0\0WAVEfmt ";
        let speech = SpeechResult {
            model: "say-1".into(),
            audio_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            format: "wav".into(),
            sample_rate_hz: Some(22_050),
        };
        let job_id = Uuid::new_v4();
        let (path, written) = save_speech_clip(dir.path(), &speech, job_id).unwrap();
        assert_eq!(written, bytes.len());
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            format!("speech-{job_id}.wav")
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);

        // Audio that does not base64-decode is the operator's error,
        // surfaced rather than written to disk as garbage.
        let bad = SpeechResult {
            audio_base64: "not base64!".into(),
            ..speech
        };
        assert!(save_speech_clip(dir.path(), &bad, job_id)
            .unwrap_err()
            .contains("base64-decode"));
    }

    const OPERATOR_WALLET: &str = "9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRSTEA";
    const COORDINATOR_WALLET: &str = "4Nd1mBQtrMJVYVfKf2PJy9NZUZdTAsp7D4xWLs4gDB4T";
    const MINT: &str = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";

    /// A `getTransaction` result in the `jsonParsed` shape the
    /// verifier reads: memo instructions plus pre/post token balances
    /// — the same hand-shaped fixture technique the deposit rail's
    /// tests use for the documented RPC format.
    fn payout_tx(memos: &[&str], transfers: &[(&str, u64, u64)]) -> serde_json::Value {
        let instructions: Vec<serde_json::Value> = memos
            .iter()
            .map(|m| {
                serde_json::json!({
                    "program": "spl-memo",
                    "programId": "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr",
                    "parsed": m,
                })
            })
            .collect();
        let balance_rows = |index: usize| -> Vec<serde_json::Value> {
            transfers
                .iter()
                .map(|(owner, pre, post)| {
                    let amount = if index == 0 { pre } else { post };
                    serde_json::json!({
                        "owner": owner,
                        "mint": MINT,
                        "uiTokenAmount": { "amount": amount.to_string() },
                    })
                })
                .collect()
        };
        serde_json::json!({
            "meta": {
                "err": null,
                "preTokenBalances": balance_rows(0),
                "postTokenBalances": balance_rows(1),
            },
            "transaction": { "message": { "instructions": instructions } },
        })
    }

    #[test]
    fn verify_payout_onchain_accepts_the_transfer_the_memo_names() {
        let (_, receipt, _) = envelope_and_receipt(1_000, &[Content::text("out")]);
        let memo = receipt.payout_memo();
        let tx = payout_tx(
            &["gm", &memo],
            &[
                (COORDINATOR_WALLET, 50_000, 49_000),
                (OPERATOR_WALLET, 0, 1_000),
            ],
        );

        let proof = verify_payout_onchain(&receipt, &tx).expect("verify");
        assert_eq!(
            proof,
            PayoutProof {
                amount_micro_usdc: 1_000,
                mint_b58: MINT.into(),
                recipient_owner_b58: OPERATOR_WALLET.into(),
            },
            "foreign memos are ignored; the payer's own decrease is not a payout"
        );
    }

    #[test]
    fn verify_payout_onchain_rejects_a_memo_for_different_work() {
        let (_, receipt, _) = envelope_and_receipt(1_000, &[Content::text("out")]);
        let (_, other, _) = envelope_and_receipt(1_000, &[Content::text("out")]);
        let tx = payout_tx(&[&other.payout_memo()], &[(OPERATOR_WALLET, 0, 1_000)]);

        let err = verify_payout_onchain(&receipt, &tx).unwrap_err();
        assert!(
            err.to_string()
                .contains(&format!("job {}", other.receipt.job_id)),
            "the error names whose payout this actually is: {err}"
        );
    }

    #[test]
    fn verify_payout_onchain_requires_exactly_one_compute_memo() {
        let (_, receipt, _) = envelope_and_receipt(1_000, &[Content::text("out")]);
        let memo = receipt.payout_memo();

        let none = payout_tx(&["gm"], &[(OPERATOR_WALLET, 0, 1_000)]);
        assert!(verify_payout_onchain(&receipt, &none)
            .unwrap_err()
            .to_string()
            .contains("found 0"));

        let two = payout_tx(&[&memo, &memo], &[(OPERATOR_WALLET, 0, 1_000)]);
        assert!(verify_payout_onchain(&receipt, &two)
            .unwrap_err()
            .to_string()
            .contains("found 2"));
    }

    #[test]
    fn verify_payout_onchain_rejects_a_failed_transaction() {
        let (_, receipt, _) = envelope_and_receipt(1_000, &[Content::text("out")]);
        let mut tx = payout_tx(&[&receipt.payout_memo()], &[(OPERATOR_WALLET, 0, 1_000)]);
        tx["meta"]["err"] = serde_json::json!({"InstructionError": [1, "Custom"]});

        assert!(verify_payout_onchain(&receipt, &tx)
            .unwrap_err()
            .to_string()
            .contains("failed on-chain"));
    }

    #[test]
    fn verify_payout_onchain_rejects_a_transfer_that_moved_nothing() {
        let (_, receipt, _) = envelope_and_receipt(1_000, &[Content::text("out")]);
        let tx = payout_tx(&[&receipt.payout_memo()], &[(OPERATOR_WALLET, 500, 500)]);

        assert!(verify_payout_onchain(&receipt, &tx)
            .unwrap_err()
            .to_string()
            .contains("moved nothing"));
    }

    #[test]
    fn verify_payout_onchain_rejects_an_ambiguous_recipient() {
        let (_, receipt, _) = envelope_and_receipt(1_000, &[Content::text("out")]);
        let tx = payout_tx(
            &[&receipt.payout_memo()],
            &[(OPERATOR_WALLET, 0, 600), (COORDINATOR_WALLET, 0, 400)],
        );

        assert!(verify_payout_onchain(&receipt, &tx)
            .unwrap_err()
            .to_string()
            .contains("ambiguous"));
    }

    #[test]
    fn verify_payout_onchain_reports_a_missing_transaction() {
        let (_, receipt, _) = envelope_and_receipt(1_000, &[Content::text("out")]);
        assert!(verify_payout_onchain(&receipt, &serde_json::Value::Null)
            .unwrap_err()
            .to_string()
            .contains("not found"));
    }

    /// Live (manual): `cargo test -p covenant-compute-buyer --locked
    /// -- --ignored`. Proves the fetch half against the real devnet
    /// RPC — transport, the not-found path for a well-formed signature
    /// that isn't on chain, and that a real foreign transaction (the
    /// newest one touching the devnet USDC mint) is rejected for
    /// carrying no compute memo, on the RPC's own jsonParsed shape
    /// rather than our fixtures. Credential-free: reads only.
    #[tokio::test]
    #[ignore = "hits api.devnet.solana.com; run manually"]
    async fn fetch_payout_transaction_live_devnet_paths() {
        const RPC: &str = "https://api.devnet.solana.com";
        let http = reqwest::Client::new();
        let (_, receipt, _) = envelope_and_receipt(1_000, &[Content::text("out")]);

        // A valid-shape 64-byte signature that was never submitted:
        // any ed25519 signature encodes to the right width.
        let ghost = &receipt.signature_b58;
        let tx = fetch_payout_transaction(&http, RPC, ghost)
            .await
            .expect("rpc must answer for an unknown signature");
        assert!(tx.is_null(), "an unsubmitted signature must come back null");
        assert!(verify_payout_onchain(&receipt, &tx)
            .unwrap_err()
            .to_string()
            .contains("not found"));

        // The newest real transaction touching the devnet USDC mint —
        // whatever it is, it isn't a payout for this receipt.
        let sigs: serde_json::Value = http
            .post(RPC)
            .json(&serde_json::json!({
                "jsonrpc": "2.0", "id": 1,
                "method": "getSignaturesForAddress",
                "params": [MINT, {"limit": 1}],
            }))
            .send()
            .await
            .expect("getSignaturesForAddress transport")
            .json()
            .await
            .expect("getSignaturesForAddress decode");
        let real_sig = sigs
            .pointer("/result/0/signature")
            .and_then(serde_json::Value::as_str)
            .expect("devnet USDC mint has at least one transaction")
            .to_string();
        let tx = fetch_payout_transaction(&http, RPC, &real_sig)
            .await
            .expect("fetch a real devnet transaction");
        assert!(!tx.is_null(), "a listed signature must resolve");
        let err = verify_payout_onchain(&receipt, &tx).unwrap_err();
        assert!(
            err.to_string().contains("found 0"),
            "a foreign transaction fails on the missing memo, got: {err}"
        );
    }

    #[tokio::test]
    async fn a_failed_payout_rpc_reads_as_rpc_not_coordinator() {
        // The payout-verification endpoint is the buyer's own Solana RPC,
        // not the coordinator; a failure there must point at the RPC so a
        // buyer with a wrong rpc_url does not go debugging a coordinator
        // that is fine.
        let http = reqwest::Client::new();
        let err = fetch_payout_transaction(&http, "http://127.0.0.1:1/", "sig")
            .await
            .unwrap_err();
        assert!(matches!(err, BuyerError::Rpc(_)), "got: {err:?}");
        let msg = err.to_string();
        assert!(msg.starts_with("solana rpc:"), "names the rpc: {msg}");
        assert!(!msg.contains("coordinator"), "not the coordinator: {msg}");
    }

    #[test]
    fn only_provably_unpaid_conclusions_free_an_idempotency_key() {
        let refused = |status| BuyerError::SubmitRefused {
            status,
            body: "refused".into(),
        };
        // A verdict on the envelope repeats forever; the key must free.
        assert!(refused(400).concludes_purchase_unpaid());
        assert!(refused(404).concludes_purchase_unpaid());
        // Funding-state refusals heal (top-up, subsidy refill) and the
        // same envelope then succeeds — the key stays bound.
        assert!(!refused(402).concludes_purchase_unpaid());
        assert!(!refused(409).concludes_purchase_unpaid());
        // A job that concluded unpaid moved no money either.
        assert!(BuyerError::NotServed {
            job_id: Uuid::new_v4(),
            status: "refunded".into(),
            reason: None,
            detail: None,
        }
        .concludes_purchase_unpaid());
        // Anything ambiguous leaves the entry in flight.
        assert!(!BuyerError::ReceiptTimeout(Uuid::new_v4()).concludes_purchase_unpaid());
        assert!(
            !BuyerError::Coordinator("submit: connection refused".into())
                .concludes_purchase_unpaid()
        );
    }

    #[test]
    fn not_served_names_the_refund_reason_when_the_coordinator_gave_one() {
        let job_id = Uuid::new_v4();
        let with_reason = BuyerError::NotServed {
            job_id,
            status: "refunded".into(),
            reason: Some("buyer_cancelled".into()),
            detail: None,
        };
        assert_eq!(
            with_reason.to_string(),
            format!("job {job_id} was not served: refunded (buyer_cancelled)")
        );
        let without = BuyerError::NotServed {
            job_id,
            status: "refunded".into(),
            reason: None,
            detail: None,
        };
        assert_eq!(
            without.to_string(),
            format!("job {job_id} was not served: refunded"),
            "a coordinator that predates the field costs the message nothing"
        );
        // A failed job names the operator's own signed cause in place of
        // the coarse refund reason, so a buyer reads "why", not just
        // "what".
        let with_detail = BuyerError::NotServed {
            job_id,
            status: "failed".into(),
            reason: Some("execution_failed".into()),
            detail: Some("the model rejected the prompt: context length exceeded".into()),
        };
        assert_eq!(
            with_detail.to_string(),
            format!(
                "job {job_id} was not served: failed: the model rejected the prompt: context \
                 length exceeded"
            )
        );
    }

    #[test]
    fn a_receipt_poll_without_a_refund_reason_still_parses() {
        let old: JobStatusResponse =
            serde_json::from_str(r#"{"status":"refunded","receipt":null}"#).unwrap();
        assert_eq!(old.status, "refunded");
        assert_eq!(old.refund_reason, None);
    }
}
