//! Operator node binary — the supply side of the Covenant compute
//! network as a runnable process: load (or mint) a persistent operator
//! identity, register the declared capability profile with the
//! coordinator, heartbeat, and serve jobs until stopped.
//!
//! `$COVENANT_COMPUTE_NODE_HOME` (default `$HOME/.covenant-compute-node`)
//! holds the identity key, the node's own hash-chained audit log, and
//! `earnings.jsonl` — the file-backed earnings ledger (a restart must
//! not zero the operator's earnings record; `covenant-compute-node
//! earnings` prints it). The identity is the operator's earnings
//! identity — every receipt is signed with it and reputation accrues
//! against its pubkey — so it is persisted, never regenerated per
//! boot. The audit log is file-backed for the same reason: its root
//! hash is embedded in every signed receipt, and a chain that survives
//! restarts is the operator's own accountability record.
//!
//! The three trust/money anchors — coordinator URL, the coordinator's
//! pinned pubkey, and the payout address — have no defaults and refuse
//! to boot when missing. Everything else defaults sanely and logs what
//! it chose. `covenant-compute-node setup` collects and validates all
//! three interactively (or via flags), probes the model backend, and
//! writes them to `node.env` in the node home, which every boot loads
//! (already-set environment variables win).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use base64::Engine as _;
use covenant_audit::{AuditEvent, AuditKind, AuditLog, JsonlAuditLog};
use covenant_compute_node::{
    audit_paid_entry, ollama, openai_compat, reconcile_paid_rows, run_benchmark, BenchmarkSpec,
    BrokerConfig, BrokerSessionBackend, ChunkSink, ContainerConfig, ContainerJobExecutor,
    Coordinator, EarningsLedger, EarningsStatus, EchoExecutor, ExecutionOutcome, ExecutorError,
    HttpCoordinatorClient, JobExecutor, JsonlEarningsLedger, LeaseControl, LeaseExecutor, Node,
    NodeConfig, NodeError, OllamaExecutor, OpenAiCompatExecutor, SayExecutor, StubSessionBackend,
    SubprocessJobExecutor, WhisperExecutor, DEFAULT_SAY_BIN, DEFAULT_WHISPER_BIN,
    READY_POLL_INTERVAL, READY_TIMEOUT, SERVICE_USAGE, SETUP_USAGE,
};
use covenant_compute_protocol::{
    canonical_model, coordinator_reason, payout_transaction_rpc_request, speech_input,
    transcription_input, CapabilityProfile, CapacityView, HardwareClass, HeartbeatRequest,
    JobEnvelopePayload, JobKind, PriceAsk, PriceUnit, RegisterRequest, SpeechInput,
    TranscriptionInput,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use covenant_runtime::SubprocessTracker;
use tracing_subscriber::EnvFilter;

const NODE_USAGE: &str = "\
covenant-compute-node — operator node for the Covenant compute network

Usage:
  covenant-compute-node                    serve jobs (register, long-poll, execute, earn)
  covenant-compute-node setup [flags]      one-time configuration wizard -> node.env
  covenant-compute-node service <action>   run across reboots: install | uninstall | status
  covenant-compute-node status             standing, floors, the market, earnings at a glance
  covenant-compute-node earnings           the credited-jobs ledger
  covenant-compute-node earnings verify    hold every paid row to the chain's own record
  covenant-compute-node earnings verify --job <id>   check just one job's payout
  covenant-compute-node bond               this node's stake: posted, slashed, unbonding
  covenant-compute-node bond claim <tx-signature>
  covenant-compute-node bond unbond <amount-micro-usdc> <recipient>
  covenant-compute-node --version          print the version

Add --json to status, earnings or bond for the underlying record instead
of the summary — the shape a monitor or a scheduled check reads.

Serving needs three trust anchors, from the environment or from the
node.env that `setup` writes: COVENANT_COMPUTE_COORDINATOR_URL,
COVENANT_COMPUTE_COORDINATOR_PUBKEY, COVENANT_COMPUTE_PAYOUT_ADDRESS.
Everything else defaults sanely; the full knob table is in the crate
README.
";

fn node_home() -> anyhow::Result<PathBuf> {
    if let Ok(p) = std::env::var("COVENANT_COMPUTE_NODE_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".covenant-compute-node"))
}

fn env_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    match std::env::var(key) {
        Ok(v) => v.trim().parse().unwrap_or_else(|_| {
            tracing::warn!(key, value = %v, "unparseable value; using the default");
            default
        }),
        Err(_) => default,
    }
}

/// `--flag` present anywhere in the args, removed in place.
fn take_flag(args: &mut Vec<String>, name: &str) -> bool {
    if let Some(i) = args.iter().position(|a| a == name) {
        args.remove(i);
        return true;
    }
    false
}

/// Flatten control characters to spaces so a string that rode in from
/// the coordinator's books can't forge status lines of its own once it
/// lands in the terminal.
fn flatten_controls(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn required(key: &str) -> anyhow::Result<String> {
    let value = std::env::var(key)
        .map(|v| v.trim().to_string())
        .unwrap_or_default();
    anyhow::ensure!(
        !value.is_empty(),
        "{key} is unset; run `covenant-compute-node setup` to configure the trust anchors, or \
         set it in the environment (it has no safe default)"
    );
    Ok(value)
}

/// The advertised hardware class and VRAM. `COVENANT_COMPUTE_NODE_HARDWARE`
/// pins the class: set, it wins and boot skips the probe. Unset, boot probes
/// for a GPU (`nvidia-smi`, then Apple Silicon) so a real GPU node advertises
/// itself out of the box instead of registering as CPU and never matching a
/// GPU job; no GPU found means cpu. `COVENANT_COMPUTE_NODE_VRAM_GB` is an
/// independent override of the advertised width — applied over the set or the
/// detected class alike, so a lone VRAM_GB no longer suppresses detection and
/// leaves a GPU box registering CPU-only.
async fn resolve_hardware() -> (HardwareClass, u32) {
    let hardware_env = std::env::var("COVENANT_COMPUTE_NODE_HARDWARE").ok();
    let vram_override = std::env::var_os("COVENANT_COMPUTE_NODE_VRAM_GB")
        .map(|_| env_or("COVENANT_COMPUTE_NODE_VRAM_GB", 0u32));
    let detected = if hardware_env.is_some() {
        None
    } else {
        covenant_compute_node::detect_gpu().await
    };
    if let Some(gpu) = &detected {
        tracing::info!(
            model = %gpu.model,
            vram_gb = vram_override.unwrap_or(gpu.vram_gb),
            "detected a GPU; advertising it — set COVENANT_COMPUTE_NODE_HARDWARE to override"
        );
    }
    resolve_hardware_decision(
        hardware_env.as_deref(),
        vram_override,
        detected.map(|g| (g.hardware_class(), g.vram_gb)),
    )
}

/// The pure precedence behind [`resolve_hardware`], split out so the
/// override rules are testable without mutating process env or shelling out
/// to a GPU probe. A set hardware class is authoritative and skips
/// detection; otherwise the detected class (or CPU-only) stands. An explicit
/// VRAM figure overrides the width in every case.
fn resolve_hardware_decision(
    hardware_env: Option<&str>,
    vram_override: Option<u32>,
    detected: Option<(HardwareClass, u32)>,
) -> (HardwareClass, u32) {
    if let Some(raw) = hardware_env {
        return (
            covenant_compute_node::parse_hardware(raw),
            vram_override.unwrap_or(0),
        );
    }
    match detected {
        Some((hardware, vram_gb)) => (hardware, vram_override.unwrap_or(vram_gb)),
        None => (HardwareClass::CpuOnly, vram_override.unwrap_or(0)),
    }
}

/// One-line description of the hardware a node advertises, for the
/// `status` read — the runtime echo of what `setup` reported.
fn describe_hardware(hardware: &HardwareClass, vram_gb: u32) -> String {
    match hardware {
        HardwareClass::ConsumerGpu { model } | HardwareClass::DatacenterGpu { model } => {
            format!("{model} ({vram_gb} GB VRAM)")
        }
        HardwareClass::CpuOnly => "CPU-only".to_string(),
    }
}

/// A container node that advertises a GPU but passes no device into the
/// container would win GPU jobs and run them CPU-only — an advertised
/// capability it can't deliver. True when that mismatch is configured.
fn gpu_advertised_but_not_passed(hardware: &HardwareClass, container_gpus: Option<&str>) -> bool {
    container_gpus.is_none()
        && matches!(
            hardware,
            HardwareClass::ConsumerGpu { .. } | HardwareClass::DatacenterGpu { .. }
        )
}

fn parse_job_kinds(raw: &str) -> Vec<JobKind> {
    let kinds: Vec<JobKind> = raw
        .split(',')
        .filter_map(|k| match k.trim() {
            "inference_call" => Some(JobKind::InferenceCall),
            "batch_job" => Some(JobKind::BatchJob),
            "lease_session" => Some(JobKind::LeaseSession),
            "embedding" => Some(JobKind::Embedding),
            "transcription" => Some(JobKind::Transcription),
            "speech_synthesis" => Some(JobKind::SpeechSynthesis),
            other => {
                if !other.is_empty() {
                    tracing::warn!(kind = other, "unknown job kind; skipping");
                }
                None
            }
        })
        .collect();
    if kinds.is_empty() {
        vec![JobKind::BatchJob]
    } else {
        kinds
    }
}

fn parse_price_unit(raw: &str) -> PriceUnit {
    match raw.trim() {
        "per_million_tokens" => PriceUnit::PerMillionTokens,
        "per_gpu_second" => PriceUnit::PerGpuSecond,
        "per_lease_hour" => PriceUnit::PerLeaseHour,
        "per_job" | "" => PriceUnit::PerJob,
        other => {
            // A mistyped unit is a silent mispricing otherwise: the node
            // would bill per_job while the operator believes they set a
            // per-token or per-second rate. Warn and fall back, the same
            // posture parse_job_kinds takes.
            tracing::warn!(unit = other, "unknown price unit; using per_job");
            PriceUnit::PerJob
        }
    }
}

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

/// The job kinds an executor can actually serve. A node must never
/// advertise a capability its backend would mis-execute: a model backend
/// handed a batch command sends it to the model as a prompt, a subprocess
/// backend handed an inference prompt runs it as a shell command, and a
/// model backend handed a lease session has no machine to rent — each
/// produces garbage a matched buyer still pays for. The `broker` backend
/// is the one that serves a lease: it rents a real machine per session.
/// `None` is `echo`, which echoes any input and exists only for smoke
/// tests; an unknown executor is rejected where the executor itself is
/// built.
fn servable_kinds(executor_kind: &str) -> Option<&'static [JobKind]> {
    match executor_kind {
        "ollama" | "openai-compat" => Some(&[JobKind::InferenceCall, JobKind::Embedding]),
        "subprocess" | "container" => Some(&[JobKind::BatchJob]),
        "whisper" => Some(&[JobKind::Transcription]),
        "say" => Some(&[JobKind::SpeechSynthesis]),
        "broker" | "lease-stub" => Some(&[JobKind::LeaseSession]),
        _ => None,
    }
}

/// Whether `executor_kind` loads one fixed model and so must advertise
/// exactly one model id. The whisper and say backends each load a single
/// local model file and stamp `models_served.first()` on every job,
/// ignoring the requested `model_id`, so a second advertised name is a
/// model the node cannot actually serve. The model backends (ollama,
/// openai-compat) route each job to any of their advertised models, and
/// the generic backends serve `any`, so several names is honest there.
fn serves_single_model(executor_kind: &str) -> bool {
    matches!(executor_kind, "whisper" | "say")
}

/// The job kinds advertised when `COVENANT_COMPUTE_NODE_JOB_KINDS` is
/// unset: inference for a model backend, transcription for a whisper
/// backend, a lease session for a broker, batch for everything else.
/// Always within the executor's [`servable_kinds`], so the default never
/// trips the boot coherence check.
fn default_kinds(executor_kind: &str) -> &'static str {
    match executor_kind {
        "ollama" | "openai-compat" => "inference_call",
        "whisper" => "transcription",
        "say" => "speech_synthesis",
        "broker" | "lease-stub" => "lease_session",
        _ => "batch_job",
    }
}

/// A short clip of synthetic speech saying "covenant compute", bundled
/// so a whisper node's benchmark-on-register has a self-contained
/// known-answer probe: the words are fixed, so a real transcript of it
/// proves the backend transcribes without reaching for any external
/// fixture. 16 kHz mono WAV, the form whisper.cpp decodes natively.
const WHISPER_BENCHMARK_WAV: &[u8] = include_bytes!("whisper_benchmark.wav");

/// The known-answer self-tests the node runs against its real executor
/// before it registers, one per capability it must prove. A deterministic
/// backend (echo, subprocess/container) round-trips a nonce; a model
/// backend generates a tiny completion to prove an inference claim and
/// embeds a short text to prove an embedding claim.
///
/// ollama answers `/api/embed` with the same model it chats with, so a
/// node serving both there proves the embed path with its inference probe.
/// An arbitrary openai-compat server can chat yet reject `/v1/embeddings`
/// (a chat-only vLLM or llama.cpp backend does), so a node declaring both
/// against openai-compat proves each claim on its own probe rather than
/// assuming the pairing — otherwise it registers an embedding claim it was
/// never asked to demonstrate, and the first embedding buyer eats a
/// refund-churn while the honest operator takes a reputation fault the
/// benchmark exists to prevent.
fn benchmark_specs(
    executor_kind: &str,
    job_kinds: &[JobKind],
    nonce: String,
) -> Vec<BenchmarkSpec> {
    let declared_kind = job_kinds.first().copied().unwrap_or(JobKind::BatchJob);
    let serves = |kind| job_kinds.contains(&kind);
    match executor_kind {
        "echo" => vec![BenchmarkSpec {
            input: vec![Content::text(nonce.clone())],
            expect_contains: Some(nonce),
            per_model: false,
            kind: declared_kind,
        }],
        "ollama" | "openai-compat" => {
            let inference = || BenchmarkSpec {
                input: vec![Content::text("Reply with one short word.")],
                expect_contains: None,
                per_model: true,
                kind: JobKind::InferenceCall,
            };
            let embedding = || BenchmarkSpec {
                input: vec![Content::text("compute embedding benchmark")],
                expect_contains: None,
                per_model: true,
                kind: JobKind::Embedding,
            };
            let mut specs = Vec::new();
            if serves(JobKind::InferenceCall) {
                specs.push(inference());
            }
            let embed_covered_by_inference =
                executor_kind == "ollama" && serves(JobKind::InferenceCall);
            if serves(JobKind::Embedding) && !embed_covered_by_inference {
                specs.push(embedding());
            }
            // A model backend always declares at least one servable kind
            // (`servable_kinds` refuses boot otherwise); fall back to the
            // generative probe if a caller ever passes none.
            if specs.is_empty() {
                specs.push(inference());
            }
            specs
        }
        // A whisper node proves it can transcribe by running a short
        // synthetic-speech clip (bundled, ~40 KB) through the real CLI and
        // checking the transcript carries the words the clip speaks. Judged
        // case-insensitively — a speech model chooses its own casing.
        "whisper" => vec![BenchmarkSpec {
            input: transcription_input(TranscriptionInput::new(
                base64::engine::general_purpose::STANDARD.encode(WHISPER_BENCHMARK_WAV),
            )),
            expect_contains: Some("covenant compute".into()),
            per_model: false,
            kind: JobKind::Transcription,
        }],
        // A speech node proves it can synthesize by voicing a short line
        // through the real backend and checking a non-empty clip comes
        // back — there is no text to match, so the audio itself is the
        // known answer.
        "say" => vec![BenchmarkSpec {
            input: speech_input(SpeechInput::new("covenant compute")),
            expect_contains: None,
            per_model: false,
            kind: JobKind::SpeechSynthesis,
        }],
        _ => vec![BenchmarkSpec {
            input: vec![Content::text(format!("printf %s {nonce}"))],
            expect_contains: Some(nonce),
            per_model: false,
            kind: declared_kind,
        }],
    }
}

fn unit_label(unit: PriceUnit) -> &'static str {
    match unit {
        PriceUnit::PerMillionTokens => "per_million_tokens",
        PriceUnit::PerGpuSecond => "per_gpu_second",
        PriceUnit::PerLeaseHour => "per_lease_hour",
        PriceUnit::PerJob => "per_job",
    }
}

/// Parses an explicit `COVENANT_COMPUTE_NODE_MODELS` value into the
/// declared model list, refusing a set-but-empty value (blank, or only
/// separators). An empty list leaves a model-serving executor with
/// nothing to benchmark, so it would register model claims it never
/// demonstrated.
fn parse_explicit_models(list: &str) -> anyhow::Result<Vec<String>> {
    let models: Vec<String> = list
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    anyhow::ensure!(
        !models.is_empty(),
        "COVENANT_COMPUTE_NODE_MODELS is set but names no model; list at least one model, \
         or unset the variable"
    );
    Ok(models)
}

/// The models this node declares. An explicit
/// `COVENANT_COMPUTE_NODE_MODELS` wins; a model-serving executor
/// otherwise asks its backend (ollama's `/api/tags`, the
/// OpenAI-compatible `/models`) so the declared profile is honest by
/// construction rather than a hand-typed claim; anything else is a
/// generic exec node serving `any`.
async fn resolve_models_served(executor_kind: &str) -> anyhow::Result<Vec<String>> {
    match std::env::var("COVENANT_COMPUTE_NODE_MODELS") {
        Ok(list) => parse_explicit_models(&list),
        Err(_) if executor_kind == "ollama" => {
            let ollama_url = std::env::var("COVENANT_COMPUTE_OLLAMA_URL")
                .unwrap_or_else(|_| ollama::DEFAULT_OLLAMA_URL.into());
            let models = ollama::list_models(&ollama_url).await.map_err(|e| {
                anyhow::anyhow!(
                    "cannot discover models from ollama at {ollama_url}: {e} — an ollama node \
                     must not register model claims it can't check"
                )
            })?;
            anyhow::ensure!(
                !models.is_empty(),
                "ollama at {ollama_url} serves no models; pull one first (e.g. `ollama pull qwen2.5:7b`)"
            );
            Ok(models)
        }
        Err(_) if executor_kind == "openai-compat" => {
            let openai_url = std::env::var("COVENANT_COMPUTE_OPENAI_URL")
                .unwrap_or_else(|_| openai_compat::DEFAULT_OPENAI_COMPAT_URL.into());
            let openai_api_key = std::env::var("COVENANT_COMPUTE_OPENAI_API_KEY")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty());
            let models = openai_compat::list_models(&openai_url, openai_api_key.as_deref())
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "cannot discover models from the backend at {openai_url}: {e} — a \
                         model-serving node must not register claims it can't check"
                    )
                })?;
            anyhow::ensure!(
                !models.is_empty(),
                "the backend at {openai_url} serves no models; load one first"
            );
            Ok(models)
        }
        // A whisper node serves exactly one local model file, and its
        // advertised name is decoupled from that file so a buyer asks for a
        // stable id rather than an operator's path. Default to `whisper-1`,
        // the id the OpenAI-compatible transcription surface uses; an
        // operator naming the model precisely still wins above.
        Err(_) if executor_kind == "whisper" => Ok(vec!["whisper-1".into()]),
        // A say node serves one local synthesizer and advertises a stable
        // id decoupled from the OS tool, exactly as a whisper node does.
        Err(_) if executor_kind == "say" => Ok(vec!["say-1".into()]),
        Err(_) => Ok(vec!["any".into()]),
    }
}

/// Runtime-selected executor behind one type, so `Node`'s generic
/// parameter stays a concrete `Sized` type. `subprocess` (default) runs
/// the job's first text block under `sh -c`, tracked and
/// hard-preempted at deadline; `container` runs the same job inside an
/// OCI container (default-deny egress, read-only rootfs, dropped
/// capabilities — B6's isolation posture); `ollama` serves real
/// inference against a local Ollama server with real token metering;
/// `openai-compat` serves inference against anything speaking the
/// OpenAI chat-completions API (vLLM, llama.cpp, LM Studio, hosted);
/// `broker` owns no hardware and rents a real GPU per lease session from
/// a cloud market; `echo` returns the input verbatim, and `lease-stub`
/// holds a lease open behind a placeholder endpoint that rents no machine
/// — the two hermetic smoke-test backends.
enum NodeExecutor {
    Echo(EchoExecutor),
    Subprocess(SubprocessJobExecutor),
    Container(ContainerJobExecutor),
    Ollama(OllamaExecutor),
    OpenAiCompat(OpenAiCompatExecutor),
    Whisper(WhisperExecutor),
    Say(SayExecutor),
    Lease(LeaseExecutor),
}

#[async_trait]
impl JobExecutor for NodeExecutor {
    async fn execute(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        match self {
            NodeExecutor::Echo(e) => e.execute(job, deadline).await,
            NodeExecutor::Subprocess(e) => e.execute(job, deadline).await,
            NodeExecutor::Container(e) => e.execute(job, deadline).await,
            NodeExecutor::Ollama(e) => e.execute(job, deadline).await,
            NodeExecutor::OpenAiCompat(e) => e.execute(job, deadline).await,
            NodeExecutor::Whisper(e) => e.execute(job, deadline).await,
            NodeExecutor::Say(e) => e.execute(job, deadline).await,
            NodeExecutor::Lease(e) => e.execute(job, deadline).await,
        }
    }

    // Forwarded explicitly: leaning on the trait default here would
    // silently strip every backend's streaming override behind this
    // enum, and the job would degrade to one-shot in the real binary
    // while unit tests on the concrete executors stay green.
    async fn execute_streaming(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
        sink: ChunkSink,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        match self {
            NodeExecutor::Echo(e) => e.execute_streaming(job, deadline, sink).await,
            NodeExecutor::Subprocess(e) => e.execute_streaming(job, deadline, sink).await,
            NodeExecutor::Container(e) => e.execute_streaming(job, deadline, sink).await,
            NodeExecutor::Ollama(e) => e.execute_streaming(job, deadline, sink).await,
            NodeExecutor::OpenAiCompat(e) => e.execute_streaming(job, deadline, sink).await,
            NodeExecutor::Whisper(e) => e.execute_streaming(job, deadline, sink).await,
            NodeExecutor::Say(e) => e.execute_streaming(job, deadline, sink).await,
            NodeExecutor::Lease(e) => e.execute_streaming(job, deadline, sink).await,
        }
    }

    // Same rule as streaming: the trait default answers always-healthy,
    // which behind this enum would declare a dead Ollama fit to serve.
    async fn health(&self) -> Result<(), ExecutorError> {
        match self {
            NodeExecutor::Echo(e) => e.health().await,
            NodeExecutor::Subprocess(e) => e.health().await,
            NodeExecutor::Container(e) => e.health().await,
            NodeExecutor::Ollama(e) => e.health().await,
            NodeExecutor::OpenAiCompat(e) => e.health().await,
            NodeExecutor::Whisper(e) => e.health().await,
            NodeExecutor::Say(e) => e.health().await,
            NodeExecutor::Lease(e) => e.health().await,
        }
    }
}

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Registers with capped-backoff retry until the coordinator accepts.
/// A home operator's node booting before its network (or the
/// coordinator) is up should keep trying, not crash-loop.
///
/// Returns the marketplace fee the coordinator disclosed. When
/// `max_fee_bps` is set, a disclosed fee above it refuses the
/// registration outright — the operator's stated tolerance, not a
/// retryable transport hiccup. Unset means disclose-and-continue: the
/// fee is logged loudly, never hidden.
async fn register_until_accepted(
    client: &HttpCoordinatorClient,
    profile: &CapabilityProfile,
    payout_address: &str,
    referral_code: Option<&str>,
    identity: &LocalIdentity,
    max_fee_bps: Option<u32>,
) -> anyhow::Result<u32> {
    let mut backoff = Duration::from_secs(1);
    loop {
        let req = RegisterRequest::sign_referred(
            profile.clone(),
            payout_address.to_string(),
            referral_code.map(str::to_string),
            identity,
        )
        .context("sign register request")?;
        match client.register(req).await {
            Ok(resp) if resp.accepted => {
                // A fee at or above 100% pays the operator zero or less for
                // its work — confiscation, not a disclosed fee, and no retry
                // fixes it. Refuse to register rather than serve under it and
                // later underflow the net-of-fee earnings credit. Mirrors the
                // coordinator's own MarketplaceFee bound on the value it
                // discloses; the node pins the coordinator key for
                // authenticity but never trusted the fee to be sane.
                covenant_compute_protocol::MarketplaceFee::new(resp.fee_bps).map_err(|e| {
                    anyhow::anyhow!("coordinator disclosed an unusable marketplace fee: {e}")
                })?;
                if let Some(max) = max_fee_bps {
                    anyhow::ensure!(
                        resp.fee_bps <= max,
                        "coordinator takes a {} bps marketplace fee; this node accepts at most \
                         {max} bps — refusing to serve under undisclosed-worse terms",
                        resp.fee_bps
                    );
                }
                if resp.fee_bps > 0 {
                    tracing::warn!(
                        fee_bps = resp.fee_bps,
                        "coordinator discloses a marketplace fee — earnings credit net of it \
                         (set COVENANT_COMPUTE_NODE_MAX_FEE_BPS to bound what you accept)"
                    );
                } else {
                    tracing::info!("registered with coordinator (no marketplace fee)");
                }
                return Ok(resp.fee_bps);
            }
            Ok(resp) => {
                // An explicit rejection is a config/policy problem a
                // retry loop can't fix — surface it and stop.
                anyhow::bail!(
                    "coordinator rejected registration: {}",
                    resp.reason.unwrap_or_else(|| "no reason given".into())
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, retry_in = ?backoff, "registration failed; retrying");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
    }
}

/// `covenant-compute-node earnings` — print the operator's earnings
/// record from the file-backed ledger and exit. Works without a
/// running coordinator or any env beyond the node home. Paid means the
/// coordinator confirmed the payout push for that job (reconciled by
/// the serving loop); unpaid is money the books still owe.
fn coordinator_url_from_env() -> anyhow::Result<String> {
    std::env::var("COVENANT_COMPUTE_COORDINATOR_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("COVENANT_COMPUTE_COORDINATOR_URL is not set — run `setup` first")
        })
}

/// The client every coordinator-facing CLI command holds: bounded
/// timeouts, and this build's wire version on every request so a
/// coordinator with a raised floor refuses by name ("upgrade this
/// binary") instead of failing a parse.
fn coordinator_http() -> anyhow::Result<reqwest::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::HeaderName::from_static(
            covenant_compute_protocol::PROTOCOL_VERSION_HEADER,
        ),
        reqwest::header::HeaderValue::from(covenant_compute_protocol::PROTOCOL_VERSION),
    );
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .default_headers(headers)
        .build()
        .context("build http client")
}

/// A coordinator transport failure as one operator-facing line, not a
/// reqwest cause chain: a node's stake and standing live on the
/// coordinator, so the `bond` commands can't answer while it is down.
/// Only for a connection that never completed — a landed 4xx/5xx keeps
/// its own status handling.
/// Why a coordinator request never got a response, as a verb phrase that
/// reads after the coordinator's URL ("… refused the connection").
fn coordinator_unreachable_reason(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "timed out"
    } else if e.is_connect() {
        "refused the connection"
    } else {
        "could not be reached"
    }
}

fn coordinator_unreachable(url: &str, doing: &str, e: &reqwest::Error) -> anyhow::Error {
    anyhow::anyhow!(
        "coordinator at {url} {} while trying to {doing} — it may be down, or \
         COVENANT_COMPUTE_COORDINATOR_URL may be wrong",
        coordinator_unreachable_reason(e)
    )
}

/// `bond`: this operator's whole stake picture off the coordinator —
/// Renders a micro-USDC figure the way every operator read does: the
/// raw amount plus its dollar value, so a stake or earnings line always
/// carries its unit.
fn usd_micro(micro: u64) -> String {
    format!("{micro} micro-USDC (${:.6})", micro as f64 / 1e6)
}

/// posted, slashed, unbonding, refunded — plus posting instructions
/// while nothing is posted. A signed read: only this node's key can
/// see its own feed.
async fn print_bond(home: &std::path::Path, json_out: bool) -> anyhow::Result<()> {
    apply_node_env_defaults(home)?;
    let coordinator_url = coordinator_url_from_env()?;
    let identity = LocalIdentity::load_or_create(&home.join("identity.json"), "operator@compute")
        .context("load operator identity")?;
    let operator_key = bs58::encode(identity.pubkey_bytes()).into_string();
    let http = coordinator_http()?;

    let info: serde_json::Value = http
        .get(format!("{coordinator_url}/federation/bond-info"))
        .send()
        .await
        .map_err(|e| coordinator_unreachable(&coordinator_url, "read this node's stake", &e))?
        .json()
        .await
        .context("decode bond-info")?;

    let path = format!("/federation/operators/{operator_key}/bond");
    let signed_at = epoch_ms();
    let signature = covenant_compute_protocol::sign_read(&identity, &path, signed_at)
        .map_err(|e| anyhow::anyhow!("sign feed read: {e}"))?;
    let resp = http
        .get(format!("{coordinator_url}{path}"))
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            signed_at.to_string(),
        )
        .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, signature)
        .send()
        .await
        .context("fetch bond feed")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("bond feed returned {status}: {}", coordinator_reason(&body));
    }
    let feed: serde_json::Value = resp.json().await.context("decode bond feed")?;

    if json_out {
        let doc = serde_json::json!({
            "operator_pubkey_b58": operator_key,
            "bond": feed,
            "bond_info": info,
        });
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }

    let status = &feed["status"];
    let usd = |v: &serde_json::Value| usd_micro(v.as_u64().unwrap_or(0));

    println!("operator:          {operator_key}");
    println!("posted:            {}", usd(&status["posted_micro_usdc"]));
    println!("slashed:           {}", usd(&status["slashed_micro_usdc"]));
    println!(
        "unbonding:         {}",
        usd(&status["unbonding_micro_usdc"])
    );
    println!("refunded:          {}", usd(&status["refunded_micro_usdc"]));
    println!(
        "at stake:          {} (slashable until it leaves)",
        usd(&status["at_stake_micro_usdc"])
    );
    println!(
        "committed:         {} (what the matcher sees)",
        usd(&status["committed_micro_usdc"])
    );
    if let Some(floor) = info["min_bond_micro_usdc"].as_u64() {
        if floor > 0 {
            let committed = status["committed_micro_usdc"].as_u64().unwrap_or(0);
            let verdict = if committed >= floor {
                "met"
            } else {
                "NOT met — this node wins no organic work"
            };
            println!("deployment floor:  {floor} micro-USDC — {verdict}");
        }
    }

    if let Some(slashes) = feed["slashes"].as_array().filter(|s| !s.is_empty()) {
        println!("\nslashes, newest first:");
        for s in slashes.iter().take(20) {
            println!(
                "  -{} micro-USDC  job {}  {}",
                s["amount_micro_usdc"].as_u64().unwrap_or(0),
                s["job_id"].as_str().unwrap_or("-"),
                flatten_controls(s["reason"].as_str().unwrap_or("-")),
            );
        }
    }
    if let Some(unbonds) = feed["unbonds"].as_array().filter(|u| !u.is_empty()) {
        println!("\nunbonds, newest first:");
        for u in unbonds.iter().take(20) {
            let paid = u["paid_micro_usdc"].as_u64().unwrap_or(0);
            let outcome = if u["pushed"].as_bool() == Some(true) {
                match u["tx_signature"].as_str() {
                    Some(sig) => format!("refunded {paid} micro-USDC, tx {sig}"),
                    None => format!("refunded {paid} micro-USDC (no on-chain signature reported)"),
                }
            } else {
                format!(
                    "maturing until epoch-ms {}",
                    u["matures_at_ms"].as_u64().unwrap_or(0)
                )
            };
            println!(
                "  {} micro-USDC requested  {}  memo {}",
                u["amount_micro_usdc"].as_u64().unwrap_or(0),
                outcome,
                u["memo"].as_str().unwrap_or("-"),
            );
        }
    }

    if status["posted_micro_usdc"].as_u64() == Some(0) {
        if info["configured"].as_bool() == Some(true) {
            println!(
                "\nnothing posted yet. To stake: {}\nmemo for this node: {}{}",
                info["how"].as_str().unwrap_or_default(),
                covenant_compute_protocol::BOND_MEMO_PREFIX,
                operator_key
            );
            println!("then: covenant-compute-node bond claim <tx-signature>");
        } else {
            println!("\nnothing posted, and this deployment has no inbound rail configured.");
        }
    }
    Ok(())
}

/// `bond claim <tx-signature>`: attribute an on-chain bond post to
/// this node. The coordinator's rail reads the transaction itself —
/// the claim carries nothing but the signature.
async fn claim_bond(home: &std::path::Path, args: &[String]) -> anyhow::Result<()> {
    let [tx_signature] = args else {
        anyhow::bail!("usage: bond claim <tx-signature>");
    };
    apply_node_env_defaults(home)?;
    let coordinator_url = coordinator_url_from_env()?;
    let identity = LocalIdentity::load_or_create(&home.join("identity.json"), "operator@compute")
        .context("load operator identity")?;
    let operator_key = bs58::encode(identity.pubkey_bytes()).into_string();

    let resp = coordinator_http()?
        .post(format!("{coordinator_url}/federation/operators/bond"))
        .json(&serde_json::json!({
            "operator_pubkey_b58": operator_key,
            "bond_id": tx_signature,
        }))
        .send()
        .await
        .map_err(|e| coordinator_unreachable(&coordinator_url, "post this bond claim", &e))?;
    let status = resp.status();
    let text = resp.text().await.context("read claim response")?;
    anyhow::ensure!(
        status.is_success(),
        "claim refused ({status}): {}",
        coordinator_reason(&text)
    );
    let body: serde_json::Value = serde_json::from_str(&text).context("decode claim response")?;
    if body["credited"].as_bool() == Some(true) {
        println!(
            "credited {} — posted total {}",
            usd_micro(body["amount_micro_usdc"].as_u64().unwrap_or(0)),
            usd_micro(body["posted_total_micro_usdc"].as_u64().unwrap_or(0)),
        );
    } else {
        println!(
            "already credited — posted total {}",
            usd_micro(body["posted_total_micro_usdc"].as_u64().unwrap_or(0)),
        );
    }
    Ok(())
}

/// `bond unbond <amount-micro-usdc> <recipient-address>`: a signed
/// request to take committed stake back out once the deployment's
/// unbonding window matures. The amount stays slashable until the
/// refund actually leaves.
async fn request_unbond(home: &std::path::Path, args: &[String]) -> anyhow::Result<()> {
    let [amount, recipient] = args else {
        anyhow::bail!("usage: bond unbond <amount-micro-usdc> <recipient-address>");
    };
    let amount_micro_usdc: u64 = amount
        .trim()
        .parse()
        .context("amount must be a positive integer of micro-USDC")?;
    apply_node_env_defaults(home)?;
    let coordinator_url = coordinator_url_from_env()?;
    let identity = LocalIdentity::load_or_create(&home.join("identity.json"), "operator@compute")
        .context("load operator identity")?;

    let request = covenant_compute_protocol::UnbondRequest::sign(
        identity.agent_id(),
        uuid::Uuid::new_v4(),
        amount_micro_usdc,
        recipient.trim().to_string(),
        epoch_ms(),
        &identity,
    )
    .map_err(|e| anyhow::anyhow!("sign unbond request: {e}"))?;
    let resp = coordinator_http()?
        .post(format!("{coordinator_url}/federation/operators/unbond"))
        .json(&request)
        .send()
        .await
        .map_err(|e| coordinator_unreachable(&coordinator_url, "submit this unbond request", &e))?;
    let status = resp.status();
    let text = resp.text().await.context("read unbond response")?;
    anyhow::ensure!(
        status.is_success(),
        "unbond refused ({status}): {}",
        coordinator_reason(&text)
    );
    let body: serde_json::Value = serde_json::from_str(&text).context("decode unbond response")?;
    println!(
        "unbond {} registered: {} to {}",
        body["unbond_id"].as_str().unwrap_or("-"),
        usd_micro(body["amount_micro_usdc"].as_u64().unwrap_or(0)),
        body["recipient_address_b58"].as_str().unwrap_or("-"),
    );
    println!(
        "matures at epoch-ms {} — slashable until the refund pushes; memo {}",
        body["matures_at_ms"].as_u64().unwrap_or(0),
        body["memo"].as_str().unwrap_or("-"),
    );
    Ok(())
}

/// Probes the configured executor's backend the way the serve loop's
/// gate does — `None` for the executors with nothing external to die
/// (echo, subprocess). Pinned `COVENANT_COMPUTE_NODE_MODELS` ride the
/// probe; a boot-discovered list is the running process's to re-check,
/// so an unpinned probe verdicts on reachability.
async fn probe_backend(identity: &LocalIdentity) -> Option<(String, Result<(), ExecutorError>)> {
    let kind = std::env::var("COVENANT_COMPUTE_NODE_EXECUTOR")
        .unwrap_or_else(|_| "subprocess".into())
        .trim()
        .to_ascii_lowercase();
    let pinned_models: Vec<String> = std::env::var("COVENANT_COMPUTE_NODE_MODELS")
        .map(|list| {
            list.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    match kind.as_str() {
        "ollama" => {
            let url = std::env::var("COVENANT_COMPUTE_OLLAMA_URL")
                .unwrap_or_else(|_| ollama::DEFAULT_OLLAMA_URL.into());
            let result = OllamaExecutor::new(url.clone(), None)
                .require_models(pinned_models)
                .health()
                .await;
            Some((format!("ollama at {url}"), result))
        }
        "openai-compat" => {
            let url = std::env::var("COVENANT_COMPUTE_OPENAI_URL")
                .unwrap_or_else(|_| openai_compat::DEFAULT_OPENAI_COMPAT_URL.into());
            let api_key = std::env::var("COVENANT_COMPUTE_OPENAI_API_KEY")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty());
            let result = OpenAiCompatExecutor::new(url.clone(), api_key, None)
                .require_models(pinned_models)
                .health()
                .await;
            Some((format!("openai-compat at {url}"), result))
        }
        "container" => {
            let env_opt = |key: &str| {
                std::env::var(key)
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
            };
            let Some(image) = env_opt("COVENANT_COMPUTE_NODE_CONTAINER_IMAGE") else {
                return Some((
                    "container".into(),
                    Err(ExecutorError::Failed(
                        "COVENANT_COMPUTE_NODE_CONTAINER_IMAGE is unset — serve refuses to \
                         boot without it"
                            .into(),
                    )),
                ));
            };
            let runtime = env_opt("COVENANT_COMPUTE_NODE_CONTAINER_RUNTIME")
                .unwrap_or_else(|| "docker".into());
            let config = ContainerConfig {
                runtime_binary: runtime.clone(),
                image,
                oci_runtime: env_opt("COVENANT_COMPUTE_NODE_CONTAINER_OCI_RUNTIME"),
                gpus: env_opt("COVENANT_COMPUTE_NODE_CONTAINER_GPUS"),
                network: env_opt("COVENANT_COMPUTE_NODE_CONTAINER_NETWORK")
                    .unwrap_or_else(|| "none".into()),
                memory: env_opt("COVENANT_COMPUTE_NODE_CONTAINER_MEMORY")
                    .unwrap_or_else(|| "512m".into()),
                cpus: env_opt("COVENANT_COMPUTE_NODE_CONTAINER_CPUS").unwrap_or_else(|| "1".into()),
                pids_limit: env_or("COVENANT_COMPUTE_NODE_CONTAINER_PIDS", 256u32),
                user: env_opt("COVENANT_COMPUTE_NODE_CONTAINER_USER"),
                instance_tag: identity
                    .agent_id()
                    .pubkey_base58()
                    .chars()
                    .take(8)
                    .collect(),
            };
            let result = ContainerJobExecutor::new(
                config,
                Arc::new(SubprocessTracker::new()),
                Duration::from_secs(2),
            )
            .health()
            .await;
            Some((format!("container engine ({runtime})"), result))
        }
        "whisper" => {
            let Some(model_path) = std::env::var("COVENANT_COMPUTE_WHISPER_MODEL")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
            else {
                return Some((
                    "whisper".into(),
                    Err(ExecutorError::Failed(
                        "COVENANT_COMPUTE_WHISPER_MODEL is unset — serve refuses to boot without \
                         a model file"
                            .into(),
                    )),
                ));
            };
            let binary = std::env::var("COVENANT_COMPUTE_WHISPER_BIN")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| DEFAULT_WHISPER_BIN.into());
            let result = WhisperExecutor::new(
                binary,
                model_path.clone(),
                "whisper-1",
                Arc::new(SubprocessTracker::new()),
                Duration::from_secs(2),
            )
            .health()
            .await;
            Some((format!("whisper model {model_path}"), result))
        }
        "say" => {
            let binary = std::env::var("COVENANT_COMPUTE_SAY_BIN")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| DEFAULT_SAY_BIN.into());
            let result = SayExecutor::new(
                binary.clone(),
                "say-1",
                None,
                Arc::new(SubprocessTracker::new()),
                Duration::from_secs(2),
            )
            .health()
            .await;
            Some((format!("say synthesizer ({binary})"), result))
        }
        _ => None,
    }
}

/// `status`: the whole "is my node healthy and earning" answer in one
/// read — identity, coordinator reachability, the executor backend's
/// health (probed exactly like the serve loop's gate), directory
/// standing against the matcher's gates (the coordinator's public
/// reputation view), the local earnings picture, and the market this
/// node competes in (the live-capacity directory filtered to what it
/// serves). When the node is not matchable, each failing gate prints
/// with what fixes it; `matchable` with no wins means price or
/// capability fit — which is what the market section reads.
/// `status --json`: the machine-readable twin of [`print_status`] for a
/// monitor or a scheduled health check. Gathers the same facts — local
/// standing, backend health, coordinator reachability and the operator's
/// directory standing — into one JSON object, leaving the intricate human
/// view below untouched.
async fn status_json(home: &std::path::Path) -> anyhow::Result<()> {
    apply_node_env_defaults(home)?;
    let identity = LocalIdentity::load_or_create(&home.join("identity.json"), "operator@compute")
        .context("load operator identity")?;
    let operator_key = bs58::encode(identity.pubkey_bytes()).into_string();
    let (hardware, vram_gb) = resolve_hardware().await;

    let executor_kind = std::env::var("COVENANT_COMPUTE_NODE_EXECUTOR")
        .unwrap_or_else(|_| "subprocess".into())
        .trim()
        .to_ascii_lowercase();
    let gpu_advertised_but_not_passed = executor_kind == "container" && {
        let container_gpus = std::env::var("COVENANT_COMPUTE_NODE_CONTAINER_GPUS")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        gpu_advertised_but_not_passed(&hardware, container_gpus.as_deref())
    };

    let ledger = JsonlEarningsLedger::read_only(&home.join("earnings.jsonl"))
        .map_err(|e| anyhow::anyhow!("open earnings ledger: {e}"))?;
    let credited = ledger.recent(usize::MAX).await;
    let earned: u64 = credited.iter().map(|e| e.amount_micro_usdc).sum();
    let unpaid = ledger.unpaid_total_micro_usdc().await;
    let outbox_pending = covenant_compute_node::ResultOutbox::read_only(&home.join("outbox.jsonl"))
        .map_err(|e| anyhow::anyhow!("open result outbox: {e}"))?
        .pending()
        .len();
    let accepted_pending =
        covenant_compute_node::AcceptedBook::read_only(&home.join("accepted.jsonl"))
            .map_err(|e| anyhow::anyhow!("open accepted book: {e}"))?
            .pending()
            .len();

    let backend = match probe_backend(&identity).await {
        Some((what, Ok(()))) => serde_json::json!({ "target": what, "healthy": true }),
        Some((what, Err(e))) => {
            serde_json::json!({ "target": what, "healthy": false, "error": e.to_string() })
        }
        None => serde_json::Value::Null,
    };

    let (coordinator, standing) = match coordinator_url_from_env() {
        Err(_) => (
            serde_json::json!({ "configured": false }),
            serde_json::Value::Null,
        ),
        Ok(url) => {
            let http = coordinator_http()?;
            match http.get(format!("{url}/health")).send().await {
                Ok(resp) if resp.status().is_success() => {
                    let standing = match http
                        .get(format!(
                            "{url}/federation/operators/{operator_key}/reputation"
                        ))
                        .send()
                        .await
                    {
                        Ok(r) => r.json().await.unwrap_or(serde_json::Value::Null),
                        Err(_) => serde_json::Value::Null,
                    };
                    (
                        serde_json::json!({ "url": url, "reachable": true }),
                        standing,
                    )
                }
                Ok(resp) => (
                    serde_json::json!({
                        "url": url,
                        "reachable": false,
                        "http_status": resp.status().as_u16(),
                    }),
                    serde_json::Value::Null,
                ),
                Err(e) => (
                    serde_json::json!({ "url": url, "reachable": false, "error": e.to_string() }),
                    serde_json::Value::Null,
                ),
            }
        }
    };

    let doc = serde_json::json!({
        "operator_pubkey_b58": operator_key,
        "home": home.display().to_string(),
        "hardware": hardware,
        "vram_gb": vram_gb,
        "gpu_advertised_but_not_passed": gpu_advertised_but_not_passed,
        "earnings": {
            "jobs_credited": credited.len(),
            "earned_micro_usdc": earned,
            "unpaid_micro_usdc": unpaid,
        },
        "outbox_pending": outbox_pending,
        "accepted_pending": accepted_pending,
        "backend": backend,
        "coordinator": coordinator,
        "standing": standing,
    });
    println!("{}", serde_json::to_string_pretty(&doc)?);
    Ok(())
}

async fn print_status(home: &std::path::Path) -> anyhow::Result<()> {
    apply_node_env_defaults(home)?;
    let identity = LocalIdentity::load_or_create(&home.join("identity.json"), "operator@compute")
        .context("load operator identity")?;
    let operator_key = bs58::encode(identity.pubkey_bytes()).into_string();
    println!("operator:      {operator_key}");
    println!("home:          {}", home.display());
    let (hardware, vram_gb) = resolve_hardware().await;
    println!("advertising:   {}", describe_hardware(&hardware, vram_gb));
    // A container node that advertises a GPU but passes no device runs
    // those jobs CPU-only. The boot log says so, but status is where an
    // operator debugging missed GPU work will look.
    let executor_kind = std::env::var("COVENANT_COMPUTE_NODE_EXECUTOR")
        .unwrap_or_else(|_| "subprocess".into())
        .trim()
        .to_ascii_lowercase();
    if executor_kind == "container" {
        let container_gpus = std::env::var("COVENANT_COMPUTE_NODE_CONTAINER_GPUS")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        if gpu_advertised_but_not_passed(&hardware, container_gpus.as_deref()) {
            println!(
                "gpu:           advertised, but the container passes no device — these jobs run \
                 CPU-only; set COVENANT_COMPUTE_NODE_CONTAINER_GPUS=all"
            );
        }
    }
    // A hand-set JOB_KINDS the executor can't serve fails the serve boot;
    // surface it here too, where an operator checking their setup looks,
    // with the same fix the boot refusal names.
    let advertised_kinds = parse_job_kinds(
        &std::env::var("COVENANT_COMPUTE_NODE_JOB_KINDS")
            .unwrap_or_else(|_| default_kinds(&executor_kind).into()),
    );
    if let Some(servable) = servable_kinds(&executor_kind) {
        if let Some(bad) = advertised_kinds.iter().find(|k| !servable.contains(k)) {
            println!(
                "job kinds:     {executor_kind} can't serve {} — the node won't boot until \
                 COVENANT_COMPUTE_NODE_JOB_KINDS is {} or the executor changes",
                kind_label(*bad),
                servable
                    .iter()
                    .map(|k| kind_label(*k))
                    .collect::<Vec<_>>()
                    .join(",")
            );
        }
    }

    let ledger = JsonlEarningsLedger::read_only(&home.join("earnings.jsonl"))
        .map_err(|e| anyhow::anyhow!("open earnings ledger: {e}"))?;
    let credited = ledger.recent(usize::MAX).await;
    let earned: u64 = credited.iter().map(|e| e.amount_micro_usdc).sum();
    let unpaid = ledger.unpaid_total_micro_usdc().await;
    println!(
        "earnings:      {} jobs credited, {earned} micro-USDC earned (${:.6}), {unpaid} \
         micro-USDC unpaid — details under `earnings`",
        credited.len(),
        earned as f64 / 1e6
    );
    let queued = covenant_compute_node::ResultOutbox::read_only(&home.join("outbox.jsonl"))
        .map_err(|e| anyhow::anyhow!("open result outbox: {e}"))?
        .pending()
        .len();
    if queued > 0 {
        println!(
            "outbox:        {queued} result(s) awaiting redelivery — not credited yet; the \
             serve loop re-pushes them until the coordinator answers"
        );
    }
    let interrupted = covenant_compute_node::AcceptedBook::read_only(&home.join("accepted.jsonl"))
        .map_err(|e| anyhow::anyhow!("open accepted book: {e}"))?
        .pending()
        .len();
    if interrupted > 0 {
        println!(
            "interrupted:   {interrupted} accepted job(s) a previous run never finished — \
             re-served at the next `serve` if their deadlines still allow"
        );
    }

    let mut backend_down = false;
    if let Some((what, result)) = probe_backend(&identity).await {
        match result {
            Ok(()) => println!("backend:       {what} — healthy"),
            Err(e) => {
                backend_down = true;
                println!(
                    "backend:       {what} — UNHEALTHY ({e}); a running serve process is \
                     paused at its gate, reporting offline, and resumes by itself when the \
                     backend answers"
                );
            }
        }
    }

    let coordinator_url = match coordinator_url_from_env() {
        Ok(url) => url,
        Err(_) => {
            println!(
                "coordinator:   NOT CONFIGURED — run `setup`, or set \
                 COVENANT_COMPUTE_COORDINATOR_URL"
            );
            return Ok(());
        }
    };
    let http = coordinator_http()?;
    match http.get(format!("{coordinator_url}/health")).send().await {
        Ok(resp) if resp.status().is_success() => {
            println!("coordinator:   {coordinator_url} — reachable");
        }
        Ok(resp) => {
            println!(
                "coordinator:   {coordinator_url} — responded {} to /health; standing unknown",
                resp.status()
            );
            return Ok(());
        }
        Err(e) => {
            println!(
                "coordinator:   {coordinator_url} — UNREACHABLE ({})",
                coordinator_unreachable_reason(&e)
            );
            return Ok(());
        }
    }

    let standing: serde_json::Value = http
        .get(format!(
            "{coordinator_url}/federation/operators/{operator_key}/reputation"
        ))
        .send()
        .await
        .context("fetch operator standing")?
        .json()
        .await
        .context("decode operator standing")?;

    let registered = standing["registered"].as_bool() == Some(true);
    if registered {
        println!(
            "directory:     registered, declared {}, seen {}s ago",
            standing["status"].as_str().unwrap_or("unknown"),
            standing["seen_ms_ago"].as_u64().unwrap_or(0) / 1_000
        );
    } else {
        println!("directory:     not registered");
    }
    println!(
        "reputation:    score {} bps — {} released, {} faults, canary {}/{} passed/failed, \
         {} disputed, redundancy {}/{} agreed/disagreed",
        standing["score_bps"],
        standing["released"],
        standing["faults"],
        standing["canary_passed"],
        standing["canary_failed"],
        standing["disputed"],
        standing["redundancy_agreed"],
        standing["redundancy_disagreed"],
    );
    print_book_lines(&coordinator_url, &identity).await;

    let score = standing["score_bps"].as_u64().unwrap_or(0);
    let score_floor = standing["min_score_bps"].as_u64().unwrap_or(0);
    let committed = standing["committed_bond_micro_usdc"].as_u64().unwrap_or(0);
    let bond_floor = standing["min_bond_micro_usdc"].as_u64().unwrap_or(0);
    if score_floor > 0 {
        let verdict = if score >= score_floor {
            "met"
        } else {
            "NOT met"
        };
        println!("score floor:   {score_floor} bps — {verdict}");
    }
    if bond_floor > 0 {
        let verdict = if committed >= bond_floor {
            "met"
        } else {
            "NOT met"
        };
        println!(
            "bond floor:    {bond_floor} micro-USDC committed — {verdict} ({committed} committed)"
        );
    }

    let matchable = standing["matchable"].as_bool() == Some(true);
    if matchable {
        println!(
            "matchable:     yes — winning a given job still depends on its price and \
             capability fit"
        );
        return print_market(&http, &coordinator_url, true).await;
    }
    println!("matchable:     NO");
    if !registered {
        println!(
            "  - not in the coordinator's directory right now (registration does not \
             survive its restarts): start the node (plain `covenant-compute-node`) or \
             install it as a service (`service install`) and it re-registers itself"
        );
    } else if standing["live"].as_bool() != Some(true) {
        if standing["status"].as_str() == Some("offline") {
            if backend_down {
                println!(
                    "  - the node reports offline because its backend is down (see \
                     `backend:` above): fix the backend; a running serve process resumes \
                     and re-registers on its own"
                );
            } else {
                println!(
                    "  - the node declared itself offline (a clean shutdown or drain does \
                     this): start it again to serve"
                );
            }
        } else if standing["status"].as_str() == Some("busy") {
            println!(
                "  - the node last reported itself at capacity {}s ago: every in-flight slot is \
                 busy, so the coordinator holds new jobs off it until one frees. A recent report \
                 means it is working and becomes matchable on its own; a long gap means the serve \
                 process stalled mid-job",
                standing["seen_ms_ago"].as_u64().unwrap_or(0) / 1_000
            );
        } else {
            println!(
                "  - heartbeats have stopped reaching the coordinator (declared {}, seen \
                 {}s ago): the serve process likely crashed or lost its network",
                standing["status"].as_str().unwrap_or("unknown"),
                standing["seen_ms_ago"].as_u64().unwrap_or(0) / 1_000
            );
        }
    }
    if score < score_floor {
        println!(
            "  - score {score} bps sits under the deployment's {score_floor} bps floor: \
             passed canary probes are the road back up — keep the node serving"
        );
    }
    if committed < bond_floor {
        println!(
            "  - committed stake {committed} micro-USDC sits under the deployment's \
             {bond_floor} floor: see `bond` for posting instructions"
        );
    }
    print_market(&http, &coordinator_url, false).await
}

/// The `unpaid rows:` and `disputed:` lines of `status` — this
/// operator's job books, read once, giving the counts above their
/// names. Unpaid conclusions summarize by refund reason: which
/// conclusions fault this node's standing, and which were the buyer
/// walking away before anyone accepted. Disputed jobs list one per
/// line with the buyer's complaint — a standing that says `1 disputed`
/// is only actionable if the operator can see which job and why. Rows
/// from a coordinator that predates either field degrade quietly
/// (bare-status summaries, no disputed lines). Silent when every row
/// paid undisputed, and soft on a failed read — standing already
/// printed, and `earnings` is the money surface.
async fn print_book_lines(coordinator_url: &str, identity: &LocalIdentity) {
    let client = HttpCoordinatorClient::with_config(
        coordinator_url.to_string(),
        std::time::Duration::from_secs(5),
        1,
    );
    let Ok(rows) = client.operator_jobs(identity).await else {
        return;
    };
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for row in &rows {
        if matches!(row.status.as_str(), "offered" | "accepted" | "completed") {
            continue;
        }
        let key = row
            .refund_reason
            .clone()
            .unwrap_or_else(|| row.status.clone());
        *counts.entry(key).or_default() += 1;
    }
    if !counts.is_empty() {
        let rendered: Vec<String> = counts
            .into_iter()
            .map(|(reason, n)| {
                let verdict = match reason.as_str() {
                    "buyer_cancelled" => " (no fault — the buyer walked away)",
                    "deadline_expired" | "execution_failed" | "operator_rejected" | "rejected"
                    | "failed" => " (faults your standing)",
                    _ => "",
                };
                format!("{reason} {n}{verdict}")
            })
            .collect();
        println!("unpaid rows:   {}", rendered.join(", "));
    }
    for row in rows.iter().filter(|r| r.disputed) {
        // The complaint is buyer-authored text landing in a terminal, so
        // its control characters flatten before it prints.
        let reason = flatten_controls(
            row.dispute_reason
                .as_deref()
                .unwrap_or("(reason not recorded by this coordinator)"),
        );
        println!("disputed:      {} — \"{reason}\"", row.job_id);
    }
}

/// Where this node's ask sits in one capacity row, as the trailing
/// clause of a `status` market line. `my_ask`/`my_unit` are this
/// invocation's local config; the row's figures are what the coordinator
/// has registered and the matcher actually prices against. For the
/// sole-operator row the two describe the same offer, so a local price
/// edited but not yet re-registered (the running serve process still
/// advertises the old one) is surfaced as drift rather than printed as
/// the live offer — otherwise the line would claim a price the market
/// does not yet show.
fn market_position(
    matchable: bool,
    my_ask: u64,
    my_unit: &str,
    row_operators: usize,
    row_min_ask: u64,
    row_min_unit: PriceUnit,
) -> String {
    match (matchable, my_ask <= row_min_ask, row_operators) {
        (true, _, 1) if my_ask == row_min_ask => {
            format!("your ask {my_ask} micro-USDC ({my_unit}) is the row's only offer")
        }
        (true, _, 1) => format!(
            "your registered ask {row_min_ask} micro-USDC ({}) is the row's only offer — your \
             local config now reads {my_ask} micro-USDC ({my_unit}); restart the node to \
             re-register at it",
            unit_label(row_min_unit)
        ),
        (true, true, _) => format!("your ask {my_ask} micro-USDC ({my_unit}) sets the row's floor"),
        (true, false, _) => format!(
            "your ask {my_ask} micro-USDC ({my_unit}) sits above the {row_min_ask} floor — \
             price-sorted matching tries cheaper supply first"
        ),
        (false, true, _) => format!(
            "your ask {my_ask} micro-USDC ({my_unit}) would set the row's floor once matchable"
        ),
        (false, false, _) => format!(
            "your ask {my_ask} micro-USDC ({my_unit}) would sit above the {row_min_ask} floor"
        ),
    }
}

/// The market section of `status`: the coordinator's live-capacity
/// directory (the same `GET /federation/capacity` a buyer reads),
/// filtered to the (kind, model) rows this node declares it serves,
/// with the node's own ask placed against each row. Asks compare on
/// raw micro-USDC whatever their declared unit — exactly how the
/// matcher prices a job — and generic (`any`) supply competes with
/// every named-model row of the same kind, so both directions carry a
/// cross-reference.
async fn print_market(
    http: &reqwest::Client,
    coordinator_url: &str,
    matchable: bool,
) -> anyhow::Result<()> {
    let view: CapacityView = http
        .get(format!("{coordinator_url}/federation/capacity"))
        .send()
        .await
        .map_err(|e| coordinator_unreachable(coordinator_url, "read the capacity directory", &e))?
        .json()
        .await
        .context("decode capacity directory")?;
    println!(
        "market:        {} matchable of {} registered operator(s) in the directory",
        view.matchable_operators, view.registered_operators
    );

    let executor_kind = std::env::var("COVENANT_COMPUTE_NODE_EXECUTOR")
        .unwrap_or_else(|_| "subprocess".into())
        .trim()
        .to_ascii_lowercase();
    let mut models = match resolve_models_served(&executor_kind).await {
        Ok(models) => models
            .iter()
            .map(|m| canonical_model(m).to_string())
            .collect::<Vec<_>>(),
        Err(e) => {
            println!("  (this node's own rows are unknown — {e})");
            return Ok(());
        }
    };
    models.sort_unstable();
    models.dedup();
    let default_job_kinds = default_kinds(&executor_kind);
    let kinds = parse_job_kinds(
        &std::env::var("COVENANT_COMPUTE_NODE_JOB_KINDS")
            .unwrap_or_else(|_| default_job_kinds.into()),
    );
    let my_ask = env_or(
        "COVENANT_COMPUTE_NODE_PRICE_MICRO_USDC",
        covenant_compute_node::DEFAULT_PRICE_MICRO_USDC,
    );
    let my_unit = unit_label(parse_price_unit(
        &std::env::var("COVENANT_COMPUTE_NODE_PRICE_UNIT").unwrap_or_else(|_| "per_job".into()),
    ));

    for kind in kinds {
        for model in &models {
            let row = view
                .entries
                .iter()
                .find(|e| e.kind == kind && &e.model == model);
            let Some(row) = row else {
                if matchable {
                    println!(
                        "  {}/{model} — the directory lists no matchable supply for this \
                         pairing yet",
                        kind_label(kind)
                    );
                } else {
                    println!(
                        "  {}/{model} — no matchable supply serves this pairing right now; \
                         your ask {my_ask} micro-USDC ({my_unit}) would be the only offer",
                        kind_label(kind)
                    );
                }
                continue;
            };
            let mut line = format!(
                "  {}/{model} — {} matchable operator(s), asks {}..{} micro-USDC",
                kind_label(kind),
                row.operators,
                row.min_ask_micro_usdc,
                row.max_ask_micro_usdc
            );
            if model == "any" {
                let named: Vec<_> = view
                    .entries
                    .iter()
                    .filter(|e| e.kind == kind && e.model != "any")
                    .collect();
                if let Some(cheapest) = named.iter().map(|e| e.min_ask_micro_usdc).min() {
                    line.push_str(&format!(
                        " (named-model supply of this kind also competes: {} row(s), asks \
                         from {cheapest})",
                        named.len()
                    ));
                }
            } else if let Some(generic) = view
                .entries
                .iter()
                .find(|e| e.kind == kind && e.model == "any")
            {
                line.push_str(&format!(
                    " (+{} generic any-model operator(s) asking from {})",
                    generic.operators, generic.min_ask_micro_usdc
                ));
            }
            let position = market_position(
                matchable,
                my_ask,
                my_unit,
                row.operators,
                row.min_ask_micro_usdc,
                row.min_ask_unit,
            );
            println!("{line}; {position}");
        }
    }
    Ok(())
}

async fn print_earnings(home: &std::path::Path, json_out: bool) -> anyhow::Result<()> {
    let ledger = JsonlEarningsLedger::read_only(&home.join("earnings.jsonl"))
        .map_err(|e| anyhow::anyhow!("open earnings ledger: {e}"))?;
    let recent = ledger.recent(usize::MAX).await;
    let unpaid = ledger.unpaid_total_micro_usdc().await;
    let earned: u64 = recent.iter().map(|e| e.amount_micro_usdc).sum();
    let fees: u64 = recent.iter().map(|e| e.fee_micro_usdc).sum();
    let (paid, paid_jobs) = recent
        .iter()
        .filter(|e| e.status == covenant_compute_node::EarningsStatus::Paid)
        .fold((0u64, 0usize), |(total, count), e| {
            (total + e.amount_micro_usdc, count + 1)
        });
    let queued = covenant_compute_node::ResultOutbox::read_only(&home.join("outbox.jsonl"))
        .map_err(|e| anyhow::anyhow!("open result outbox: {e}"))?
        .pending();
    let interrupted = covenant_compute_node::AcceptedBook::read_only(&home.join("accepted.jsonl"))
        .map_err(|e| anyhow::anyhow!("open accepted book: {e}"))?
        .pending();

    if json_out {
        let now = epoch_ms();
        let doc = serde_json::json!({
            "jobs_credited": recent.len(),
            "earned_total_micro_usdc": earned,
            "fees_withheld_micro_usdc": fees,
            "paid_out_micro_usdc": paid,
            "paid_jobs": paid_jobs,
            "unpaid_micro_usdc": unpaid,
            "awaiting_redelivery": queued
                .iter()
                .map(|entry| {
                    serde_json::json!({
                        "job_id": entry.job_id,
                        "price_micro_usdc": entry.message.receipt.receipt.price_micro_usdc,
                        "queued_ms_ago": now.saturating_sub(entry.queued_at_ms),
                    })
                })
                .collect::<Vec<_>>(),
            "interrupted": interrupted
                .iter()
                .map(|entry| {
                    serde_json::json!({
                        "job_id": entry.job_id,
                        "price_micro_usdc": entry.envelope.payload.price_micro_usdc,
                        "accepted_ms_ago": now.saturating_sub(entry.accepted_at_ms),
                    })
                })
                .collect::<Vec<_>>(),
            "entries": recent,
        });
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }

    println!("jobs credited:     {}", recent.len());
    println!(
        "earned total:      {earned} micro-USDC (${:.6})",
        earned as f64 / 1e6
    );
    if fees > 0 {
        println!(
            "fees withheld:     {fees} micro-USDC (${:.6}) — disclosed coordinator take, \
             already deducted above",
            fees as f64 / 1e6
        );
    }
    println!(
        "paid out:          {paid} micro-USDC (${:.6}) across {paid_jobs} jobs",
        paid as f64 / 1e6
    );
    println!(
        "unpaid balance:    {unpaid} micro-USDC (${:.6})",
        unpaid as f64 / 1e6
    );
    if !queued.is_empty() {
        println!(
            "awaiting redelivery: {} result(s) the coordinator has not acknowledged — they \
             credit (or settle as refunds) when the serve loop's next re-push lands",
            queued.len()
        );
        for entry in &queued {
            println!(
                "  {}  {:>12} micro-USDC  queued {}s ago",
                entry.job_id,
                entry.message.receipt.receipt.price_micro_usdc,
                epoch_ms().saturating_sub(entry.queued_at_ms) / 1_000
            );
        }
    }
    if !interrupted.is_empty() {
        println!(
            "interrupted:       {} accepted job(s) a previous run never finished — the next \
             `serve` re-runs each whose deadline still allows, and its earnings follow the \
             usual settlement",
            interrupted.len()
        );
        for entry in &interrupted {
            println!(
                "  {}  {:>12} micro-USDC  accepted {}s ago",
                entry.job_id,
                entry.envelope.payload.price_micro_usdc,
                epoch_ms().saturating_sub(entry.accepted_at_ms) / 1_000
            );
        }
    }
    if !recent.is_empty() {
        println!("\nmost recent first:");
    }
    for e in recent.iter().take(20) {
        let settled = match (&e.status, e.paid_tx_signature.as_deref()) {
            (covenant_compute_node::EarningsStatus::Paid, Some(sig)) => format!("paid tx {sig}"),
            (covenant_compute_node::EarningsStatus::Paid, None) => {
                "paid (no on-chain signature reported)".into()
            }
            _ => "unpaid".into(),
        };
        println!(
            "  {}  {:>12} micro-USDC  {:?}  {settled}",
            e.job_id, e.amount_micro_usdc, e.funding_source
        );
    }
    Ok(())
}

/// `node.env` (written by `setup`) supplies env defaults; variables
/// already set in the environment win.
fn apply_node_env_defaults(home: &std::path::Path) -> anyhow::Result<()> {
    let env_file = home.join(covenant_compute_node::ENV_FILE);
    let Ok(raw) = std::fs::read_to_string(&env_file) else {
        return Ok(());
    };
    // node.env can hold an API key. Repair a secret an older setup left
    // world-readable (or a copy/restore widened) in place on every boot,
    // the same self-heal the identity key does when it loads.
    covenant_compute_node::ensure_owner_only(&env_file);
    let pairs = covenant_compute_node::parse_env_file(&raw)
        .map_err(|e| anyhow::anyhow!("{}: {e}", env_file.display()))?;
    let apply = covenant_compute_node::unset_pairs(pairs, |key| std::env::var_os(key).is_some());
    if !apply.is_empty() {
        tracing::info!(
            count = apply.len(),
            path = %env_file.display(),
            "applying node.env defaults"
        );
        for (key, value) in apply {
            std::env::set_var(key, value);
        }
    }
    Ok(())
}

/// The optional `--job <id>` filter for `earnings verify`: hold the chain
/// to one job's payout instead of the whole ledger — a targeted check
/// that skips re-fetching every paid row. `--job` is the only flag.
fn parse_verify_job(args: &[String]) -> anyhow::Result<Option<uuid::Uuid>> {
    let mut it = args.iter();
    let mut job = None;
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--job" => {
                let raw = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--job needs a job id"))?;
                job = Some(
                    uuid::Uuid::parse_str(raw.trim())
                        .with_context(|| format!("--job {raw:?} is not a job id"))?,
                );
            }
            other => anyhow::bail!(
                "unknown `earnings verify` flag {other:?} — the only flag is `--job <id>`"
            ),
        }
    }
    Ok(job)
}

/// `covenant-compute-node earnings verify` — hold the chain to every
/// Paid row. Each payout transaction is fetched from the operator's
/// OWN RPC endpoint (`COVENANT_COMPUTE_NODE_RPC_URL` — never the
/// coordinator's suggestion, which could vouch for its own transfers)
/// and must carry this node's receipt-derived memo, have moved exactly
/// the credited amount, and — when `COVENANT_COMPUTE_PAYOUT_ADDRESS`
/// is set — have paid this operator's wallet. Exits nonzero when any
/// paid row is contradicted or unreachable, so a scheduled `earnings
/// verify` means silence == every paid row proven on-chain.
async fn verify_earnings(home: &std::path::Path, args: &[String]) -> anyhow::Result<()> {
    let job_filter = parse_verify_job(args)?;
    apply_node_env_defaults(home)?;
    let rpc_url = std::env::var("COVENANT_COMPUTE_NODE_RPC_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "COVENANT_COMPUTE_NODE_RPC_URL is not set — pick your own RPC endpoint \
                 (e.g. https://api.devnet.solana.com); the read-back must not go through \
                 anyone with a stake in the answer"
            )
        })?;
    anyhow::ensure!(
        rpc_url.starts_with("http://") || rpc_url.starts_with("https://"),
        "COVENANT_COMPUTE_NODE_RPC_URL must start with http:// or https:// (got {rpc_url:?})"
    );
    let own_wallet = std::env::var("COVENANT_COMPUTE_PAYOUT_ADDRESS")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());

    let ledger = JsonlEarningsLedger::read_only(&home.join("earnings.jsonl"))
        .map_err(|e| anyhow::anyhow!("open earnings ledger: {e}"))?;
    let mut entries = ledger.recent(usize::MAX).await;
    if let Some(job_id) = job_filter {
        entries.retain(|e| e.job_id == job_id);
        if entries.is_empty() {
            anyhow::bail!("no ledger row for job {job_id} — `earnings` lists this node's jobs");
        }
    }
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .context("build http client")?;

    match job_filter {
        Some(job_id) => println!("holding {rpc_url} to job {job_id}:"),
        None => println!("holding {rpc_url} to the ledger's paid rows:"),
    }
    if own_wallet.is_none() {
        println!("note: COVENANT_COMPUTE_PAYOUT_ADDRESS unset — recipients go unchecked");
    }
    let (mut verified, mut offchain, mut legacy, mut unpaid) = (0usize, 0usize, 0usize, 0usize);
    let (mut contradicted, mut unreachable, mut pending) = (0usize, 0usize, 0usize);
    for e in &entries {
        if e.status != EarningsStatus::Paid {
            unpaid += 1;
            continue;
        }
        let Some(tx_signature) = e.paid_tx_signature.as_deref() else {
            offchain += 1;
            println!(
                "  {}  offchain record only — the payout backend submitted no transaction",
                e.job_id
            );
            continue;
        };
        if e.receipt_signature_b58.is_none() {
            legacy += 1;
            println!(
                "  {}  unverifiable — credited before receipt journaling",
                e.job_id
            );
            continue;
        }
        match fetch_transaction(&http, &rpc_url, tx_signature).await {
            // getTransaction serves confirmed transactions only, so a
            // null read is this operator's own RPC not having served the
            // payout back yet, not a failed one. Tell that ("re-check
            // shortly") apart from a signature the chain never saw ("a
            // real contradiction").
            Ok(tx) if tx.is_null() => {
                match fetch_signature_status(&http, &rpc_url, tx_signature).await {
                    Ok(Some(level)) => {
                        pending += 1;
                        println!(
                            "  {}  pending — on chain ({level}) but your RPC hasn't served it \
                             back yet; re-check shortly (tx {tx_signature})",
                            e.job_id
                        );
                    }
                    Ok(None) => {
                        contradicted += 1;
                        println!(
                            "  {}  CONTRADICTED — recorded payout tx {tx_signature} has no \
                             record on your RPC",
                            e.job_id
                        );
                    }
                    Err(err) => {
                        unreachable += 1;
                        println!("  {}  unreachable — {err} (tx {tx_signature})", e.job_id);
                    }
                }
            }
            Ok(tx) => match audit_paid_entry(e, &tx, own_wallet.as_deref()) {
                Ok(proof) => {
                    verified += 1;
                    println!(
                        "  {}  verified on-chain — {} base units of {} to {} (tx {tx_signature})",
                        e.job_id,
                        proof.amount_micro_usdc,
                        proof.mint_b58,
                        proof.recipient_owner_b58
                    );
                }
                Err(err) => {
                    contradicted += 1;
                    println!("  {}  CONTRADICTED — {err} (tx {tx_signature})", e.job_id);
                }
            },
            Err(err) => {
                unreachable += 1;
                println!("  {}  unreachable — {err} (tx {tx_signature})", e.job_id);
            }
        }
    }
    println!(
        "\n{verified} verified on-chain, {pending} pending, {offchain} offchain-only, {legacy} \
         unverifiable, {unpaid} not yet paid, {contradicted} contradicted, {unreachable} unreachable"
    );
    if contradicted > 0 || unreachable > 0 {
        anyhow::bail!(
            "{} paid rows did not verify — see above",
            contradicted + unreachable
        );
    }
    Ok(())
}

/// The `getTransaction` fetch, request body single-sourced from the
/// protocol crate so this fetch can't drift from the shape the
/// verifier parses.
async fn fetch_transaction(
    http: &reqwest::Client,
    rpc_url: &str,
    signature: &str,
) -> anyhow::Result<serde_json::Value> {
    let resp = http
        .post(rpc_url)
        .json(&payout_transaction_rpc_request(signature))
        .send()
        .await
        .context("rpc transport")?;
    let status = resp.status();
    anyhow::ensure!(status.is_success(), "rpc returned {status}");
    let envelope: serde_json::Value = resp.json().await.context("rpc response decode")?;
    if let Some(err) = envelope.get("error") {
        anyhow::bail!("rpc error: {err}");
    }
    Ok(envelope
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

/// The confirmation level `signature` has reached on `rpc_url`'s recent
/// status cache (`getSignatureStatuses`), or `None` when that RPC has no
/// record of it. Lets `earnings verify` tell a payout still propagating
/// to the operator's own RPC apart from a signature the chain never saw.
async fn fetch_signature_status(
    http: &reqwest::Client,
    rpc_url: &str,
    signature: &str,
) -> anyhow::Result<Option<String>> {
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
        .context("rpc transport")?;
    let status = resp.status();
    anyhow::ensure!(status.is_success(), "rpc returned {status}");
    let envelope: serde_json::Value = resp.json().await.context("rpc response decode")?;
    if let Some(err) = envelope.get("error") {
        anyhow::bail!("rpc error: {err}");
    }
    Ok(envelope
        .pointer("/result/value/0/confirmationStatus")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string))
}

/// Set by the service definitions to a directory the node owns; when
/// present on the serve path, logs roll into dated files there instead of
/// stdout. The launchd agent sets it because launchd redirects stdout to
/// a single file it never rotates, so a machine left earning for months
/// would otherwise grow one log without bound. systemd leaves it unset —
/// journald already rotates the unit's output.
const LOG_DIR_ENV: &str = "COVENANT_COMPUTE_NODE_LOG_DIR";

/// Dated log files kept before the oldest is dropped on rotation.
const LOG_KEEP_FILES: usize = 7;

/// The rotating log directory for this run, or `None` to log to stdout.
/// Only serve honours it: the read/setup subcommands are interactive and
/// belong on the console, and an empty or whitespace value is treated as
/// unset so a blank service definition can't silence the console.
fn resolve_log_dir(serve_mode: bool, configured: Option<&str>) -> Option<String> {
    if !serve_mode {
        return None;
    }
    configured
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_string)
}

/// A daily-rotating file appender under `dir`, keeping [`LOG_KEEP_FILES`]
/// dated files. The directory is created if missing.
fn build_rolling_appender(
    dir: &str,
) -> anyhow::Result<tracing_appender::rolling::RollingFileAppender> {
    std::fs::create_dir_all(dir).with_context(|| format!("create log directory {dir}"))?;
    tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("node")
        .filename_suffix("log")
        .max_log_files(LOG_KEEP_FILES)
        .build(dir)
        .with_context(|| format!("open rotating log in {dir}"))
}

/// Installs the global tracing subscriber. Serve runs unattended under a
/// service manager; when it points [`LOG_DIR_ENV`] at a directory, output
/// goes to a daily-rotating, count-capped file so the disk can't fill.
/// Every other path (and a terminal serve run) logs to stderr, leaving
/// stdout for the command's own answer — so `status --json` stays valid
/// JSON even when a startup line (applying node.env defaults, a warning)
/// fires on the same run.
fn init_logging(serve_mode: bool) -> anyhow::Result<()> {
    let filter = || {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| "covenant_compute_node=info".into())
    };
    match resolve_log_dir(serve_mode, std::env::var(LOG_DIR_ENV).ok().as_deref()) {
        Some(dir) => {
            tracing_subscriber::fmt()
                .with_env_filter(filter())
                .with_ansi(false)
                .with_writer(build_rolling_appender(&dir)?)
                .init();
        }
        None => {
            tracing_subscriber::fmt()
                .with_env_filter(filter())
                .with_writer(std::io::stderr)
                .init();
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // Help and version answer before any filesystem or config touch —
    // asking a question must not mint an identity or create a home.
    match args.first().map(String::as_str) {
        Some("--help" | "-h" | "help") => {
            print!("{NODE_USAGE}");
            return Ok(());
        }
        Some("--version" | "-V" | "version") => {
            println!("{} {}", env!("CARGO_BIN_NAME"), env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        _ => {}
    }

    // `<subcommand> --help`/`-h` answers with that subcommand's usage and
    // exits clean, the way a first-time operator expects — setup and
    // service carry their own flag-level usage, the rest are covered by
    // the top-level table. Answered here, before any home or identity
    // touch, for the same reason the top-level help is: asking a question
    // must not mint an identity or create a home.
    if let Some(sub) = args.first().map(String::as_str) {
        if args[1..].iter().any(|a| a == "--help" || a == "-h") {
            match sub {
                "setup" => println!("{SETUP_USAGE}"),
                "service" => println!("{SERVICE_USAGE}"),
                _ => print!("{NODE_USAGE}"),
            }
            return Ok(());
        }
    }

    // `--json` switches the read subcommands (status, earnings, bond) from
    // their human summary to the underlying record, for a monitor or a
    // script — the same affordance the buyer CLI carries. Stripped up
    // front so it can sit anywhere on the line; inert on the commands that
    // only act.
    let json_out = take_flag(&mut args, "--json");

    let home = node_home()?;
    std::fs::create_dir_all(&home).with_context(|| format!("create {}", home.display()))?;

    // Serve is the bare invocation; every other subcommand below returns
    // before the loop. Only serve runs unattended under a service
    // manager, so only serve sends its output to the bounded rotating
    // file when the agent points `COVENANT_COMPUTE_NODE_LOG_DIR` at one.
    // A terminal run (LOG_DIR unset) logs to stderr, leaving stdout for a
    // command's own answer.
    let serve_mode = args.is_empty();
    init_logging(serve_mode)?;

    match args.first().map(String::as_str) {
        Some("earnings") => {
            return match args.get(1).map(String::as_str) {
                None => print_earnings(&home, json_out).await,
                Some("verify") => verify_earnings(&home, &args[2..]).await,
                Some(other) => anyhow::bail!(
                    "unknown earnings subcommand {other:?} — plain `earnings` prints the \
                     ledger, `earnings verify` holds each paid row to the chain's record"
                ),
            };
        }
        Some("setup") => {
            let opts = covenant_compute_node::parse_setup_args(&args[1..])
                .map_err(|e| anyhow::anyhow!(e))?;
            // The hardware probe happens here so the wizard stays
            // hermetic and testable: it takes the detected value, never
            // probes hardware itself.
            let detected_gpu = covenant_compute_node::detect_gpu().await;
            let mut input = std::io::stdin().lock();
            let mut out = std::io::stdout();
            covenant_compute_node::run_setup(&home, opts, detected_gpu, &mut input, &mut out)
                .await?;
            return Ok(());
        }
        Some("service") => {
            let action = covenant_compute_node::parse_service_args(&args[1..])
                .map_err(|e| anyhow::anyhow!(e))?;
            covenant_compute_node::run_service(&home, action, &mut std::io::stdout())
                .map_err(|e| anyhow::anyhow!(e))?;
            return Ok(());
        }
        Some("bond") => {
            return match args.get(1).map(String::as_str) {
                None => print_bond(&home, json_out).await,
                Some("claim") => claim_bond(&home, &args[2..]).await,
                Some("unbond") => request_unbond(&home, &args[2..]).await,
                Some(other) => anyhow::bail!(
                    "unknown bond subcommand {other:?} — plain `bond` prints this node's \
                     stake, `bond claim <tx-signature>` attributes an on-chain post, \
                     `bond unbond <amount-micro-usdc> <recipient>` requests a matured refund"
                ),
            };
        }
        Some("status") => {
            return if json_out {
                status_json(&home).await
            } else {
                print_status(&home).await
            };
        }
        Some(other) => {
            anyhow::bail!("unknown subcommand {other:?} — run `--help` for usage")
        }
        None => {}
    }

    // `node.env` (written by `setup`) supplies defaults for everything
    // below; variables already set in the environment win. Loaded
    // before any config read so the two paths can't disagree.
    apply_node_env_defaults(&home)?;

    let identity = LocalIdentity::load_or_create(&home.join("identity.json"), "operator@compute")
        .context("load or create operator identity")?;
    // Second handle onto the same persisted key for the heartbeat task;
    // `LocalIdentity` is deliberately not `Clone`.
    let heartbeat_identity =
        LocalIdentity::load_or_create(&home.join("identity.json"), "operator@compute")
            .context("reload operator identity")?;
    tracing::info!(
        pubkey = %bs58::encode(identity.pubkey_bytes()).into_string(),
        home = %home.display(),
        "operator identity ready — earnings and reputation accrue against this pubkey"
    );

    let coordinator_url = required("COVENANT_COMPUTE_COORDINATOR_URL")?;
    let coordinator_pubkey_b58 = required("COVENANT_COMPUTE_COORDINATOR_PUBKEY")?;
    // Refuse a malformed coordinator key before any work is taken: it is
    // the pinned anchor every escrow hold verifies against, so a typo
    // (dropped char, wrong byte length) admits nothing — the node wins
    // matches and then faults its own standing rejecting every one. The
    // wizard validates on entry; this covers a hand-written node.env.
    covenant_compute_protocol::validate_address_b58("coordinator pubkey", &coordinator_pubkey_b58)
        .map_err(|e| {
            anyhow::anyhow!("COVENANT_COMPUTE_COORDINATOR_PUBKEY: {e} — fix it or re-run `setup`")
        })?;
    let payout_address = required("COVENANT_COMPUTE_PAYOUT_ADDRESS")?;
    // Refuse a malformed payout address before any work is taken: past
    // this point the first place it can fail is the payout push —
    // earnings for jobs already served, refused by the rail and
    // retried forever by a sweep that can't fix a typo. The wizard
    // validates on entry; this covers a hand-written node.env.
    covenant_compute_protocol::validate_address_b58("payout address", &payout_address).map_err(
        |e| anyhow::anyhow!("COVENANT_COMPUTE_PAYOUT_ADDRESS: {e} — fix it or re-run `setup`"),
    )?;

    let executor_kind = std::env::var("COVENANT_COMPUTE_NODE_EXECUTOR")
        .unwrap_or_else(|_| "subprocess".into())
        .trim()
        .to_ascii_lowercase();

    let ollama_url = std::env::var("COVENANT_COMPUTE_OLLAMA_URL")
        .unwrap_or_else(|_| ollama::DEFAULT_OLLAMA_URL.into());
    let openai_url = std::env::var("COVENANT_COMPUTE_OPENAI_URL")
        .unwrap_or_else(|_| openai_compat::DEFAULT_OPENAI_COMPAT_URL.into());
    let openai_api_key = std::env::var("COVENANT_COMPUTE_OPENAI_API_KEY")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let models_served = resolve_models_served(&executor_kind).await?;
    let default_job_kinds = default_kinds(&executor_kind);

    let price_micro_usdc = env_or(
        "COVENANT_COMPUTE_NODE_PRICE_MICRO_USDC",
        covenant_compute_node::DEFAULT_PRICE_MICRO_USDC,
    );
    // A zero ask is the cheapest possible supply, so it wins matches — and
    // because an unpriced buyer call resolves its offer to the cheapest
    // ask, it serves those for nothing. The wizard refuses 0 on entry;
    // refuse it here too so a hand-written node.env can't serve for free.
    anyhow::ensure!(
        price_micro_usdc > 0,
        "COVENANT_COMPUTE_NODE_PRICE_MICRO_USDC must be at least 1 — a node asking 0 serves \
         every job for free; set a real price or re-run `setup`"
    );
    let job_kinds = parse_job_kinds(
        &std::env::var("COVENANT_COMPUTE_NODE_JOB_KINDS")
            .unwrap_or_else(|_| default_job_kinds.into()),
    );
    // Never advertise a capability this backend can't actually serve: a
    // matched job it can't run is mis-executed, not refused, and the
    // buyer is billed for the garbage. Refuse at boot, before a single
    // such job can be matched. (The defaults are always coherent; this
    // only ever catches a hand-set COVENANT_COMPUTE_NODE_JOB_KINDS.)
    if let Some(servable) = servable_kinds(&executor_kind) {
        if let Some(bad) = job_kinds.iter().find(|k| !servable.contains(k)) {
            anyhow::bail!(
                "the {executor_kind} executor can't serve {}: it would mis-execute a matched job \
                 and still bill the buyer. Set COVENANT_COMPUTE_NODE_JOB_KINDS to {} or change \
                 COVENANT_COMPUTE_NODE_EXECUTOR.",
                kind_label(*bad),
                servable
                    .iter()
                    .map(|k| kind_label(*k))
                    .collect::<Vec<_>>()
                    .join(",")
            );
        }
    }
    // A whisper or say node loads one local model file and serves it under
    // a single advertised name (resolve_models_served: "exactly one local
    // model file"); the executor stamps `models_served.first()` and runs
    // that one model for every job, never reading the requested model_id.
    // Advertising several would let a buyer pin — and be matched and billed
    // on an `Ok` receipt for — a name the node silently answers with the
    // wrong model, the one mis-serve the refund path can't catch because it
    // isn't a fault. Refuse a hand-set COVENANT_COMPUTE_NODE_MODELS that
    // names more than one, the model twin of the job-kind check above. (The
    // defaults are a single model, so this only ever catches a hand-set list.)
    if serves_single_model(&executor_kind) && models_served.len() > 1 {
        anyhow::bail!(
            "the {executor_kind} executor serves exactly one model, but \
             COVENANT_COMPUTE_NODE_MODELS names {}: {}. It would advertise them all, \
             then serve every job with the first ({}) and still bill the buyer. Name \
             a single model, or unset COVENANT_COMPUTE_NODE_MODELS for the default.",
            models_served.len(),
            models_served.join(", "),
            models_served
                .first()
                .map(String::as_str)
                .unwrap_or_default(),
        );
    }
    let (hardware, vram_gb) = resolve_hardware().await;
    let profile = CapabilityProfile {
        operator: identity.agent_id(),
        hardware,
        vram_gb,
        models_served,
        job_kinds,
        price: PriceAsk {
            unit: parse_price_unit(
                &std::env::var("COVENANT_COMPUTE_NODE_PRICE_UNIT")
                    .unwrap_or_else(|_| "per_job".into()),
            ),
            micro_usdc: price_micro_usdc,
        },
        tee_capable: false,
    };
    // A lease settles by meter over its window, and only a per-hour ask is
    // scaled to that window (a per-job ask floors flat on it). A lease node
    // priced per-GPU-second or per-million-tokens would clear a buyer's
    // offer at a fraction of its rate and be metered the full window —
    // underpaid, with no downstream gate to catch it. Refuse it at boot,
    // the model twin of the job-kind and single-model checks above; the
    // coordinator refuses the same defect at registration.
    if let Err(e) = profile.validate_lease_pricing() {
        anyhow::bail!(
            "{e}. Set COVENANT_COMPUTE_NODE_PRICE_UNIT to per_lease_hour (or per_job) for a \
             lease-serving node."
        );
    }
    tracing::info!(
        hardware = ?profile.hardware,
        vram_gb = profile.vram_gb,
        models = ?profile.models_served,
        job_kinds = ?profile.job_kinds,
        price_micro_usdc,
        "capability profile declared"
    );

    // Built before the executor so a broker's lease executor can watch it
    // for the buyer's close: a lease ends when the buyer says stop, and
    // nothing pushes to a node that only ever dials out, so the executor
    // polls this same client for the close the coordinator recorded.
    let client = Arc::new(HttpCoordinatorClient::new(coordinator_url.clone()));

    let executor = match executor_kind.as_str() {
        "echo" => {
            tracing::warn!("executor: echo (returns job input verbatim — smoke tests only)");
            NodeExecutor::Echo(EchoExecutor)
        }
        "ollama" => {
            let default_model = std::env::var("COVENANT_COMPUTE_OLLAMA_DEFAULT_MODEL").ok();
            let keep_alive = std::env::var("COVENANT_COMPUTE_OLLAMA_KEEP_ALIVE").ok();
            tracing::info!(
                url = %ollama_url,
                default_model = ?default_model,
                keep_alive = ?keep_alive,
                "executor: ollama (real inference, token-metered)"
            );
            NodeExecutor::Ollama(
                OllamaExecutor::new(ollama_url.clone(), default_model)
                    .require_models(profile.models_served.clone())
                    .keep_alive(keep_alive),
            )
        }
        "openai-compat" => {
            let default_model = std::env::var("COVENANT_COMPUTE_OPENAI_DEFAULT_MODEL").ok();
            tracing::info!(
                url = %openai_url,
                default_model = ?default_model,
                authenticated = openai_api_key.is_some(),
                "executor: openai-compat (real inference, token-metered)"
            );
            NodeExecutor::OpenAiCompat(
                OpenAiCompatExecutor::new(
                    openai_url.clone(),
                    openai_api_key.clone(),
                    default_model,
                )
                .require_models(profile.models_served.clone()),
            )
        }
        "whisper" => {
            let model_path = std::env::var("COVENANT_COMPUTE_WHISPER_MODEL")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .context(
                    "COVENANT_COMPUTE_WHISPER_MODEL must be set when \
                     COVENANT_COMPUTE_NODE_EXECUTOR=whisper — point it at a whisper.cpp ggml \
                     model file (e.g. ggml-base.en.bin)",
                )?;
            anyhow::ensure!(
                std::path::Path::new(&model_path).is_file(),
                "COVENANT_COMPUTE_WHISPER_MODEL={model_path} is not a readable file; download a \
                 model first (e.g. whisper.cpp's download-ggml-model.sh base.en)"
            );
            let binary = std::env::var("COVENANT_COMPUTE_WHISPER_BIN")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| DEFAULT_WHISPER_BIN.into());
            let model_name = profile
                .models_served
                .first()
                .cloned()
                .unwrap_or_else(|| "whisper-1".into());
            tracing::info!(
                binary = %binary,
                model_path = %model_path,
                model_name = %model_name,
                "executor: whisper (local speech-to-text, envelope-metered)"
            );
            NodeExecutor::Whisper(WhisperExecutor::new(
                binary,
                model_path,
                model_name,
                Arc::new(SubprocessTracker::new()),
                Duration::from_secs(2),
            ))
        }
        "say" => {
            let binary = std::env::var("COVENANT_COMPUTE_SAY_BIN")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| DEFAULT_SAY_BIN.into());
            let default_voice = std::env::var("COVENANT_COMPUTE_SAY_VOICE")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty());
            let model_name = profile
                .models_served
                .first()
                .cloned()
                .unwrap_or_else(|| "say-1".into());
            tracing::info!(
                binary = %binary,
                model_name = %model_name,
                default_voice = ?default_voice,
                "executor: say (local text-to-speech, envelope-metered)"
            );
            NodeExecutor::Say(SayExecutor::new(
                binary,
                model_name,
                default_voice,
                Arc::new(SubprocessTracker::new()),
                Duration::from_secs(2),
            ))
        }
        "container" => {
            let image = std::env::var("COVENANT_COMPUTE_NODE_CONTAINER_IMAGE")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .context(
                    "COVENANT_COMPUTE_NODE_CONTAINER_IMAGE must be set when \
                     COVENANT_COMPUTE_NODE_EXECUTOR=container — which rootfs to trust \
                     is an operator decision with no safe default",
                )?;
            let env_opt = |key: &str| {
                std::env::var(key)
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
            };
            let config = ContainerConfig {
                runtime_binary: env_opt("COVENANT_COMPUTE_NODE_CONTAINER_RUNTIME")
                    .unwrap_or_else(|| "docker".into()),
                image,
                oci_runtime: env_opt("COVENANT_COMPUTE_NODE_CONTAINER_OCI_RUNTIME"),
                gpus: env_opt("COVENANT_COMPUTE_NODE_CONTAINER_GPUS"),
                network: env_opt("COVENANT_COMPUTE_NODE_CONTAINER_NETWORK")
                    .unwrap_or_else(|| "none".into()),
                memory: env_opt("COVENANT_COMPUTE_NODE_CONTAINER_MEMORY")
                    .unwrap_or_else(|| "512m".into()),
                cpus: env_opt("COVENANT_COMPUTE_NODE_CONTAINER_CPUS").unwrap_or_else(|| "1".into()),
                pids_limit: env_or("COVENANT_COMPUTE_NODE_CONTAINER_PIDS", 256u32),
                user: env_opt("COVENANT_COMPUTE_NODE_CONTAINER_USER"),
                // Container names carry the operator, so two nodes on
                // one engine never sweep each other's jobs at boot.
                instance_tag: identity
                    .agent_id()
                    .pubkey_base58()
                    .chars()
                    .take(8)
                    .collect(),
            };
            tracing::info!(
                runtime = %config.runtime_binary,
                image = %config.image,
                network = %config.network,
                oci_runtime = ?config.oci_runtime,
                "executor: container (namespace-isolated, default-deny egress)"
            );
            if gpu_advertised_but_not_passed(&profile.hardware, config.gpus.as_deref()) {
                tracing::warn!(
                    "advertising {} but COVENANT_COMPUTE_NODE_CONTAINER_GPUS is unset — GPU \
                     jobs would run CPU-only inside the container; set \
                     COVENANT_COMPUTE_NODE_CONTAINER_GPUS=all to pass the device (or re-run \
                     setup)",
                    describe_hardware(&profile.hardware, profile.vram_gb)
                );
            }
            let container = ContainerJobExecutor::new(
                config,
                Arc::new(SubprocessTracker::new()),
                Duration::from_secs(2),
            );
            // A previous instance that crashed mid-job left its
            // container running — the engine doesn't stop it when the
            // node dies. Sweep before serving anything new.
            let reaped = container.reap_orphans().await;
            if reaped > 0 {
                tracing::warn!(reaped, "killed orphaned job containers from a previous run");
            }
            // Warm the image before serving so the first job does not pay
            // the pull latency inside its own deadline and fault on it.
            container.ensure_image_present().await;
            NodeExecutor::Container(container)
        }
        "subprocess" => {
            tracing::info!("executor: subprocess (sh -c, tracked, hard-preempted at deadline)");
            // The env is scrubbed, the cwd is a scratch dir and output
            // is capped — but there is no filesystem or network wall
            // around the child. Fine for a trusted-local rig; say it
            // loudly when this node is offering to run strangers' work.
            if profile.job_kinds.contains(&JobKind::BatchJob) {
                tracing::warn!(
                    "serving batch_job with the subprocess executor: buyers' commands run \
                     directly on this host with no filesystem or network isolation — set \
                     COVENANT_COMPUTE_NODE_EXECUTOR=container to put strangers' work \
                     behind container walls"
                );
            }
            NodeExecutor::Subprocess(SubprocessJobExecutor::new(
                Arc::new(SubprocessTracker::new()),
                Duration::from_secs(2),
            ))
        }
        "broker" => {
            // A broker owns no hardware; it rents a real GPU per session
            // from a cloud market, so it needs a funded market account. No
            // key configured means no supply to broker — refuse to register
            // a capability this node cannot serve. This is what keeps the
            // broker off by default: the executor runs only when the
            // operator names it, and even then only with a real account.
            let vast = covenant_compute_vast::VastClient::from_environment()
                .context("read the GPU-market configuration for the broker executor")?
                .context(
                    "the broker executor rents GPUs per lease from a cloud market and needs a \
                     funded account: set COVENANT_VAST_API_KEY (or COVENANT_VAST_API_KEY_FILE). \
                     None is configured, so this node has no supply to broker.",
                )?;
            let image = std::env::var("COVENANT_COMPUTE_BROKER_IMAGE")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .context(
                    "COVENANT_COMPUTE_BROKER_IMAGE must be set when \
                     COVENANT_COMPUTE_NODE_EXECUTOR=broker — the digest-pinned image every \
                     rented session runs (e.g. docker.io/nvidia/cuda@sha256:...). Pin by \
                     digest so the machine a buyer gets is the machine that was measured.",
                )?;
            // One spend ceiling, the market client's own:
            // COVENANT_VAST_MAX_HOURLY_MICROS bounds every offer surveyed
            // and every instance launched. Bounds the operator's loss on a
            // single lease independently of what the buyer paid.
            let config = BrokerConfig {
                image,
                max_hourly_micros: vast.config().max_hourly_micros,
                ready_timeout: Duration::from_secs(env_or(
                    "COVENANT_COMPUTE_BROKER_READY_TIMEOUT_SECS",
                    READY_TIMEOUT.as_secs(),
                )),
                ready_poll_interval: Duration::from_secs(
                    env_or(
                        "COVENANT_COMPUTE_BROKER_READY_POLL_SECS",
                        READY_POLL_INTERVAL.as_secs(),
                    )
                    .max(1),
                ),
            };
            tracing::info!(
                image = %config.image,
                max_hourly_micros = config.max_hourly_micros,
                ready_timeout_secs = config.ready_timeout.as_secs(),
                "executor: broker (rents a real GPU per lease from a cloud market)"
            );
            let backend = Arc::new(BrokerSessionBackend::new(Arc::new(vast), config));
            // The lease executor watches the coordinator for the buyer's
            // close and destroys the rented machine the moment it lands —
            // the buyer is billed by the second until then.
            NodeExecutor::Lease(
                LeaseExecutor::new(backend, LeaseControl::new()).watching(client.clone()),
            )
        }
        "lease-stub" => {
            // A stub broker for smoke tests: it rents no machine and points
            // the buyer at a placeholder endpoint, so the whole lease
            // lifecycle — access grant, live meter, buyer close, metered
            // settlement — runs end to end without a cloud account. The
            // meter and settlement are real; only the machine is not, and
            // the access grant says as much. The lease sibling of `echo`,
            // gated the same way: it runs only when the operator names it.
            let endpoint = std::env::var("COVENANT_COMPUTE_LEASE_STUB_ENDPOINT")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "stub://no-machine".into());
            tracing::warn!(
                %endpoint,
                "executor: lease-stub (holds a lease open behind a placeholder endpoint, \
                 rents no machine — smoke tests only)"
            );
            let backend = Arc::new(StubSessionBackend::new(endpoint));
            NodeExecutor::Lease(
                LeaseExecutor::new(backend, LeaseControl::new()).watching(client.clone()),
            )
        }
        // A typo must not silently run strangers' code with LESS
        // isolation than the operator configured.
        other => anyhow::bail!(
            "COVENANT_COMPUTE_NODE_EXECUTOR must be one of \
             echo|subprocess|container|ollama|openai-compat|whisper|say|broker|lease-stub, \
             got {other:?}"
        ),
    };

    let audit = Arc::new(
        JsonlAuditLog::open(home.join("audit.jsonl"))
            .await
            .context("open node audit log")?,
    );

    // B5: prove every profile claim through the real executor before
    // registering it. A deterministic backend answers a nonce
    // round-trip (a true known answer); a model-serving backend must
    // load and generate from each declared model. Probes land in the
    // node's own audit chain either way, and any failure stops
    // registration — the network never sees a claim this node couldn't
    // demonstrate to itself.
    if std::env::var("COVENANT_COMPUTE_NODE_SKIP_BENCHMARK").as_deref() == Ok("1") {
        tracing::warn!(
            "capability benchmark SKIPPED — this node registers claims it has not demonstrated"
        );
    } else if executor_kind == "broker" {
        // A lease has no known-answer self-test, and the only way to
        // demonstrate one at boot would be to rent a real machine —
        // spending the operator's money before a single buyer has paid.
        // The market account and the pinned image were validated when the
        // executor was built; a broker's capacity is proven per lease, when
        // a buyer pays for it, not at register.
        tracing::info!(
            "broker executor: no capability benchmark — a lease is proven per rental, not \
             by renting a machine at boot"
        );
    } else if executor_kind == "lease-stub" {
        // Nothing to benchmark: the stub rents no machine, so there is no
        // capability to demonstrate. The build-time warning already told
        // the operator the endpoint is a placeholder.
        tracing::info!("lease-stub executor: no capability benchmark — the stub rents no machine");
    } else {
        let nonce = format!("compute-benchmark-{}", epoch_ms());
        let specs = benchmark_specs(&executor_kind, &profile.job_kinds, nonce);
        let per_probe_timeout = Duration::from_secs(env_or(
            "COVENANT_COMPUTE_BENCHMARK_TIMEOUT_SECS",
            // Generous: a cold model load on consumer hardware is slow,
            // and a slow pass beats a false failure at boot.
            120u64,
        ));
        let probe_count: usize = specs
            .iter()
            .map(|spec| {
                if spec.per_model {
                    profile.models_served.len().max(1)
                } else {
                    1
                }
            })
            .sum();
        tracing::info!(
            probes = probe_count,
            claims = specs.len(),
            timeout_secs = per_probe_timeout.as_secs(),
            "benchmarking the declared capability profile against the real executor"
        );
        let mut failures: Vec<String> = Vec::new();
        let mut total_probes = 0usize;
        for spec in &specs {
            let probes = run_benchmark(
                &executor,
                &identity.agent_id(),
                &profile.models_served,
                spec,
                per_probe_timeout,
            )
            .await;
            for probe in &probes {
                total_probes += 1;
                let (passed, reason, stats) = match &probe.result {
                    Ok(stats) => (true, "ok".to_string(), Some(stats.clone())),
                    Err(reason) => (false, reason.clone(), None),
                };
                if passed {
                    tracing::info!(
                        kind = kind_label(spec.kind),
                        model = probe.model_id.as_deref().unwrap_or("-"),
                        wall_ms = stats.as_ref().map(|s| s.wall_ms).unwrap_or(0),
                        tokens_out = ?stats.as_ref().and_then(|s| s.tokens_out),
                        "capability claim demonstrated"
                    );
                } else {
                    tracing::error!(
                        kind = kind_label(spec.kind),
                        model = probe.model_id.as_deref().unwrap_or("-"),
                        %reason,
                        "capability claim FAILED its benchmark"
                    );
                }
                if let Err(e) = audit
                    .record(AuditEvent {
                        id: uuid::Uuid::new_v4(),
                        timestamp_ms: epoch_ms(),
                        issuer: identity.agent_id(),
                        kind: AuditKind::ComputeCapabilityBenchmarked {
                            operator_pubkey_b58: identity.agent_id().pubkey_base58(),
                            model_id: probe.model_id.clone(),
                            passed,
                            wall_ms: stats.as_ref().map(|s| s.wall_ms).unwrap_or(0),
                            tokens_out: stats.as_ref().and_then(|s| s.tokens_out),
                            reason,
                        },
                    })
                    .await
                {
                    tracing::warn!(error = %e, "benchmark audit write failed");
                }
                if let Err(reason) = &probe.result {
                    // Name the kind: one model can now carry both an
                    // inference and an embedding probe, so a bare model id
                    // wouldn't say which claim failed.
                    failures.push(format!(
                        "{} {}: {reason}",
                        kind_label(spec.kind),
                        probe.model_id.as_deref().unwrap_or("(unpinned)")
                    ));
                }
            }
        }
        anyhow::ensure!(
            failures.is_empty(),
            "refusing to register a profile this node cannot serve. {} of {} claims failed:\n  {}\n\
             re-run `covenant-compute-node setup` and drop any model that can't serve its \
             declared job kind (an embedding model under an inference profile can't generate; a \
             chat model that won't load), or set COVENANT_COMPUTE_NODE_SKIP_BENCHMARK=1 to \
             register the unproven claims anyway",
            failures.len(),
            total_probes,
            failures.join("\n  ")
        );
    }

    let max_fee_bps: Option<u32> = std::env::var("COVENANT_COMPUTE_NODE_MAX_FEE_BPS")
        .ok()
        .map(|v| {
            v.trim().parse::<u32>().map_err(|_| {
                anyhow::anyhow!(
                    "COVENANT_COMPUTE_NODE_MAX_FEE_BPS must be whole basis points \
                     (e.g. 250 for 2.5%), got {v:?}"
                )
            })
        })
        .transpose()?;
    // Partner attribution (C8): signed into the registration so the
    // partner who onboarded this operator earns their share of the
    // marketplace fee on every job it serves.
    let referral_code = std::env::var("COVENANT_COMPUTE_REFERRAL_CODE")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    if let Some(code) = &referral_code {
        tracing::info!(code, "registering under a partner referral code");
    }
    let fee_bps = register_until_accepted(
        &client,
        &profile,
        &payout_address,
        referral_code.as_deref(),
        &identity,
        max_fee_bps,
    )
    .await?;
    let earnings = Arc::new(
        JsonlEarningsLedger::open(&home.join("earnings.jsonl"))
            .map_err(|e| anyhow::anyhow!("open earnings ledger: {e}"))?,
    );
    tracing::info!(
        credited_jobs = earnings.recent(usize::MAX).await.len(),
        unpaid_micro_usdc = earnings.unpaid_total_micro_usdc().await,
        "earnings ledger restored (run with `earnings` argument to inspect)"
    );
    let outbox = Arc::new(
        covenant_compute_node::ResultOutbox::open(&home.join("outbox.jsonl"))
            .map_err(|e| anyhow::anyhow!("open result outbox: {e}"))?,
    );
    let queued = outbox.pending().len();
    if queued > 0 {
        tracing::info!(
            queued,
            "undelivered results restored; redelivery starts with the serve loop"
        );
    }
    let accepted = Arc::new(
        covenant_compute_node::AcceptedBook::open(&home.join("accepted.jsonl"))
            .map_err(|e| anyhow::anyhow!("open accepted book: {e}"))?,
    );
    let interrupted = accepted.pending().len();
    if interrupted > 0 {
        tracing::info!(
            interrupted,
            "accepted jobs from a previous run restored; recovery runs before the serve loop"
        );
    }
    let node = Arc::new(
        Node::new(
            identity,
            profile.clone(),
            client.clone(),
            Arc::new(executor),
            earnings.clone(),
            audit,
            NodeConfig {
                coordinator_pubkey_b58,
                max_in_flight: env_or("COVENANT_COMPUTE_NODE_MAX_IN_FLIGHT", 2usize),
                preempt_grace: Duration::from_secs(2),
                fee_bps,
            },
        )
        .with_outbox(outbox)
        .with_accepted_book(accepted),
    );

    // Heartbeat: liveness + honest queue depth + backend health (a
    // node whose model server is down declares Offline, so the matcher
    // routes around it). A protocol-level error usually means the
    // (in-memory) coordinator restarted and forgot this node —
    // re-register and carry on, no operator action needed.
    // Floor at 1s: a heartbeat can't be turned off — a node that stops
    // heartbeating goes stale in the registry and matches nothing — so a
    // `0` is a mistake, and taken literally it would sleep zero and spin
    // the loop into a signing-and-POST storm. The retry interval below
    // floors the same way.
    let heartbeat_interval =
        Duration::from_secs(env_or("COVENANT_COMPUTE_NODE_HEARTBEAT_SECS", 15u64).max(1));
    let hb_client = client.clone();
    let hb_node = node.clone();
    let hb_profile = profile.clone();
    let hb_payout = payout_address.clone();
    let hb_referral = referral_code.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(heartbeat_interval).await;
            let in_flight = hb_node.in_flight();
            let status = hb_node.current_status();
            let req = match HeartbeatRequest::sign(
                heartbeat_identity.agent_id(),
                status,
                in_flight as u32,
                epoch_ms(),
                &heartbeat_identity,
            ) {
                Ok(req) => req,
                Err(e) => {
                    tracing::error!(error = %e, "heartbeat signing failed");
                    continue;
                }
            };
            match hb_client.heartbeat(req).await {
                Ok(_) => {}
                Err(covenant_compute_node::CoordinatorError::Protocol(msg)) => {
                    tracing::warn!(%msg, "heartbeat rejected; re-registering");
                    // Ceiling = the fee accepted at boot: a coordinator
                    // that raised its take across a restart is refused
                    // (this loop keeps erroring, no new jobs arrive) —
                    // accepting the new rate is an operator decision,
                    // made by restarting the node.
                    if let Err(e) = register_until_accepted(
                        &hb_client,
                        &hb_profile,
                        &hb_payout,
                        hb_referral.as_deref(),
                        &heartbeat_identity,
                        Some(fee_bps),
                    )
                    .await
                    {
                        tracing::error!(error = %e, "re-registration failed");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "heartbeat transport failed"),
            }
        }
    });

    // B4: reconcile the local ledger against the coordinator's payout
    // confirmations. Every credited job starts Unpaid; when the
    // operator books report the push that settled it, the entry flips
    // to Paid with the reported transaction signature. Runs once at
    // boot (catching payouts that landed while the node was down),
    // then on the interval. 0 disables.
    let reconcile_secs = env_or("COVENANT_COMPUTE_NODE_PAYOUT_POLL_SECS", 60u64);
    if reconcile_secs > 0 {
        let rec_client = client.clone();
        let rec_earnings = earnings.clone();
        let rec_identity =
            LocalIdentity::load_or_create(&home.join("identity.json"), "operator@compute")
                .context("reload operator identity for payout reconcile")?;
        tokio::spawn(async move {
            loop {
                match rec_client.operator_jobs(&rec_identity).await {
                    Ok(rows) => {
                        reconcile_paid_rows(rec_earnings.as_ref(), &rows, epoch_ms()).await;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "payout reconcile fetch failed; will retry")
                    }
                }
                tokio::time::sleep(Duration::from_secs(reconcile_secs)).await;
            }
        });
    } else {
        tracing::warn!(
            "payout reconcile disabled (COVENANT_COMPUTE_NODE_PAYOUT_POLL_SECS=0) — \
             earnings will stay Unpaid however many payouts land"
        );
    }

    tracing::info!(coordinator = %coordinator_url, "serving jobs — ctrl-c to stop");
    let backend_retry =
        Duration::from_secs(env_or("COVENANT_COMPUTE_NODE_BACKEND_RETRY_SECS", 5u64).max(1));
    let serve_node = node.clone();
    let serve = async move {
        let mut boot_recovery_done = false;
        loop {
            // Redeliver anything a dead coordinator left queued — a
            // no-op on the empty outbox, so it costs nothing on the
            // normal path. Runs before the drain check so the last
            // job's result never exits with the process.
            serve_node.drain_outbox().await;
            if serve_node.draining() {
                break;
            }
            // Never take work the backend can't serve: a dead model
            // server pauses intake right here (and reports Offline)
            // instead of faulting every job the matcher keeps sending.
            serve_node.wait_for_backend(backend_retry).await;
            // The gate also releases on drain — re-check before
            // touching recovery or new work.
            if serve_node.draining() {
                break;
            }
            if !boot_recovery_done {
                boot_recovery_done = true;
                // First healthy pass only: re-serve whatever a previous
                // life accepted and never finished, before any new work
                // is polled — run later it would execute this life's
                // in-flight jobs a second time.
                let recovered = serve_node.recover_accepted().await;
                if recovered > 0 {
                    tracing::info!(recovered, "interrupted jobs re-served");
                }
            }
            let polled_at = std::time::Instant::now();
            match serve_node.run_once().await {
                Ok(Some(outcome)) => {
                    let unpaid = serve_node.earnings.unpaid_total_micro_usdc().await;
                    tracing::info!(
                        job_id = %outcome.job_id,
                        status = ?outcome.receipt.receipt.status,
                        wall_ms = outcome.receipt.receipt.meter.wall_ms,
                        unpaid_micro_usdc = unpaid,
                        "job served"
                    );
                }
                // Long-poll timed out with no work — poll again. A healthy
                // coordinator holds this open ~30s, but one that answers
                // `null` at once — an older build, a reverse proxy that
                // buffers the hanging GET — would spin this into a request
                // storm against both `next-job` and the backend health
                // probe. Floor the empty-poll cadence: a real long-poll
                // hold already outran the floor and waits not at all,
                // while an immediate `null` costs one short sleep.
                Ok(None) => {
                    const EMPTY_POLL_FLOOR: Duration = Duration::from_secs(1);
                    if let Some(rest) = EMPTY_POLL_FLOOR.checked_sub(polled_at.elapsed()) {
                        tokio::time::sleep(rest).await;
                    }
                }
                Err(NodeError::Admission(e)) => {
                    tracing::warn!(error = %e, "job rejected at admission");
                }
                Err(NodeError::Coordinator(e)) => {
                    tracing::warn!(error = %e, "coordinator error; backing off");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                Err(e) => {
                    tracing::error!(error = %e, "job loop error; backing off");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    };

    let mut serve = std::pin::pin!(serve);
    tokio::select! {
        _ = &mut serve => {}
        _ = shutdown_signal() => {
            tracing::info!(
                in_flight = node.in_flight(),
                "drain requested: finishing in-flight work, taking nothing new \
                 (signal again to exit immediately)"
            );
            // The drain's Offline beat makes the coordinator re-match
            // whatever still sits in this node's queue right now, while
            // anything mid-execution runs to completion here.
            node.begin_drain().await;
            tokio::select! {
                _ = &mut serve => {
                    tracing::info!("drained clean");
                }
                _ = shutdown_signal() => {
                    tracing::info!(
                        "immediate shutdown; interrupted jobs re-serve on the next boot"
                    );
                }
            }
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_compute_protocol::RegisterResponse;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn parse_price_unit_maps_known_units_and_falls_back_on_the_rest() {
        assert_eq!(
            parse_price_unit("per_million_tokens"),
            PriceUnit::PerMillionTokens
        );
        assert_eq!(parse_price_unit("per_gpu_second"), PriceUnit::PerGpuSecond);
        assert_eq!(parse_price_unit("per_lease_hour"), PriceUnit::PerLeaseHour);
        assert_eq!(parse_price_unit(" per_job "), PriceUnit::PerJob);
        // Unset (the caller's default) and blank stay per_job silently.
        assert_eq!(parse_price_unit("per_job"), PriceUnit::PerJob);
        assert_eq!(parse_price_unit(""), PriceUnit::PerJob);
        // A mistype falls back to per_job rather than refusing to boot.
        assert_eq!(parse_price_unit("per_gpu_hour"), PriceUnit::PerJob);
        assert_eq!(parse_price_unit("per_token"), PriceUnit::PerJob);
    }

    #[test]
    fn parse_explicit_models_refuses_a_set_but_empty_list() {
        assert_eq!(
            parse_explicit_models("qwen2.5:7b, llama3").unwrap(),
            vec!["qwen2.5:7b".to_string(), "llama3".to_string()]
        );
        // Stray separators trim to a real single model, not an empty list.
        assert_eq!(
            parse_explicit_models(" , llama3 ,").unwrap(),
            vec!["llama3".to_string()]
        );
        // Blank or separator-only: refused, so a model-serving executor
        // never benchmarks zero claims and registers them anyway.
        for empty in ["", "   ", ",", " , , "] {
            let err = parse_explicit_models(empty).unwrap_err().to_string();
            assert!(err.contains("names no model"), "{empty:?}: {err}");
        }
    }

    #[test]
    fn resolve_hardware_decision_treats_vram_as_an_independent_override() {
        let gpu = || HardwareClass::ConsumerGpu {
            model: "RTX 4090".into(),
        };

        // A set class is authoritative and skips detection; VRAM overrides
        // its width, or 0 when unset.
        assert_eq!(
            resolve_hardware_decision(Some("consumer:RTX 4090"), Some(24), None),
            (gpu(), 24)
        );
        assert_eq!(
            resolve_hardware_decision(Some("cpu"), None, None),
            (HardwareClass::CpuOnly, 0)
        );

        // Class unset: the detected GPU stands. A lone VRAM_GB overrides
        // the detected width instead of suppressing detection and leaving
        // the box CPU-only — the fix.
        assert_eq!(
            resolve_hardware_decision(None, None, Some((gpu(), 24))),
            (gpu(), 24)
        );
        assert_eq!(
            resolve_hardware_decision(None, Some(16), Some((gpu(), 24))),
            (gpu(), 16)
        );

        // Class unset, no GPU found: CPU-only, width override still honored.
        assert_eq!(
            resolve_hardware_decision(None, None, None),
            (HardwareClass::CpuOnly, 0)
        );
        assert_eq!(
            resolve_hardware_decision(None, Some(8), None),
            (HardwareClass::CpuOnly, 8)
        );
    }

    #[test]
    fn benchmark_specs_prove_each_declared_capability() {
        let nonce = "compute-benchmark-1".to_string();

        // Embedding-only model node: one embedding probe, per model, so an
        // embedding model proves itself instead of failing a chat probe it
        // can't answer.
        let embed = benchmark_specs("ollama", &[JobKind::Embedding], nonce.clone());
        assert_eq!(embed.len(), 1);
        assert_eq!(embed[0].kind, JobKind::Embedding);
        assert!(embed[0].per_model);
        assert!(embed[0].expect_contains.is_none());

        // Inference-only node (the default): one generative chat probe.
        let infer = benchmark_specs("openai-compat", &[JobKind::InferenceCall], nonce.clone());
        assert_eq!(infer.len(), 1);
        assert_eq!(infer[0].kind, JobKind::InferenceCall);
        assert!(infer[0].per_model);

        // ollama serves embeddings from the model it chats with, so a
        // both-kinds ollama node proves the embed path with its inference
        // probe alone.
        let ollama_both = benchmark_specs(
            "ollama",
            &[JobKind::InferenceCall, JobKind::Embedding],
            nonce.clone(),
        );
        assert_eq!(
            ollama_both.iter().map(|s| s.kind).collect::<Vec<_>>(),
            vec![JobKind::InferenceCall]
        );

        // An openai-compat chat server can reject `/v1/embeddings`, so a
        // both-kinds node there proves each claim on its own probe rather
        // than registering an embedding claim it never demonstrated.
        let openai_both = benchmark_specs(
            "openai-compat",
            &[JobKind::InferenceCall, JobKind::Embedding],
            nonce.clone(),
        );
        assert_eq!(
            openai_both.iter().map(|s| s.kind).collect::<Vec<_>>(),
            vec![JobKind::InferenceCall, JobKind::Embedding]
        );
        assert!(openai_both.iter().all(|s| s.per_model));

        // Deterministic backends keep the nonce round-trip against the
        // declared kind.
        let echo = benchmark_specs("echo", &[JobKind::BatchJob], nonce.clone());
        assert_eq!(echo.len(), 1);
        assert_eq!(echo[0].kind, JobKind::BatchJob);
        assert_eq!(echo[0].expect_contains.as_deref(), Some(nonce.as_str()));
        assert!(!echo[0].per_model);

        let batch = benchmark_specs("subprocess", &[JobKind::BatchJob], nonce.clone());
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].kind, JobKind::BatchJob);
        assert_eq!(batch[0].expect_contains.as_deref(), Some(nonce.as_str()));

        // A whisper node proves the transcription claim with the bundled
        // synthetic clip, judged case-insensitively on the spoken words.
        let whisper = benchmark_specs("whisper", &[JobKind::Transcription], nonce);
        assert_eq!(whisper.len(), 1);
        assert_eq!(whisper[0].kind, JobKind::Transcription);
        assert_eq!(
            whisper[0].expect_contains.as_deref(),
            Some("covenant compute")
        );
        assert!(!whisper[0].per_model);
        assert!(
            !whisper[0].input.is_empty(),
            "the probe carries the audio clip"
        );

        // A say node proves the synthesis claim with a spoken line; the clip
        // is the answer, so there is no text to match.
        let say = benchmark_specs("say", &[JobKind::SpeechSynthesis], "nonce-say".into());
        assert_eq!(say.len(), 1);
        assert_eq!(say[0].kind, JobKind::SpeechSynthesis);
        assert!(say[0].expect_contains.is_none());
        assert!(!say[0].per_model);
        assert!(
            !say[0].input.is_empty(),
            "the probe carries the text to voice"
        );
    }

    /// Binds `servable_kinds` to `benchmark_specs`: every kind a real
    /// backend is allowed to declare must be proven at boot, or the node
    /// registers a claim it never demonstrated — the exact fail-open
    /// benchmark-on-register exists to close. The two maps are edited
    /// apart, so this declares each backend's FULL servable set (what the
    /// coherence check lets an operator do) and confirms the benchmark
    /// covers all of it: a probe of the kind, or the one deliberate fold
    /// where ollama's embedding claim rides its inference probe because
    /// ollama answers `/api/embed` with the model it chats with. Add a
    /// servable kind without a probe (or, for ollama, without extending
    /// the fold) and this fails first. Broker is exempt on purpose: a
    /// lease is proven per paid rental, not by renting a machine at boot.
    #[test]
    fn the_boot_benchmark_covers_every_kind_a_backend_can_declare() {
        let nonce = "compute-benchmark-bind".to_string();
        // The sole documented fold: ollama serves embeddings from the model
        // it chats with, so its inference probe demonstrates the embed path.
        // openai-compat is deliberately absent — a chat server can reject
        // `/v1/embeddings`, so it must prove embeddings on their own probe.
        let folded: &[(&str, JobKind)] = &[("ollama", JobKind::Embedding)];

        // echo and any unknown kind return `None` from `servable_kinds` (no
        // coherence constraint, a dev backend), so they carry no claim to
        // prove and are excluded here by construction.
        for executor_kind in [
            "ollama",
            "openai-compat",
            "subprocess",
            "container",
            "whisper",
            "say",
        ] {
            let servable = servable_kinds(executor_kind)
                .unwrap_or_else(|| panic!("{executor_kind} must have a servable set"));
            let specs = benchmark_specs(executor_kind, servable, nonce.clone());
            assert!(
                !specs.is_empty(),
                "{executor_kind} may serve {servable:?} but the boot benchmark probes nothing"
            );
            for kind in servable {
                let probed = specs.iter().any(|s| s.kind == *kind);
                let folded_in = folded.contains(&(executor_kind, *kind));
                assert!(
                    probed || folded_in,
                    "{executor_kind} may serve {kind:?} but the boot benchmark neither probes it \
                     nor folds it into another probe — it would register unproven"
                );
            }
            // A probe of a kind the backend can't serve proves nothing about
            // what a buyer will actually ask it to run.
            for spec in &specs {
                assert!(
                    servable.contains(&spec.kind),
                    "{executor_kind} benchmarks {:?}, outside its servable set {servable:?}",
                    spec.kind
                );
            }
        }

        // Broker is the one servable backend with no boot benchmark: proving
        // a lease means renting a real machine (spending the operator's money
        // before a buyer pays), so its capacity is proven per rental instead.
        assert_eq!(servable_kinds("broker"), Some(&[JobKind::LeaseSession][..]));
    }

    /// The whole kind-string wiring for a say node, mirroring the whisper
    /// check: a gap in any of these four maps is what would keep a speech
    /// node from booting — its default kind skipped as unknown, fallen back
    /// to batch, and failing the coherence check against `servable_kinds`.
    #[test]
    fn the_say_backend_knows_speech_synthesis_across_the_kind_wiring() {
        assert_eq!(default_kinds("say"), "speech_synthesis");
        assert_eq!(
            parse_job_kinds("speech_synthesis"),
            vec![JobKind::SpeechSynthesis]
        );
        assert_eq!(servable_kinds("say"), Some(&[JobKind::SpeechSynthesis][..]));
        assert_eq!(kind_label(JobKind::SpeechSynthesis), "speech_synthesis");
    }

    /// The whole kind-string wiring for a whisper node, in one place: the
    /// default kind parses back to the variant, the backend serves it, and
    /// it labels round-trip. A gap anywhere here is what kept a whisper
    /// node from booting at all — its default "transcription" was skipped
    /// as an unknown kind, fell back to batch, and failed the coherence
    /// check.
    #[test]
    fn the_whisper_backend_knows_transcription_across_the_kind_wiring() {
        assert_eq!(default_kinds("whisper"), "transcription");
        assert_eq!(
            parse_job_kinds("transcription"),
            vec![JobKind::Transcription]
        );
        assert_eq!(
            servable_kinds("whisper"),
            Some(&[JobKind::Transcription][..])
        );
        assert_eq!(kind_label(JobKind::Transcription), "transcription");
    }

    /// The whole kind-string wiring for a broker node, mirroring the
    /// whisper and say checks: the default kind parses back to the variant,
    /// the backend serves exactly it, and it labels round-trip. A gap
    /// anywhere here is what would keep a broker from booting — its default
    /// "lease_session" skipped as an unknown kind, fallen back to batch, and
    /// failing the coherence check against `servable_kinds`.
    #[test]
    fn the_broker_backend_knows_lease_session_across_the_kind_wiring() {
        assert_eq!(default_kinds("broker"), "lease_session");
        assert_eq!(
            parse_job_kinds("lease_session"),
            vec![JobKind::LeaseSession]
        );
        assert_eq!(servable_kinds("broker"), Some(&[JobKind::LeaseSession][..]));
        assert_eq!(kind_label(JobKind::LeaseSession), "lease_session");
    }

    /// A broker rents whole machines and serves nothing else: a hand-set
    /// COVENANT_COMPUTE_NODE_JOB_KINDS naming any other kind falls outside
    /// `servable_kinds`, so the boot coherence check refuses it before a
    /// mismatched job can be matched and mis-served.
    #[test]
    fn a_broker_serves_only_lease_sessions() {
        let servable = servable_kinds("broker").expect("broker has a servable set");
        assert!(servable.contains(&JobKind::LeaseSession));
        for other in [
            JobKind::InferenceCall,
            JobKind::BatchJob,
            JobKind::Embedding,
            JobKind::Transcription,
            JobKind::SpeechSynthesis,
        ] {
            assert!(
                !servable.contains(&other),
                "a broker rents machines; it must not advertise {other:?}"
            );
        }
    }

    /// `lease-stub` is the hermetic sibling of `broker`: it serves the same
    /// lease sessions through the exact kind wiring, so the whole lifecycle
    /// (access grant, meter, close, metered settlement) can be exercised
    /// without a cloud account. It rents no machine and pins no model, so —
    /// like `broker` — it must not be a fixed-model backend the boot check
    /// would refuse a multi-name model list for.
    #[test]
    fn lease_stub_serves_lease_sessions_like_a_broker() {
        assert_eq!(default_kinds("lease-stub"), "lease_session");
        assert_eq!(
            servable_kinds("lease-stub"),
            Some(&[JobKind::LeaseSession][..])
        );
        assert!(!serves_single_model("lease-stub"));
    }

    /// A whisper or say node loads one local model file and stamps
    /// `models_served.first()` on every job, so a second advertised model is
    /// a name it silently answers with the wrong model — matched and billed
    /// on an `Ok` receipt the buyer can't dispute. `serves_single_model`
    /// marks exactly the fixed-model backends the boot check refuses a
    /// multi-model `COVENANT_COMPUTE_NODE_MODELS` for; the routing and
    /// generic backends carry several names honestly.
    #[test]
    fn only_whisper_and_say_are_single_fixed_model_backends() {
        assert!(serves_single_model("whisper"));
        assert!(serves_single_model("say"));
        for routing in [
            "ollama",
            "openai-compat",
            "subprocess",
            "container",
            "broker",
            "echo",
        ] {
            assert!(
                !serves_single_model(routing),
                "{routing} routes per job or serves any; it is not a fixed single-model backend"
            );
        }
    }

    #[test]
    fn market_position_sole_operator_reads_the_local_ask_when_it_is_the_registered_one() {
        let line = market_position(true, 500, "per_job", 1, 500, PriceUnit::PerJob);
        assert_eq!(
            line,
            "your ask 500 micro-USDC (per_job) is the row's only offer"
        );
    }

    #[test]
    fn market_position_flags_a_local_ask_that_has_not_re_registered() {
        // The serve process registered 800 (what the row and the matcher
        // see); this invocation's config now reads 1000. The line must
        // not claim 1000 is offered — the market still shows 800.
        let line = market_position(true, 1000, "per_job", 1, 800, PriceUnit::PerJob);
        assert!(
            line.contains("registered ask 800 micro-USDC (per_job)"),
            "{line}"
        );
        assert!(line.contains("local config now reads 1000"), "{line}");
        assert!(line.contains("restart"), "{line}");
        assert!(
            !line.contains("your ask 1000 micro-USDC (per_job) is the row's only offer"),
            "the stale local ask is never printed as the live offer: {line}"
        );
    }

    #[test]
    fn market_position_places_a_matchable_ask_against_a_shared_row() {
        assert!(
            market_position(true, 400, "per_job", 3, 400, PriceUnit::PerJob)
                .contains("sets the row's floor")
        );
        let above = market_position(true, 900, "per_job", 3, 400, PriceUnit::PerJob);
        assert!(above.contains("sits above the 400 floor"), "{above}");
        assert!(above.contains("tries cheaper supply first"), "{above}");
    }

    #[test]
    fn market_position_projects_where_an_unmatchable_ask_would_sit() {
        assert!(
            market_position(false, 400, "per_job", 2, 500, PriceUnit::PerJob)
                .contains("would set the row's floor once matchable")
        );
        assert!(
            market_position(false, 900, "per_job", 2, 500, PriceUnit::PerJob)
                .contains("would sit above the 500 floor")
        );
    }

    #[test]
    fn resolve_log_dir_only_redirects_serve_with_a_real_dir() {
        assert_eq!(
            resolve_log_dir(true, Some("/var/compute/logs")),
            Some("/var/compute/logs".to_string())
        );
        // Blank, whitespace or unset keeps serve on the console.
        assert_eq!(resolve_log_dir(true, Some("   ")), None);
        assert_eq!(resolve_log_dir(true, Some("")), None);
        assert_eq!(resolve_log_dir(true, None), None);
        // A one-shot command stays on the console even if the dir is set.
        assert_eq!(resolve_log_dir(false, Some("/var/compute/logs")), None);
        // Surrounding whitespace is trimmed off the path.
        assert_eq!(
            resolve_log_dir(true, Some("  /tmp/l  ")),
            Some("/tmp/l".to_string())
        );
    }

    #[test]
    fn parse_verify_job_reads_the_optional_filter() {
        // No flag verifies the whole ledger.
        assert!(parse_verify_job(&[]).unwrap().is_none());
        // A well-formed id is parsed through.
        let id = uuid::Uuid::new_v4();
        assert_eq!(
            parse_verify_job(&["--job".to_string(), id.to_string()]).unwrap(),
            Some(id)
        );
        // A malformed id, a missing value, and any other flag each fail.
        assert!(parse_verify_job(&["--job".to_string(), "nope".to_string()]).is_err());
        assert!(parse_verify_job(&["--job".to_string()]).is_err());
        assert!(parse_verify_job(&["--since".to_string(), "0".to_string()]).is_err());
    }

    #[test]
    fn rolling_appender_writes_a_dated_file_under_a_created_dir() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        // Nested path the node hasn't created yet — build_rolling_appender
        // makes it, mirroring the launchd agent pointing at {home}/logs.
        let dir = tmp.path().join("logs");
        let mut appender = build_rolling_appender(dir.to_str().unwrap()).unwrap();
        writeln!(appender, "operator identity ready").unwrap();
        appender.flush().unwrap();

        let files: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(files.len(), 1, "one dated file expected, got {files:?}");
        assert!(
            files[0].starts_with("node.") && files[0].ends_with(".log"),
            "unexpected log name {}",
            files[0]
        );
        let body = std::fs::read_to_string(dir.join(&files[0])).unwrap();
        assert!(body.contains("operator identity ready"), "{body}");
    }

    #[test]
    fn describe_hardware_labels_gpu_and_cpu() {
        assert_eq!(
            describe_hardware(
                &HardwareClass::ConsumerGpu {
                    model: "NVIDIA GeForce RTX 4090".into()
                },
                24
            ),
            "NVIDIA GeForce RTX 4090 (24 GB VRAM)"
        );
        assert_eq!(
            describe_hardware(
                &HardwareClass::DatacenterGpu {
                    model: "NVIDIA H100 80GB HBM3".into()
                },
                80
            ),
            "NVIDIA H100 80GB HBM3 (80 GB VRAM)"
        );
        assert_eq!(describe_hardware(&HardwareClass::CpuOnly, 0), "CPU-only");
    }

    #[test]
    fn gpu_container_coherence_warns_only_on_an_undelivered_gpu() {
        let gpu = HardwareClass::ConsumerGpu {
            model: "NVIDIA GeForce RTX 4090".into(),
        };
        // Advertises a GPU, passes no device — the mismatch to warn about.
        assert!(gpu_advertised_but_not_passed(&gpu, None));
        // Device passed: coherent, whether all or a named subset.
        assert!(!gpu_advertised_but_not_passed(&gpu, Some("all")));
        assert!(!gpu_advertised_but_not_passed(&gpu, Some("device=0")));
        // Datacenter parts are GPUs too.
        assert!(gpu_advertised_but_not_passed(
            &HardwareClass::DatacenterGpu {
                model: "NVIDIA H100 80GB HBM3".into()
            },
            None
        ));
        // A CPU-only node passing no device is coherent, not a warning.
        assert!(!gpu_advertised_but_not_passed(
            &HardwareClass::CpuOnly,
            None
        ));
    }

    /// A coordinator that answers registration with a fixed verdict and
    /// disclosed fee, optionally failing the first `flaky_failures`
    /// requests at the transport level (500) — enough surface to tell a
    /// retryable hiccup from a policy refusal.
    async fn spawn_disclosing_coordinator(
        fee_bps: u32,
        accept: bool,
        flaky_failures: u32,
    ) -> String {
        let remaining = Arc::new(AtomicU32::new(flaky_failures));
        let app = axum::Router::new().route(
            "/federation/operators/register",
            axum::routing::post(move || {
                let remaining = remaining.clone();
                async move {
                    if remaining
                        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                        .is_ok()
                    {
                        return Err(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
                    }
                    Ok(axum::Json(RegisterResponse {
                        accepted: accept,
                        operator_session: accept.then(|| "session".into()),
                        reason: (!accept).then(|| "policy: closed pool".into()),
                        fee_bps,
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn test_profile(identity: &LocalIdentity) -> CapabilityProfile {
        CapabilityProfile {
            operator: identity.agent_id(),
            hardware: HardwareClass::CpuOnly,
            vram_gb: 0,
            models_served: vec!["any".into()],
            job_kinds: vec![JobKind::BatchJob],
            price: PriceAsk {
                unit: PriceUnit::PerJob,
                micro_usdc: 100,
            },
            tee_capable: false,
        }
    }

    #[tokio::test]
    async fn a_disclosed_fee_above_the_operator_ceiling_refuses_registration() {
        let base = spawn_disclosing_coordinator(300, true, 0).await;
        let client = HttpCoordinatorClient::new(base);
        let identity = LocalIdentity::generate("op@test");
        let profile = test_profile(&identity);

        // Above the stated tolerance: a terms refusal naming both
        // numbers, not a retry loop that eventually serves anyway.
        let err =
            register_until_accepted(&client, &profile, "payout-addr", None, &identity, Some(250))
                .await
                .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("300 bps"), "names the disclosed fee: {msg}");
        assert!(msg.contains("250 bps"), "names the ceiling: {msg}");

        // Exactly at the ceiling is the operator's stated tolerance.
        let fee =
            register_until_accepted(&client, &profile, "payout-addr", None, &identity, Some(300))
                .await
                .unwrap();
        assert_eq!(fee, 300);

        // No ceiling set: disclose-and-continue.
        let fee = register_until_accepted(&client, &profile, "payout-addr", None, &identity, None)
            .await
            .unwrap();
        assert_eq!(fee, 300);
    }

    #[tokio::test]
    async fn a_confiscatory_disclosed_fee_refuses_registration_even_with_no_ceiling() {
        // A 100%-or-more fee pays the operator zero or less and would
        // underflow the net-of-fee earnings credit. The node refuses it
        // outright, before any ceiling check — that is not a tolerance
        // the operator opts into with COVENANT_COMPUTE_NODE_MAX_FEE_BPS,
        // it is unrepresentable, the same bound the coordinator puts on
        // the value it discloses.
        let identity = LocalIdentity::generate("op@test");
        let profile = test_profile(&identity);

        for fee_bps in [covenant_compute_protocol::MAX_FEE_BPS, 20_000] {
            let base = spawn_disclosing_coordinator(fee_bps, true, 0).await;
            let client = HttpCoordinatorClient::new(base);
            let err =
                register_until_accepted(&client, &profile, "payout-addr", None, &identity, None)
                    .await
                    .unwrap_err();
            assert!(
                err.to_string().contains("unusable marketplace fee"),
                "refuses a confiscatory {fee_bps} bps fee: {err}"
            );
        }

        // One basis point under the line is still disclose-and-continue.
        let just_under = covenant_compute_protocol::MAX_FEE_BPS - 1;
        let base = spawn_disclosing_coordinator(just_under, true, 0).await;
        let client = HttpCoordinatorClient::new(base);
        let fee = register_until_accepted(&client, &profile, "payout-addr", None, &identity, None)
            .await
            .unwrap();
        assert_eq!(fee, just_under);
    }

    #[tokio::test]
    async fn an_explicit_rejection_bails_while_a_transport_hiccup_retries() {
        // A policy rejection is not retryable — it surfaces with the
        // coordinator's own reason.
        let rejecting = spawn_disclosing_coordinator(0, false, 0).await;
        let client = HttpCoordinatorClient::new(rejecting);
        let identity = LocalIdentity::generate("op@test");
        let profile = test_profile(&identity);
        let err = register_until_accepted(&client, &profile, "payout-addr", None, &identity, None)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("coordinator rejected registration"), "{msg}");
        assert!(msg.contains("closed pool"), "{msg}");

        // A transport-class failure retries until the coordinator
        // answers — the boot-before-network posture. The 500 surfaces
        // from one register() call as an error; the outer loop's
        // backoff pass is what lands the registration.
        let flaky = spawn_disclosing_coordinator(40, true, 1).await;
        let client = HttpCoordinatorClient::new(flaky);
        let fee = register_until_accepted(&client, &profile, "payout-addr", None, &identity, None)
            .await
            .unwrap();
        assert_eq!(fee, 40);
    }

    async fn spawn_status_rpc(result: serde_json::Value) -> String {
        let app = axum::Router::new().route(
            "/",
            axum::routing::post(move || {
                let result = result.clone();
                async move {
                    axum::Json(serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": result }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn a_known_signature_reports_its_confirmation_level() {
        // A payout the operator's RPC has seen at any level is on the
        // way: `earnings verify` reports it pending, never contradicted.
        let rpc = spawn_status_rpc(
            serde_json::json!({ "value": [{ "confirmationStatus": "confirmed" }] }),
        )
        .await;
        let level = fetch_signature_status(&reqwest::Client::new(), &rpc, "some-sig")
            .await
            .unwrap();
        assert_eq!(level.as_deref(), Some("confirmed"));
    }

    #[tokio::test]
    async fn a_signature_the_rpc_never_saw_is_none() {
        let rpc = spawn_status_rpc(serde_json::json!({ "value": [serde_json::Value::Null] })).await;
        let level = fetch_signature_status(&reqwest::Client::new(), &rpc, "phantom")
            .await
            .unwrap();
        assert!(level.is_none());
    }
}
