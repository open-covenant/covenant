//! Covenant daemon library. Local-first coordination layer exposing intent
//! dispatch, agent runtime, memory, identity, permissions, audit, and
//! settlement over a Unix socket and an HTTP gateway. Per-dispatch we
//! write a working-tier memory record, a settlement receipt, an audit
//! event, and a `CapabilityCheck` audit row that records the dispatch
//! attribution alongside the dispatch-time scope predicates enforced in
//! [`covenant_permissions`].

#![deny(unsafe_code)]

pub mod http;
pub mod hyre;
pub mod sse;
pub mod stream_dispatch;
pub mod stream_tracker;
pub mod x402;

use anyhow::{Context, Result};
use covenant_a2a::Mailbox;
use covenant_audit::{hash_hex, AuditError, AuditEvent, AuditKind, AuditLog};
use covenant_budget::{
    BudgetCheckpointError, BudgetError, BudgetLedger, JsonlPauseCheckpointStore,
};
use covenant_identity::LocalIdentity;
use covenant_ipc::{
    read_frame, write_frame, ChainStatus, IpcError, ReceiptBatchSummary, Request, Response,
    StreamEnvelope,
};
use covenant_llm::Embedder;
use covenant_mcp::ToolRegistry;
use covenant_memory::{memory_receipt_backfill_correlations, IgnoreSet, MemoryStore};
use covenant_peer_auth::{PeerEntry, PeerRegistry, PeerToken, RevokeOutcome};
#[cfg(test)]
use covenant_permissions::verify_with_clock;
use covenant_permissions::{
    a2a_scope_allows as permission_a2a_scope_allows,
    audit_purge_scope_allows as permission_audit_purge_scope_allows,
    capabilities_purge_scope_allows as permission_capabilities_purge_scope_allows,
    chain_scope_allows as permission_chain_scope_allows,
    memory_backfill_scope_allows as permission_memory_backfill_scope_allows,
    memory_compaction_scope_allows as permission_memory_compaction_scope_allows,
    memory_purge_scope_allows as permission_memory_purge_scope_allows,
    memory_read_record_scope_allows as permission_memory_read_record_scope_allows,
    memory_read_scope_allows as permission_memory_read_scope_allows,
    memory_repair_scope_allows as permission_memory_repair_scope_allows,
    memory_write_scope_allows as permission_memory_write_scope_allows,
    peer_scope_allows as permission_peer_scope_allows,
    settlement_backfill_scope_allows as permission_settlement_backfill_scope_allows,
    sign as sign_capability, tool_call_scope_allows as permission_tool_call_scope_allows,
    validate_scope, verify_with_clock_and_trust_root, A2aScopeRequest, CapabilityStore,
    ChainScopeRequest, MemoryCompactionScopeRequest, PeerScopeRequest,
};
use covenant_router::{AgentCard, Router};
use covenant_runtime::{AgentResult, Runner};
use covenant_sap_bridge::{Config as SapBridgeConfig, SapBridge};
use covenant_settlement::{
    build_receipt_batch, intent_dispatch_credits, memory_write_credits, ChainConfirmation,
    Settlement,
};
use covenant_types::{
    AgentId, BudgetPauseCheckpoint, BudgetPauseReason, Capability, Intent, MemoryCompactionRequest,
    MemoryRecord, MemoryRepairCommand, MemoryRepairMode, MemoryTier, Priority, ResourceKind,
    SettlementReceipt,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

pub fn covenant_home() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("COVENANT_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".covenant"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeRunnerConfig {
    TrustedLocal,
    LinuxGvisor {
        runsc_path: PathBuf,
        rootfs: PathBuf,
        scratch_root: PathBuf,
    },
}

impl RuntimeRunnerConfig {
    pub fn backend_name(&self) -> &'static str {
        match self {
            Self::TrustedLocal => "trusted-local",
            Self::LinuxGvisor { .. } => "linux-gvisor",
        }
    }
}

pub fn runtime_runner_config_from_env(home: &Path) -> Result<RuntimeRunnerConfig> {
    runtime_runner_config_from_values(
        home,
        std::env::var("COVENANT_RUNTIME_BACKEND").ok().as_deref(),
        std::env::var("COVENANT_GVISOR_ROOTFS").ok().as_deref(),
        std::env::var("COVENANT_RUNSC").ok().as_deref(),
        std::env::var("COVENANT_GVISOR_SCRATCH").ok().as_deref(),
    )
}

pub fn runtime_runner_config_from_values(
    home: &Path,
    backend: Option<&str>,
    rootfs: Option<&str>,
    runsc_path: Option<&str>,
    scratch_root: Option<&str>,
) -> Result<RuntimeRunnerConfig> {
    let backend = backend
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("trusted-local");

    match backend {
        "trusted-local" => Ok(RuntimeRunnerConfig::TrustedLocal),
        "linux-gvisor" => {
            let rootfs = rootfs
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .context(
                    "COVENANT_GVISOR_ROOTFS is required when COVENANT_RUNTIME_BACKEND=linux-gvisor",
                )?;
            let runsc_path = runsc_path
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("runsc"));
            let scratch_root = scratch_root
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("runtime").join("gvisor"));
            Ok(RuntimeRunnerConfig::LinuxGvisor {
                runsc_path,
                rootfs,
                scratch_root,
            })
        }
        other => anyhow::bail!(
            "unsupported COVENANT_RUNTIME_BACKEND {other:?}; expected trusted-local or linux-gvisor"
        ),
    }
}

/// Builds the per-backend runner. `tracker` is wired into both the
/// trusted-local SubprocessRunner and the linux-gvisor GvisorRunner so
/// the daemon's future budget-preempt projection tick can walk
/// in-flight subprocesses by intent_id regardless of which backend ran
/// them. For gVisor, the tracker holds the host-visible runsc pid; a
/// SIGTERM to runsc's process group propagates termination into the
/// sandbox.
pub fn runtime_runner_from_config(
    config: &RuntimeRunnerConfig,
    tracker: Arc<covenant_runtime::SubprocessTracker>,
) -> Arc<dyn Runner> {
    match config {
        RuntimeRunnerConfig::TrustedLocal => {
            Arc::new(covenant_runtime::SubprocessRunner::with_tracker(tracker))
        }
        RuntimeRunnerConfig::LinuxGvisor {
            runsc_path,
            rootfs,
            scratch_root,
        } => Arc::new(covenant_runtime::GvisorRunner::with_paths_and_tracker(
            runsc_path,
            rootfs,
            scratch_root,
            tracker,
        )),
    }
}

/// Gateway connection for the Hermes runtime backend
/// (<https://github.com/NousResearch/hermes-agent>). Wraps the API base URL
/// and an optional bearer token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HermesGatewayConfig {
    pub base_url: String,
    pub api_key: Option<String>,
}

/// Read the Hermes gateway config from env. Returns `None` if
/// `HERMES_API_BASE_URL` is unset or empty — in which case any agent
/// with `runtime = "hermes"` will fail dispatch with
/// `RunnerError::HermesUnconfigured`.
pub fn hermes_gateway_config_from_env() -> Option<HermesGatewayConfig> {
    hermes_gateway_config_from_values(
        std::env::var("HERMES_API_BASE_URL").ok().as_deref(),
        std::env::var("HERMES_API_KEY").ok().as_deref(),
    )
}

/// Resolve the Synapse Agent Protocol bridge config from the same
/// `COVENANT_SAP_*` environment the worker reads, so daemon and worker
/// stay consistent. The returned config may be `enabled: false`
/// (default); callers should still construct a [`SapBridge`] from it —
/// disabled-bridge methods return `BridgeDisabledError` without
/// touching the network.
pub fn sap_bridge_config_from_env() -> SapBridgeConfig {
    SapBridgeConfig::from_env(std::env::vars())
}

pub fn hermes_gateway_config_from_values(
    base_url: Option<&str>,
    api_key: Option<&str>,
) -> Option<HermesGatewayConfig> {
    let base_url = base_url
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    let api_key = api_key
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    Some(HermesGatewayConfig { base_url, api_key })
}

/// Build the dispatching runner the daemon hands to `Server::new`. The
/// local backend (subprocess or gVisor) handles `runtime = python3|node|rust-bin`;
/// the Hermes gateway, if configured, handles `runtime = hermes`.
pub fn runtime_runner_composite(
    local: &RuntimeRunnerConfig,
    hermes: Option<&HermesGatewayConfig>,
    tracker: Arc<covenant_runtime::SubprocessTracker>,
    events: Option<covenant_runtime::RuntimeEventSink>,
) -> Arc<dyn Runner> {
    let local_runner = runtime_runner_from_config(local, tracker);
    let hermes_runner: Option<Arc<dyn Runner>> = hermes.and_then(move |cfg| {
        match covenant_runtime::HermesRunner::new(cfg.base_url.clone(), cfg.api_key.clone()) {
            Ok(r) => {
                // Wire the live event sink so the gateway's SSE trace stream
                // folds into the audit chain as it arrives, not all at once.
                let r = match events {
                    Some(tx) => r.with_event_sink(tx),
                    None => r,
                };
                Some(Arc::new(r) as Arc<dyn Runner>)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    base_url = %cfg.base_url,
                    "hermes runner init failed (TLS or client config) — hermes runtime disabled",
                );
                None
            }
        }
    });
    Arc::new(covenant_runtime::CompositeRunner::new(
        local_runner,
        hermes_runner,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct A2AAutoRetrySchedulerConfig {
    pub enabled: bool,
    pub interval_ms: u64,
    pub policy: covenant_a2a::A2AAutoRetryPolicy,
}

impl Default for A2AAutoRetrySchedulerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_ms: 60_000,
            policy: covenant_a2a::A2AAutoRetryPolicy::default(),
        }
    }
}

pub fn a2a_auto_retry_scheduler_config_from_env() -> Result<A2AAutoRetrySchedulerConfig> {
    a2a_auto_retry_scheduler_config_from_values(
        std::env::var("COVENANT_A2A_AUTO_RETRY_SCHEDULER")
            .ok()
            .as_deref(),
        std::env::var("COVENANT_A2A_AUTO_RETRY_INTERVAL_MS")
            .ok()
            .as_deref(),
        std::env::var("COVENANT_A2A_AUTO_RETRY_MIN_LEASE_AGE_MS")
            .ok()
            .as_deref(),
        std::env::var("COVENANT_A2A_AUTO_RETRY_MAX_ATTEMPTS")
            .ok()
            .as_deref(),
        std::env::var("COVENANT_A2A_AUTO_RETRY_MAX_REQUEUES")
            .ok()
            .as_deref(),
        std::env::var("COVENANT_A2A_AUTO_RETRY_SCAN_LIMIT")
            .ok()
            .as_deref(),
    )
}

pub fn a2a_auto_retry_scheduler_config_from_values(
    enabled: Option<&str>,
    interval_ms: Option<&str>,
    min_lease_age_ms: Option<&str>,
    max_attempts: Option<&str>,
    max_requeues: Option<&str>,
    scan_limit: Option<&str>,
) -> Result<A2AAutoRetrySchedulerConfig> {
    let mut config = A2AAutoRetrySchedulerConfig::default();
    config.enabled = enabled
        .map(parse_env_bool)
        .transpose()?
        .unwrap_or(config.enabled);
    config.policy.enabled = config.enabled;

    if let Some(value) = interval_ms {
        config.interval_ms = parse_env_u64("COVENANT_A2A_AUTO_RETRY_INTERVAL_MS", value)?;
        if config.interval_ms == 0 {
            anyhow::bail!("COVENANT_A2A_AUTO_RETRY_INTERVAL_MS must be greater than zero");
        }
    }
    if let Some(value) = min_lease_age_ms {
        config.policy.min_lease_age_ms =
            parse_env_u64("COVENANT_A2A_AUTO_RETRY_MIN_LEASE_AGE_MS", value)?;
    }
    if let Some(value) = max_attempts {
        config.policy.max_attempts = parse_env_u32("COVENANT_A2A_AUTO_RETRY_MAX_ATTEMPTS", value)?;
    }
    if let Some(value) = max_requeues {
        config.policy.max_requeues =
            parse_env_usize("COVENANT_A2A_AUTO_RETRY_MAX_REQUEUES", value)?;
    }
    if let Some(value) = scan_limit {
        config.policy.scan_limit = parse_env_usize("COVENANT_A2A_AUTO_RETRY_SCAN_LIMIT", value)?;
    }

    Ok(config)
}

/// Lift a `RuntimeTrace` from a runner (currently only Hermes) into the
/// matching `AuditKind` row. The raw `preview` payload is hashed here so
/// the chain never embeds tool input verbatim.
fn runtime_trace_to_audit_kind(
    intent_id: Uuid,
    trace: covenant_runtime::RuntimeTrace,
) -> AuditKind {
    use covenant_runtime::RuntimeTrace as T;
    match trace {
        T::HermesToolInvoked {
            run_id,
            tool,
            preview,
        } => AuditKind::HermesToolInvoked {
            intent_id,
            run_id,
            tool,
            preview_hash_hex: hash_hex(preview.as_bytes()),
        },
        T::HermesToolCompleted {
            run_id,
            tool,
            duration_ms,
            error,
        } => AuditKind::HermesToolCompleted {
            intent_id,
            run_id,
            tool,
            duration_ms,
            error,
        },
        T::HermesApprovalRequested { run_id, choices } => AuditKind::HermesApprovalRequested {
            intent_id,
            run_id,
            choices,
        },
        T::HermesApprovalResponded {
            run_id,
            choice,
            resolved,
        } => AuditKind::HermesApprovalResolved {
            intent_id,
            run_id,
            choice,
            resolved,
        },
        T::HermesFileWritten {
            run_id,
            path,
            bytes,
        } => AuditKind::HermesFileWritten {
            intent_id,
            run_id,
            path,
            bytes,
        },
    }
}

fn parse_env_bool(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => anyhow::bail!("expected boolean env value, got {other:?}"),
    }
}

fn parse_env_u64(name: &str, value: &str) -> Result<u64> {
    value
        .trim()
        .parse()
        .with_context(|| format!("{name} must be an integer"))
}

fn parse_env_u32(name: &str, value: &str) -> Result<u32> {
    value
        .trim()
        .parse()
        .with_context(|| format!("{name} must be an integer"))
}

fn parse_env_usize(name: &str, value: &str) -> Result<usize> {
    value
        .trim()
        .parse()
        .with_context(|| format!("{name} must be an integer"))
}

/// Cadence and grace window for the budget projection tick driver.
/// `period_ms` controls how often [`Server::run_projection_tick_iteration`]
/// runs; `grace_ms` is the SIGTERM→SIGKILL window passed to every
/// dispatched [`Server::preempt_intent`] call. Both are read from
/// environment variables at daemon startup (see
/// [`projection_tick_config_from_env`]) so operators can tune cadence
/// without a rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectionTickConfig {
    pub period_ms: u64,
    pub grace_ms: u64,
}

impl Default for ProjectionTickConfig {
    fn default() -> Self {
        Self {
            period_ms: 250,
            grace_ms: 2_000,
        }
    }
}

pub fn projection_tick_config_from_env() -> Result<ProjectionTickConfig> {
    projection_tick_config_from_values(
        std::env::var("COVENANT_BUDGET_PROJECTION_TICK_MS")
            .ok()
            .as_deref(),
        std::env::var("COVENANT_BUDGET_PREEMPT_GRACE_MS")
            .ok()
            .as_deref(),
    )
}

pub fn projection_tick_config_from_values(
    period_ms: Option<&str>,
    grace_ms: Option<&str>,
) -> Result<ProjectionTickConfig> {
    let mut config = ProjectionTickConfig::default();
    if let Some(value) = period_ms {
        config.period_ms = parse_env_u64("COVENANT_BUDGET_PROJECTION_TICK_MS", value)?;
        if config.period_ms == 0 {
            anyhow::bail!("COVENANT_BUDGET_PROJECTION_TICK_MS must be greater than zero");
        }
    }
    if let Some(value) = grace_ms {
        config.grace_ms = parse_env_u64("COVENANT_BUDGET_PREEMPT_GRACE_MS", value)?;
    }
    Ok(config)
}

/// Spawn the budget projection tick driver. Returns a `JoinHandle` so
/// `main` can hold it for the lifetime of the daemon; dropping the
/// handle lets tokio reap the task on runtime shutdown.
///
/// The driver loops `interval.tick().await; run_projection_tick_iteration(grace).await;`
/// with `MissedTickBehavior::Skip` so a slow inner iteration (e.g., one
/// preempt_intent whose grace window approaches the tick period) does
/// not stampede preempt dispatches on resume — missed ticks are
/// dropped and the driver yields a steady one-iteration-per-period
/// rate regardless of contention.
pub fn spawn_projection_tick_driver(
    server: Server,
    config: ProjectionTickConfig,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let period = Duration::from_millis(config.period_ms);
        let grace = Duration::from_millis(config.grace_ms);
        let mut interval = tokio::time::interval(period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let preempted = server.run_projection_tick_iteration(grace).await;
            if preempted > 0 {
                info!(
                    preempted,
                    "budget projection tick preempted in-flight intents"
                );
            }
        }
    })
}

/// Drains runtime traces the Hermes runner streams live and writes each into
/// the audit chain the moment it arrives, so the task page shows the coding
/// step-trail building in real time instead of all at once when the run ends.
/// The matching `runtime_events` returned by the runner are empty in this mode,
/// so the end-of-dispatch fold writes nothing (no double rows).
///
/// Each trace is also published to `broadcast_tx`, a fan-out channel any
/// number of HTTP subscribers (the `/intents/:id/events` SSE endpoint, the
/// operator UI) can join via `subscribe()` to receive a live copy without
/// re-polling the audit log. `broadcast.send` returns `Err` only when there
/// are no receivers — that's the steady state when nobody is watching, and
/// it must be tolerated, not logged.
pub fn spawn_runtime_event_drainer(
    server: Server,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<covenant_runtime::StreamedTrace>,
    broadcast_tx: tokio::sync::broadcast::Sender<covenant_runtime::StreamedTrace>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(st) = rx.recv().await {
            let event = AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: st.issuer.clone(),
                kind: runtime_trace_to_audit_kind(st.intent_id, st.trace.clone()),
            };
            server.record_peer_event(&st.issuer, event).await;
            // Best-effort live broadcast: subscribers (the SSE endpoint)
            // get a copy; no subscribers = no-op.
            let _ = broadcast_tx.send(st);
        }
    })
}

pub fn spawn_a2a_auto_retry_scheduler(
    server: Server,
    config: A2AAutoRetrySchedulerConfig,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let delay = Duration::from_millis(config.interval_ms);
        loop {
            tokio::time::sleep(delay).await;
            match server
                .run_a2a_auto_retry_scheduler_once(config.policy)
                .await
            {
                Response::A2AAutoRetried { report } => {
                    info!(
                        considered = report.considered,
                        requeued = report.requeued.len(),
                        skipped = report.skipped.len(),
                        "a2a auto retry scheduler scan complete"
                    );
                }
                Response::Error { message } => {
                    warn!(error = %message, "a2a auto retry scheduler scan rejected");
                }
                other => {
                    warn!(response = ?other, "a2a auto retry scheduler returned unexpected response");
                }
            }
        }
    })
}

fn a2a_repair_action(command: &covenant_a2a::A2ARepairCommand) -> &'static str {
    match command {
        covenant_a2a::A2ARepairCommand::Requeue { .. } => "requeue",
        covenant_a2a::A2ARepairCommand::ForceError { .. } => "force_error",
    }
}

fn a2a_repair_lease_id(command: &covenant_a2a::A2ARepairCommand) -> Option<Uuid> {
    match command {
        covenant_a2a::A2ARepairCommand::Requeue { lease_id, .. }
        | covenant_a2a::A2ARepairCommand::ForceError { lease_id, .. } => *lease_id,
    }
}

fn a2a_duplicate_risk(command: &covenant_a2a::A2ARepairCommand) -> Option<&'static str> {
    match command {
        covenant_a2a::A2ARepairCommand::Requeue { duplicate_risk, .. } => match duplicate_risk {
            covenant_a2a::A2ADuplicateRisk::Idempotent => Some("idempotent"),
            covenant_a2a::A2ADuplicateRisk::OperatorAccepted => Some("operator_accepted"),
        },
        covenant_a2a::A2ARepairCommand::ForceError { .. } => None,
    }
}

fn memory_repair_id(command: &MemoryRepairCommand) -> Uuid {
    match command {
        MemoryRepairCommand::DetachParent { id, .. }
        | MemoryRepairCommand::DeleteRecord { id }
        | MemoryRepairCommand::BackfillProvenance { id, .. } => *id,
    }
}

fn memory_repair_action(command: &MemoryRepairCommand) -> &'static str {
    match command {
        MemoryRepairCommand::DetachParent { .. } => "detach_parent",
        MemoryRepairCommand::DeleteRecord { .. } => "delete_record",
        MemoryRepairCommand::BackfillProvenance { .. } => "backfill_provenance",
    }
}

fn memory_repair_mode(mode: MemoryRepairMode) -> &'static str {
    match mode {
        MemoryRepairMode::DryRun => "dry_run",
        MemoryRepairMode::Apply => "apply",
    }
}

fn memory_tier_name(tier: MemoryTier) -> &'static str {
    match tier {
        MemoryTier::Working => "working",
        MemoryTier::Episodic => "episodic",
        MemoryTier::LongTerm => "longterm",
    }
}

fn memory_read_actions(tier: Option<MemoryTier>) -> Vec<String> {
    match tier {
        Some(tier) => vec![
            "memory.read".into(),
            format!("memory.read.{}", memory_tier_name(tier)),
        ],
        None => vec![
            "memory.read".into(),
            "memory.read.working".into(),
            "memory.read.episodic".into(),
            "memory.read.longterm".into(),
        ],
    }
}

fn memory_read_record_allowed(
    scopes: &[(String, serde_json::Value)],
    record: &MemoryRecord,
) -> bool {
    let tier = memory_tier_name(record.tier);
    let record_id = record.id.to_string();
    scopes.iter().any(|(action, scope)| {
        permission_memory_read_record_scope_allows(
            action,
            scope,
            &record_id,
            tier,
            record.created_at,
        )
        .unwrap_or(false)
    })
}

fn settlement_resource_name(resource: ResourceKind) -> &'static str {
    match resource {
        ResourceKind::Compute => "compute",
        ResourceKind::Memory => "memory",
        ResourceKind::Tool => "tool",
        ResourceKind::Message => "message",
        ResourceKind::Registration => "registration",
    }
}

fn chain_receipt_allowed(
    scopes: &[(String, serde_json::Value)],
    receipt: &SettlementReceipt,
) -> bool {
    let payer = receipt.payer.pubkey_base58();
    let resource = settlement_resource_name(receipt.resource);
    let cluster = receipt.cluster.as_deref().unwrap_or("");
    let batch_id = receipt.batch_id.as_deref().unwrap_or("");
    scopes.iter().any(|(action, scope)| {
        permission_chain_scope_allows(
            action,
            scope,
            action,
            ChainScopeRequest {
                payer_pubkey_b58: Some(&payer),
                resource: Some(resource),
                cluster: Some(cluster),
                batch_id: Some(batch_id),
                ..ChainScopeRequest::default()
            },
        )
        .unwrap_or(false)
    })
}

fn a2a_entry_visible_to_peer(entry: &covenant_a2a::A2ATaskQueueEntry, peer: &AgentId) -> bool {
    entry.task.sender.pubkey == peer.pubkey
        || entry.task.recipient.pubkey == peer.pubkey
        || entry
            .leased_to
            .as_ref()
            .map(|agent| agent.pubkey == peer.pubkey)
            .unwrap_or(false)
}

fn a2a_entry_matches_min_lease_age(
    entry: &covenant_a2a::A2ATaskQueueEntry,
    min_lease_age_ms: Option<u64>,
    now_ms: u64,
) -> bool {
    let Some(min_lease_age_ms) = min_lease_age_ms else {
        return true;
    };
    if entry.state != covenant_a2a::A2ATaskQueueState::InFlight {
        return true;
    }
    entry
        .leased_at_ms
        .map(|leased_at| now_ms.saturating_sub(leased_at) >= min_lease_age_ms)
        .unwrap_or(false)
}

/// `--deadline-within-ms <N>` filter. Returns true when `N` is unset
/// (no filter), otherwise keeps only entries whose `task.deadline_ms`
/// is `Some(d)` and `d <= now_ms + N`. Tasks without a deadline are
/// always dropped under an active filter so the operator can triage
/// by remaining time without scraping the JSON for `deadline_ms !=
/// null`. Saturating addition keeps an oversized `N` from wrapping.
fn a2a_entry_matches_deadline_within(
    entry: &covenant_a2a::A2ATaskQueueEntry,
    deadline_within_ms: Option<u64>,
    now_ms: u64,
) -> bool {
    let Some(window) = deadline_within_ms else {
        return true;
    };
    let Some(deadline) = entry.task.deadline_ms else {
        return false;
    };
    deadline <= now_ms.saturating_add(window)
}

/// `--state queued|in_flight` filter. Returns true when the filter
/// is unset, otherwise keeps only entries whose `state` matches. The
/// filter runs against the typed `A2ATaskQueueState` discriminator on
/// the queue entry — the CLI maps `--state` strings to the same enum
/// so a typo at the CLI layer is rejected before the daemon sees it.
fn a2a_entry_matches_state(
    entry: &covenant_a2a::A2ATaskQueueEntry,
    state_filter: Option<covenant_a2a::A2ATaskQueueState>,
) -> bool {
    match state_filter {
        Some(state) => entry.state == state,
        None => true,
    }
}

/// Cap on `RevokeOutcome::Ambiguous.matches`. When more than this many
/// registry entries match the operator's prefix, the daemon returns the
/// first `PEER_MATCH_LIMIT` summaries plus `truncated: true` so the
/// operator can narrow with a longer prefix without first paying for an
/// unbounded payload. Distinct from the CLI's `PEER_LOOKUP_LIMIT`
/// (`crates/covenant/src/main.rs`) which bounds peer-lookup fanout for
/// `expand_a2a_action`, not revoke-match fanout — keep the names
/// distinct so a future `grep` resolves cleanly.
const PEER_MATCH_LIMIT: usize = 16;

/// Result of an async (hermes) dispatch, polled by the web client while a
/// long coding run is in flight. Heavy builds (scaffold + npm install +
/// compile) outlast the front-door LB's idle window, so the submit verb
/// returns `status:"running"` immediately and the run finishes in a spawned
/// task that writes its outcome here.
#[derive(Clone)]
struct IntentOutcome {
    status: String,
    intent_text: String,
    matched_agent: Option<String>,
    text: String,
    result_hash_hex: Option<String>,
    updated_ms: u64,
    /// Workspace files captured from the run (for the UI file tree / preview).
    /// Set via `set_files` during dispatch; `complete` leaves them untouched.
    files: Vec<covenant_runtime::BuildFile>,
}

#[derive(Default)]
struct OutcomeStore {
    map: std::collections::HashMap<Uuid, IntentOutcome>,
    order: std::collections::VecDeque<Uuid>,
}

impl OutcomeStore {
    const CAP: usize = 512;

    fn insert_running(&mut self, id: Uuid, intent_text: &str, matched_agent: Option<String>) {
        self.map.insert(
            id,
            IntentOutcome {
                status: "running".into(),
                intent_text: intent_text.to_string(),
                matched_agent,
                text: String::new(),
                result_hash_hex: None,
                updated_ms: epoch_ms(),
                files: Vec::new(),
            },
        );
        self.order.push_back(id);
        while self.order.len() > Self::CAP {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }

    fn set_files(&mut self, id: Uuid, files: Vec<covenant_runtime::BuildFile>) {
        if let Some(o) = self.map.get_mut(&id) {
            o.files = files;
            o.updated_ms = epoch_ms();
        }
    }

    fn complete(&mut self, id: Uuid, resp: &Response) {
        let Some(o) = self.map.get_mut(&id) else {
            return;
        };
        match resp {
            Response::IntentResult { status, text, .. } => {
                o.status = status.clone();
                o.text = text.clone();
                o.result_hash_hex = Some(hash_hex(text.as_bytes()));
            }
            Response::Error { message } => {
                o.status = "error".into();
                o.text = message.clone();
            }
            _ => {
                o.status = "error".into();
                o.text = "unexpected dispatch response".into();
            }
        }
        o.updated_ms = epoch_ms();
    }
}

#[derive(Clone)]
pub struct Server {
    router: Arc<Router>,
    runner: Arc<dyn Runner>,
    memory: Arc<dyn MemoryStore>,
    settlement: Arc<dyn Settlement>,
    audit: Arc<dyn AuditLog>,
    capabilities: Arc<dyn CapabilityStore>,
    embedder: Arc<dyn Embedder>,
    identity: Arc<LocalIdentity>,
    ignore: Arc<IgnoreSet>,
    tools: Arc<ToolRegistry>,
    mailbox: Arc<dyn Mailbox>,
    pub peers: Arc<dyn PeerRegistry>,
    budget: Arc<dyn BudgetLedger>,
    budget_checkpoints: Option<Arc<JsonlPauseCheckpointStore>>,
    active_budget_pauses: Arc<Mutex<BTreeMap<Uuid, BudgetPauseCheckpoint>>>,
    /// In-flight IPC v2 streaming-response tracker (ADR 0010). Shared
    /// across connection handlers; entries are keyed by
    /// `(connection_id, stream_id)`. The Server allocates a fresh
    /// `Uuid::new_v4()` connection_id per accepted connection in
    /// `serve()` and the handler's `PurgeOnDrop` guard calls
    /// `purge_connection` on every exit path so a client disconnect
    /// cleans up its in-flight streams. The per-verb streaming dispatch
    /// forks (ADR 0010 slices 3.d–5.d) register an entry per opened
    /// stream and unregister it on stream end or error.
    stream_tracker: Arc<stream_tracker::StreamTracker>,
    /// Daemon-shared in-flight subprocess tracker. `SubprocessRunner`
    /// and `GvisorRunner` register `intent_id → TrackedSubprocess`
    /// entries on each spawn; `Server::preempt_intent` reads them to
    /// signal the kernel-visible pid. By default `Server::new` creates
    /// a fresh tracker; the daemon's `main` overrides this via
    /// `Server::with_subprocess_tracker` to share one Arc with the
    /// runner so the lookup actually finds the runner's entries.
    subprocess_tracker: Arc<covenant_runtime::SubprocessTracker>,
    /// `$COVENANT_HOME` for this daemon — set via [`Server::with_home`]
    /// in the binary's `main`. Required by [`Server::rotate_operator_token`]
    /// (which needs to read the current operator token from
    /// `<home>/peers/operator.token` and write the rotated one back to
    /// the same path with mode 0600). All other handlers are home-agnostic
    /// — they go through the storage traits — so unit tests that don't
    /// exercise rotation leave this `None`.
    home: Option<PathBuf>,
    /// Opt-in outbound x402 dispatch config. None when no operator
    /// has wired up the funding-key sidecar; in that state every
    /// `Request::PayX402` returns a "not configured" error and no
    /// USDC is ever spent.
    x402_dispatch: Option<Arc<x402::X402Config>>,
    /// Opt-in Hyre provider profile: the materialised catalog + config.
    /// None when the operator has not enabled Hyre; in that state no
    /// `hyre.*` tool is advertised or callable.
    hyre: Option<Arc<hyre::HyreState>>,
    /// Opt-in Synapse Agent Protocol bridge. `None` when no operator
    /// has wired it in (the default); a built [`SapBridge`] when
    /// `Server::with_sap_bridge` was called at boot. Handlers that
    /// need on-chain identity / attestation / discovery read this and
    /// surface `BridgeDisabledError` when it's absent or
    /// `enabled = false`.
    sap_bridge: Option<SapBridge>,
    /// Outcomes of in-flight async (hermes) dispatches, keyed by intent id.
    /// `std::sync::Mutex` (not the tokio one used elsewhere) because the
    /// critical section is a trivial map mutation never held across `.await`.
    intent_outcomes: Arc<std::sync::Mutex<OutcomeStore>>,
}

#[allow(clippy::too_many_arguments)]
impl Server {
    pub fn new(
        router: Arc<Router>,
        runner: Arc<dyn Runner>,
        memory: Arc<dyn MemoryStore>,
        settlement: Arc<dyn Settlement>,
        audit: Arc<dyn AuditLog>,
        capabilities: Arc<dyn CapabilityStore>,
        embedder: Arc<dyn Embedder>,
        identity: Arc<LocalIdentity>,
        ignore: Arc<IgnoreSet>,
        tools: Arc<ToolRegistry>,
        mailbox: Arc<dyn Mailbox>,
        peers: Arc<dyn PeerRegistry>,
        budget: Arc<dyn BudgetLedger>,
    ) -> Self {
        Self {
            router,
            runner,
            memory,
            settlement,
            audit,
            capabilities,
            embedder,
            identity,
            ignore,
            tools,
            mailbox,
            peers,
            budget,
            budget_checkpoints: None,
            active_budget_pauses: Arc::new(Mutex::new(BTreeMap::new())),
            stream_tracker: Arc::new(stream_tracker::StreamTracker::new()),
            subprocess_tracker: Arc::new(covenant_runtime::SubprocessTracker::new()),
            home: None,
            x402_dispatch: None,
            hyre: None,
            sap_bridge: None,
            intent_outcomes: Arc::new(std::sync::Mutex::new(OutcomeStore::default())),
        }
    }

    /// JSON snapshot of an async dispatch's current state, or `None` if the
    /// id is unknown (synchronous intent, evicted, or never submitted here).
    pub fn intent_outcome(&self, id: &Uuid) -> Option<serde_json::Value> {
        let store = self.intent_outcomes.lock().ok()?;
        let o = store.map.get(id)?;
        Some(serde_json::json!({
            "kind": "intent_outcome",
            "intent_id": id,
            "status": o.status,
            "intent_text": o.intent_text,
            "matched_agent": o.matched_agent,
            "text": o.text,
            "result_hash_hex": o.result_hash_hex,
            "updated_ms": o.updated_ms,
            "files": o.files,
        }))
    }

    /// Returns a clone of the shared in-flight stream tracker. Tests
    /// hold this Arc to pre-register synthetic entries and assert the
    /// connection handler's purge-on-close behavior. Production dispatch
    /// writes through the `stream_tracker` field directly; this accessor
    /// is reserved for a future operator-facing in-flight-streams snapshot.
    pub fn stream_tracker(&self) -> Arc<stream_tracker::StreamTracker> {
        self.stream_tracker.clone()
    }

    /// Returns a clone of the shared in-flight subprocess tracker.
    /// Tests use this to register synthetic entries before calling
    /// `preempt_intent`. The daemon's `main` calls
    /// `with_subprocess_tracker` to wire the same Arc into both the
    /// Server and the runner.
    pub fn subprocess_tracker(&self) -> Arc<covenant_runtime::SubprocessTracker> {
        self.subprocess_tracker.clone()
    }

    /// Replace the Server's subprocess tracker with one the caller
    /// already owns. The daemon's `main` constructs one
    /// `Arc<SubprocessTracker>` and passes a clone to both the runner
    /// (via `runtime_runner_composite`) and the Server (via this
    /// builder) so `preempt_intent` finds the entries the runner
    /// registered. Without this, the Server's default tracker and the
    /// runner's tracker are distinct allocations and `preempt_intent`
    /// always returns `NotInFlight`.
    pub fn with_subprocess_tracker(
        mut self,
        tracker: Arc<covenant_runtime::SubprocessTracker>,
    ) -> Self {
        self.subprocess_tracker = tracker;
        self
    }

    /// Bind a `$COVENANT_HOME` path so `Server::rotate_operator_token`
    /// knows where to read the current token and where to rewrite it.
    /// Daemon `main` calls this once after [`Server::new`]. Without it,
    /// `RotateOperatorToken` returns `Response::Error`.
    pub fn with_home(mut self, home: PathBuf) -> Self {
        self.home = Some(home);
        self
    }

    /// Wire the outbound x402 dispatch config. Without this, every
    /// `Request::PayX402` returns a "not configured" error and no
    /// paid call leaves the daemon. The daemon's `main` calls this
    /// after [`Server::new`] when the operator has opted in via env.
    pub fn with_x402_dispatch(mut self, config: x402::X402Config) -> Self {
        self.x402_dispatch = Some(Arc::new(config));
        self
    }

    /// Enable the Hyre provider profile. Advertises one `hyre.*` MCP
    /// tool per catalog endpoint and routes their calls through the
    /// outbound x402 path. Requires [`Self::with_x402_dispatch`] for
    /// the funding-key sidecar; without it a `hyre.*` call returns a
    /// "not configured" error.
    pub fn with_hyre(mut self, state: hyre::HyreState) -> Self {
        self.hyre = Some(Arc::new(state));
        self
    }

    pub fn with_budget_checkpoints(mut self, store: Arc<JsonlPauseCheckpointStore>) -> Self {
        self.budget_checkpoints = Some(store);
        self
    }

    /// Attach the Synapse Agent Protocol bridge. Daemon `main` calls
    /// this once at boot with the bridge from [`sap_bridge_config_from_env`],
    /// so it is always attached; the config's `enabled` flag governs
    /// behavior, and a disabled bridge surfaces `BridgeDisabledError`.
    pub fn with_sap_bridge(mut self, bridge: SapBridge) -> Self {
        self.sap_bridge = Some(bridge);
        self
    }

    /// Returns the attached SAP bridge, if any. Handlers should treat
    /// `None` the same as a disabled bridge — a soft no-op surfaced as
    /// `BridgeDisabledError` to the caller.
    pub fn sap_bridge(&self) -> Option<&SapBridge> {
        self.sap_bridge.as_ref()
    }

    /// Resolve the SAP bridge status. Returns a disabled snapshot when
    /// no bridge was wired in at boot — handlers must never panic on
    /// `sap_bridge().is_none()`.
    pub(crate) fn sap_status(&self) -> Response {
        match self.sap_bridge.as_ref() {
            Some(bridge) => {
                let cfg = bridge.config();
                Response::SapStatus {
                    enabled: cfg.enabled,
                    cluster: cfg.cluster.as_str().to_owned(),
                    program_id: cfg.program_id.clone(),
                    rpc_url: cfg.rpc_url.clone(),
                    explorer_url: cfg.explorer_url.clone(),
                    // Bridge config doesn't carry the keypair path; an
                    // operator can read the daemon env directly. We
                    // surface "configured" iff COVENANT_SAP_KEYPAIR is
                    // set (the worker reads the same var).
                    has_signer: std::env::var("COVENANT_SAP_KEYPAIR")
                        .map(|v| !v.trim().is_empty())
                        .unwrap_or(false),
                }
            }
            None => Response::SapStatus {
                enabled: false,
                cluster: String::new(),
                program_id: String::new(),
                rpc_url: String::new(),
                explorer_url: String::new(),
                has_signer: false,
            },
        }
    }

    /// Publish an agent through the SAP bridge. Errors (disabled
    /// bridge, RPC failure, missing signer, etc.) flatten onto
    /// `Response::Error` with the bridge's own message so the CLI
    /// renders them consistently with other failures.
    pub(crate) async fn sap_publish_agent(&self, manifest_json: String) -> Response {
        let Some(bridge) = self.sap_bridge.as_ref() else {
            return Response::Error {
                message: "sap bridge is not wired into this daemon".into(),
            };
        };
        let manifest: covenant_sap_bridge::identity::AgentManifest =
            match serde_json::from_str(&manifest_json) {
                Ok(m) => m,
                Err(e) => {
                    return Response::Error {
                        message: format!("invalid manifest JSON: {e}"),
                    }
                }
            };
        match bridge.publish_agent(&manifest).await {
            Ok(published) => Response::SapPublishedAgent {
                agent_pda: published.agent_pda,
                signature: published.signature,
            },
            Err(e) => Response::Error {
                message: format!("sap publish_agent: {e}"),
            },
        }
    }

    /// Walk the router's registered agents and seed each one's budget
    /// bucket from its manifest's `Settlement.budget_credits_per_hour`.
    /// Cards with `budget_credits_per_hour == 0` are skipped — the spec's
    /// `Settlement` rustdoc says "Phase 0 tolerates 0; enforced from
    /// Phase 1," and `dispatch_intent` mirrors the predicate. Idempotent:
    /// calling twice is fine because [`BudgetLedger::set_capacity`]
    /// re-stamps the bucket without resetting `tokens_remaining` further
    /// than the (possibly shrunk) capacity. Daemon main calls this once
    /// after `Server::new`; tests that need budget enforcement call it
    /// explicitly.
    pub async fn register_agent_budgets(&self) -> Result<(), BudgetSeedError> {
        for card in self.router.agents() {
            let cap = card.manifest.settlement.budget_credits_per_hour;
            if cap == 0 {
                continue;
            }
            let agent = agent_id_for_card(card);
            self.budget
                .set_capacity(&agent, cap)
                .await
                .map_err(|source| BudgetSeedError {
                    agent_id: card.id.clone(),
                    source,
                })?;
        }
        Ok(())
    }

    /// Preempt one in-flight intent by intent_id. The Server looks up
    /// the runner-registered pid in `subprocess_tracker`, hands it to
    /// [`covenant_runtime::preempt_subprocess_pg`] with the supplied
    /// grace window, maps the returned `PreemptOutcome` to either a
    /// `BudgetPreempted` or `BudgetPreemptFailed` audit row, and
    /// returns a [`PreemptResult`] for the caller (today: tests; soon:
    /// the daemon-side projection tick).
    ///
    /// The lookup and the kill happen back-to-back; there is no
    /// intermediate await between `tracker.get` and `preempt_subprocess_pg`,
    /// so a fast natural-exit racing with the dispatcher cannot make
    /// the daemon kill a recycled pid. The audit append is the last
    /// step: a kill that succeeded but whose audit row failed to
    /// persist surfaces as `AuditWriteFailed` so the caller can choose
    /// whether to retry, log, or escalate.
    pub async fn preempt_intent(
        &self,
        intent_id: Uuid,
        reason: String,
        grace: std::time::Duration,
    ) -> PreemptResult {
        let Some(entry) = self.subprocess_tracker.get(&intent_id) else {
            return PreemptResult::NotInFlight;
        };
        let outcome = covenant_runtime::preempt_subprocess_pg(entry.pid, grace).await;
        let audit_kind = match &outcome {
            covenant_runtime::PreemptOutcome::AlreadyDead => Some(AuditKind::BudgetPreempted {
                agent_display: entry.agent_id.clone(),
                intent_id,
                reason: reason.clone(),
                signal_sent: "none".into(),
                exit_code: None,
            }),
            covenant_runtime::PreemptOutcome::ExitedDuringGrace => {
                Some(AuditKind::BudgetPreempted {
                    agent_display: entry.agent_id.clone(),
                    intent_id,
                    reason: reason.clone(),
                    signal_sent: "SIGTERM".into(),
                    exit_code: None,
                })
            }
            covenant_runtime::PreemptOutcome::SigKilled => Some(AuditKind::BudgetPreempted {
                agent_display: entry.agent_id.clone(),
                intent_id,
                reason: reason.clone(),
                signal_sent: "SIGKILL".into(),
                exit_code: None,
            }),
            covenant_runtime::PreemptOutcome::PermissionDenied { errno } => {
                Some(AuditKind::BudgetPreemptFailed {
                    agent_display: entry.agent_id.clone(),
                    intent_id,
                    reason: reason.clone(),
                    errno: *errno,
                })
            }
            covenant_runtime::PreemptOutcome::UnsupportedPlatform => None,
        };
        if let Some(kind) = audit_kind {
            let event = AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: self.identity.agent_id(),
                kind,
            };
            if let Err(e) = self.record_daemon_event_required(event).await {
                return PreemptResult::AuditWriteFailed {
                    outcome,
                    error: e.to_string(),
                };
            }
        }
        match outcome {
            covenant_runtime::PreemptOutcome::UnsupportedPlatform => {
                PreemptResult::UnsupportedPlatform
            }
            other => PreemptResult::Preempted { outcome: other },
        }
    }

    /// Walk every in-flight tracker entry, ask the budget ledger whether
    /// the owning agent's bucket is exhausted, and dispatch
    /// [`Server::preempt_intent`] for every flagged entry. Returns the
    /// number of successful preempts (`PreemptResult::Preempted`).
    ///
    /// This is the single-iteration core of the budget projection tick.
    /// The companion follow-on slice wires a `tokio::time::interval`
    /// driver that calls this on each tick; the iteration is exposed as
    /// a separate `pub async fn` so tests can drive it deterministically
    /// without time-mocking, and so the driver slice can focus only on
    /// scheduling concerns (cadence, shutdown signal, policy).
    ///
    /// `would_exceed(agent, 1)` is the v0.x exhaustion trigger: the
    /// [`BudgetLedger`] trait does not expose per-agent capacity, so the
    /// `LinearExtrapolation` policy from
    /// [`covenant_budget::project_overshoot`] needs a per-intent
    /// debit-rate signal that the ledger schema does not yet carry.
    /// Exhaustion-as-trigger delivers the operator-promised hard-guarantee
    /// shape ("tokens_remaining == 0 → kill the in-flight subprocess")
    /// without expanding the budget trait surface.
    ///
    /// Error policy: `BudgetError::NoCapacity` for an agent that was
    /// deprovisioned mid-flight skips that entry silently — the agent
    /// has no bucket so the in-flight intent is left to complete or
    /// fail on its own next debit attempt. Any other `BudgetError`
    /// produces a `warn!` and the entry is skipped; the next tick will
    /// retry.
    pub async fn run_projection_tick_iteration(&self, grace: std::time::Duration) -> usize {
        let mut preempted = 0;
        for (intent_id, entry) in self.subprocess_tracker.snapshot() {
            let agent = agent_id_for_card_id(&entry.agent_id);
            match self.budget.would_exceed(&agent, 1).await {
                Ok(true) => {
                    if let PreemptResult::Preempted { .. } = self
                        .preempt_intent(intent_id, "budget_overshoot".into(), grace)
                        .await
                    {
                        preempted += 1;
                    }
                }
                Ok(false) => {}
                Err(BudgetError::NoCapacity(_)) => {}
                Err(e) => {
                    warn!(
                        agent = %entry.agent_id,
                        ?intent_id,
                        error = %e,
                        "projection-tick: budget lookup failed; skipping entry until next tick"
                    );
                }
            }
        }
        preempted
    }

    /// Best-effort outbound error frame on the read-side failures the
    /// IPC loop terminates on (frame-size violation, malformed JSON).
    /// Without this the client sees a bare EOF and cannot distinguish a
    /// protocol-level fault from a transport reset. The Response::Error
    /// message is generic on purpose — concrete byte counts or serde
    /// position context would be info-disclosure if the frame came from
    /// an unauthenticated peer. Transport-level `IpcError::Io` skips the
    /// write because the socket is already torn.
    async fn write_frame_error<W>(connection_id: Uuid, stream: &mut W, err: &IpcError)
    where
        W: tokio::io::AsyncWriteExt + Unpin,
    {
        let message: &'static str = match err {
            IpcError::FrameTooLarge { got } => {
                warn!(
                    ?connection_id,
                    got_bytes = got,
                    "ipc frame exceeded MAX_FRAME; closing connection"
                );
                "frame too large"
            }
            IpcError::Serde(serde_err) => {
                warn!(
                    ?connection_id,
                    error = %serde_err,
                    "ipc frame failed to deserialize; closing connection"
                );
                "malformed frame"
            }
            IpcError::Io(_) => return,
        };
        if let Err(write_err) = write_frame(
            stream,
            &Response::Error {
                message: message.into(),
            },
        )
        .await
        {
            debug!(
                ?connection_id,
                error = %write_err,
                "failed to write frame-error response before closing"
            );
        }
    }

    pub async fn serve(&self, listener: UnixListener) -> Result<()> {
        loop {
            let (stream, _peer) = listener.accept().await?;
            // One `connection_id` per accepted connection. ADR 0010
            // requires stream_ids to be connection-scoped, so the
            // tuple key `(connection_id, stream_id)` must be unique
            // even when two clients allocate the same stream_id.
            // Uuid::new_v4 is collision-safe within the daemon's
            // lifetime; the id is Copy so the spawned task owns it
            // without lifetime gymnastics.
            let connection_id = Uuid::new_v4();
            debug!(?connection_id, "accepted connection");
            let me = self.clone();
            tokio::spawn(async move {
                if let Err(e) = me.handle(connection_id, stream).await {
                    warn!(?connection_id, error = %e, "connection failed");
                }
            });
        }
    }

    async fn handle(&self, connection_id: Uuid, mut stream: UnixStream) -> Result<()> {
        // Drop guard: regardless of how this fn exits (success, error,
        // panic-unwinding), purge every StreamTracker entry the
        // connection registered. The per-verb streaming dispatch forks
        // register an entry while a stream is open, so this guard closes
        // the disconnect-leaks-entries failure mode when a client drops
        // mid-stream.
        struct PurgeOnDrop<'a> {
            tracker: &'a stream_tracker::StreamTracker,
            connection_id: Uuid,
        }
        impl Drop for PurgeOnDrop<'_> {
            fn drop(&mut self) {
                self.tracker.purge_connection(self.connection_id);
            }
        }
        let _purge = PurgeOnDrop {
            tracker: &self.stream_tracker,
            connection_id,
        };
        // The daemon accepts any number of `ProtocolInfo` probes before
        // authentication. The first non-probe frame must authenticate the peer;
        // anything else terminates the connection after one failure reply.
        let peer = loop {
            let first: Request = match read_frame(&mut stream).await {
                Ok(r) => r,
                Err(IpcError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(());
                }
                Err(e) => {
                    Self::write_frame_error(connection_id, &mut stream, &e).await;
                    return Err(e.into());
                }
            };
            match first {
                Request::ProtocolInfo => {
                    write_frame(
                        &mut stream,
                        &Response::ProtocolInfo {
                            info: covenant_ipc::protocol_info(),
                        },
                    )
                    .await?;
                    continue;
                }
                Request::Authenticate { token_b58 } => match self.authenticate(&token_b58).await {
                    Some(agent_id) => {
                        write_frame(
                            &mut stream,
                            &Response::Authenticated {
                                display: agent_id.display.clone(),
                            },
                        )
                        .await?;
                        break agent_id;
                    }
                    None => {
                        let reason = "unknown or revoked token";
                        let response = match self.record_auth_failure("ipc", reason).await {
                            Ok(()) => Response::AuthenticationFailed {
                                reason: reason.into(),
                            },
                            Err(_) => Response::Error {
                                message: "audit write failed; refusing to proceed".into(),
                            },
                        };
                        write_frame(&mut stream, &response).await?;
                        return Ok(());
                    }
                },
                _ => {
                    let reason = "first frame must be Authenticate";
                    let response = match self.record_auth_failure("ipc", reason).await {
                        Ok(()) => Response::AuthenticationFailed {
                            reason: reason.into(),
                        },
                        Err(_) => Response::Error {
                            message: "audit write failed; refusing to proceed".into(),
                        },
                    };
                    write_frame(&mut stream, &response).await?;
                    return Ok(());
                }
            }
        };

        loop {
            let req: Request = match read_frame(&mut stream).await {
                Ok(r) => r,
                Err(IpcError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(());
                }
                Err(e) => {
                    Self::write_frame_error(connection_id, &mut stream, &e).await;
                    return Err(e.into());
                }
            };
            // ADR 0010 slice 3.d streaming dispatch fork. v1 clients
            // never set prefer_stream; v2 clients that explicitly
            // request streaming (prefer_stream == Some(true)) route
            // through Server::stream_recent_memory, which emits
            // StreamEnvelope frames directly to the writer. Some(false)
            // is wire-distinct from None and means "I know about v2
            // streaming but want the v1 terminal shape this call" —
            // it must fall through to the respond + write_frame path.
            // Matching exactly on Some(true) keeps the contract; a
            // shortcut like prefer_stream.unwrap_or(false) routes
            // Some(false) into the streaming branch and breaks the
            // v1-compatible fallback. tier and limit are Copy so the
            // destructure borrows req — req stays owned for the
            // fallthrough self.respond(req, &peer) call.
            if let Request::RecentMemory {
                tier,
                limit,
                prefer_stream: Some(true),
            } = &req
            {
                self.stream_recent_memory(&mut stream, connection_id, *tier, *limit, &peer)
                    .await?;
                continue;
            }
            // ADR 0010 slice 4.d streaming dispatch fork for RecentAudit.
            // Symmetric to the RecentMemory fork above; matches exactly
            // on Some(true) so Some(false) and None fall through to the
            // v1 respond + write_frame path. since_ms is Option<u64>
            // (Copy) and limit is usize (Copy), so the destructure
            // borrows req — req stays owned for the fallthrough.
            if let Request::RecentAudit {
                limit,
                since_ms,
                prefer_stream: Some(true),
            } = &req
            {
                self.stream_recent_audit(&mut stream, connection_id, *limit, *since_ms, &peer)
                    .await?;
                continue;
            }
            // ADR 0010 slice 5.d streaming dispatch fork for SubmitIntent.
            // Symmetric to the RecentMemory/RecentAudit forks above but
            // text is String (not Copy) — the destructure borrows via
            // ref text and the call clones it. req stays owned for the
            // v1 fallthrough so a Some(false)/None client gets the v1
            // Response::IntentResult shape.
            if let Request::SubmitIntent {
                text,
                prefer_stream: Some(true),
            } = &req
            {
                self.stream_submit_intent(&mut stream, connection_id, text.clone(), &peer)
                    .await?;
                continue;
            }
            let resp = self.respond(req, &peer).await;
            write_frame(&mut stream, &resp).await?;
        }
    }

    async fn authenticate(&self, token_b58: &str) -> Option<AgentId> {
        let token = PeerToken::from_b58(token_b58).ok()?;
        match self.peers.resolve(&token).await {
            Ok(found) => found,
            Err(e) => {
                // Distinguish storage failure from unknown token. The
                // caller still emits its standard wire-level auth-failed
                // rejection above this; the operator-side log gives them
                // the actual cause (a peer-registry file outage) so an
                // auth-failure spike doesn't get misread as a credential
                // probe. error! goes loud; the wire response stays
                // generic so attackers can't distinguish.
                error!(error = %e, "peer registry resolve failed during authenticate");
                None
            }
        }
    }

    pub async fn record_auth_failure(
        &self,
        transport: &str,
        reason: &str,
    ) -> Result<(), AuditError> {
        let event = AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: epoch_ms(),
            issuer: self.identity.agent_id(),
            kind: AuditKind::AuthenticationFailed {
                transport: transport.to_string(),
                reason: reason.to_string(),
            },
        };
        self.record_daemon_event_required(event).await
    }

    /// Record an audit event that represents an action by the
    /// authenticated `peer`. Asserts `event.issuer.pubkey == peer.pubkey`
    /// in debug builds and warns in release builds; the row is recorded
    /// either way (dropping it would hide the very regression the
    /// invariant is here to surface). Compare on the 32-byte pubkey, not
    /// the wire-supplied `display`.
    ///
    /// Fire-and-forget warn-and-continue posture. Use
    /// [`Self::record_peer_event_required`] for rejection-event kinds
    /// where the operator's view of the rejection depends on the row
    /// landing — those callers must propagate the `AuditError` to a
    /// `Response::Error` instead of returning the standard rejection.
    /// Debug builds assert that the kind is not in the must-record set.
    async fn record_peer_event(&self, peer: &AgentId, event: AuditEvent) {
        debug_assert_eq!(
            event.issuer.pubkey, peer.pubkey,
            "audit invariant: peer-action event.issuer.pubkey must equal authenticated peer.pubkey"
        );
        if event.issuer.pubkey != peer.pubkey {
            warn!(
                expected = %peer.display,
                got = %event.issuer.display,
                "audit invariant violated: peer-action event.issuer != peer"
            );
        }
        debug_assert!(
            !audit_kind_requires_persistence(&event.kind),
            "must-record audit kind routed through record_peer_event; use record_peer_event_required"
        );
        if let Err(e) = self.audit.record(event).await {
            warn!(error = %e, "audit record failed");
        }
    }

    /// Like [`Self::record_peer_event`] but surfaces the audit error to
    /// the caller. Used by rejection paths where the response itself is
    /// a security-relevant rejection (AuthenticationFailed,
    /// *RevokeRejected, *RotationRejected, *PeersListRejected,
    /// A2ASenderMismatch, A2aRecipientRejected, BudgetExhausted) — if
    /// the row can't be persisted, the caller returns
    /// `Response::Error { message: "audit write failed; refusing to
    /// proceed" }` instead of the standard rejection, so an attacker
    /// who can fill the audit disk cannot suppress the probe rows the
    /// operator's `/audit/recent` view depends on.
    async fn record_peer_event_required(
        &self,
        peer: &AgentId,
        event: AuditEvent,
    ) -> Result<(), AuditError> {
        debug_assert_eq!(
            event.issuer.pubkey, peer.pubkey,
            "audit invariant: peer-action event.issuer.pubkey must equal authenticated peer.pubkey"
        );
        if event.issuer.pubkey != peer.pubkey {
            warn!(
                expected = %peer.display,
                got = %event.issuer.display,
                "audit invariant violated: peer-action event.issuer != peer"
            );
        }
        if let Err(e) = self.audit.record(event).await {
            error!(error = %e, "audit record failed on required kind; refusing to proceed");
            return Err(e);
        }
        Ok(())
    }

    /// Record an audit event the daemon emits on its own behalf — i.e.
    /// when no peer is authenticated (currently only
    /// `AuthenticationFailed`). Asserts the issuer matches
    /// `self.identity` to catch a future regression that routes a
    /// peer-action through this path. Same release-mode posture as
    /// [`Self::record_peer_event`].
    async fn record_daemon_event(&self, event: AuditEvent) {
        let identity_pubkey = self.identity.agent_id().pubkey;
        debug_assert_eq!(
            event.issuer.pubkey, identity_pubkey,
            "audit invariant: daemon-internal event.issuer.pubkey must equal self.identity.pubkey"
        );
        if event.issuer.pubkey != identity_pubkey {
            warn!(
                got = %event.issuer.display,
                "audit invariant violated: daemon event.issuer != daemon identity"
            );
        }
        debug_assert!(
            !audit_kind_requires_persistence(&event.kind),
            "must-record audit kind routed through record_daemon_event; use record_daemon_event_required"
        );
        if let Err(e) = self.audit.record(event).await {
            warn!(error = %e, "audit record failed");
        }
    }

    async fn record_daemon_event_required(&self, event: AuditEvent) -> Result<(), AuditError> {
        let identity_pubkey = self.identity.agent_id().pubkey;
        debug_assert_eq!(
            event.issuer.pubkey, identity_pubkey,
            "audit invariant: daemon-internal event.issuer.pubkey must equal self.identity.pubkey"
        );
        if event.issuer.pubkey != identity_pubkey {
            warn!(
                got = %event.issuer.display,
                "audit invariant violated: daemon event.issuer != daemon identity"
            );
        }
        if let Err(e) = self.audit.record(event).await {
            error!(error = %e, "audit record failed on required kind; refusing to proceed");
            return Err(e);
        }
        Ok(())
    }

    async fn save_budget_pause_checkpoint(&self, checkpoint: BudgetPauseCheckpoint) {
        let Some(store) = &self.budget_checkpoints else {
            return;
        };
        if let Err(e) = store.save_pause(checkpoint).await {
            warn!(error = %e, "budget pause checkpoint save failed");
        }
    }

    async fn remember_active_budget_checkpoint(&self, checkpoint: BudgetPauseCheckpoint) {
        self.active_budget_pauses
            .lock()
            .await
            .insert(checkpoint.intent_id, checkpoint);
    }

    async fn clear_active_budget_checkpoint(&self, intent_id: Uuid) {
        self.active_budget_pauses.lock().await.remove(&intent_id);
    }

    pub async fn save_shutdown_budget_checkpoints(&self) -> usize {
        let Some(store) = &self.budget_checkpoints else {
            return 0;
        };
        let checkpoints: Vec<BudgetPauseCheckpoint> = self
            .active_budget_pauses
            .lock()
            .await
            .values()
            .cloned()
            .collect();

        let mut saved = 0usize;
        for checkpoint in checkpoints {
            match store.save_pause(checkpoint).await {
                Ok(()) => saved += 1,
                Err(BudgetCheckpointError::AlreadyPaused(_)) => {}
                Err(e) => warn!(error = %e, "shutdown budget checkpoint save failed"),
            }
        }
        saved
    }

    pub async fn run_a2a_auto_retry_scheduler_once(
        &self,
        policy: covenant_a2a::A2AAutoRetryPolicy,
    ) -> Response {
        let peer = self.identity.agent_id();
        let response = self.retry_a2a_stale(policy, &peer).await;
        self.record_a2a_auto_retry_scheduler_scan(policy, &response)
            .await;
        response
    }

    async fn record_a2a_auto_retry_scheduler_scan(
        &self,
        policy: covenant_a2a::A2AAutoRetryPolicy,
        response: &Response,
    ) {
        let mut skipped_by_reason = BTreeMap::new();
        let (considered, requeued, skipped, error) = match response {
            Response::A2AAutoRetried { report } => {
                for skipped in &report.skipped {
                    *skipped_by_reason
                        .entry(skipped.reason.as_str().to_string())
                        .or_insert(0) += 1;
                }
                (
                    report.considered as u64,
                    report.requeued.len() as u64,
                    report.skipped.len() as u64,
                    None,
                )
            }
            Response::Error { message } => (0, 0, 0, Some(message.clone())),
            other => (0, 0, 0, Some(format!("unexpected response: {other:?}"))),
        };

        let event = AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: epoch_ms(),
            issuer: self.identity.agent_id(),
            kind: AuditKind::A2AAutoRetrySchedulerScan {
                enabled: policy.enabled,
                considered,
                requeued,
                skipped,
                skipped_by_reason,
                min_lease_age_ms: policy.min_lease_age_ms,
                max_attempts: policy.max_attempts,
                max_requeues: policy.max_requeues as u64,
                scan_limit: policy.scan_limit as u64,
                error,
            },
        };
        self.record_daemon_event(event).await;
    }

    pub async fn respond(&self, req: Request, peer: &AgentId) -> Response {
        match req {
            Request::Ping => Response::Pong,
            Request::ProtocolInfo => Response::ProtocolInfo {
                info: covenant_ipc::protocol_info(),
            },
            Request::Authenticate { token_b58 } => match self.authenticate(&token_b58).await {
                Some(agent_id) => Response::Authenticated {
                    display: agent_id.display,
                },
                None => Response::AuthenticationFailed {
                    reason: "unknown or revoked token".into(),
                },
            },
            Request::SubmitIntent {
                text,
                prefer_stream: _,
            } => self.dispatch_intent(Uuid::new_v4(), text, peer, true).await,
            Request::RecentMemory {
                tier,
                limit,
                prefer_stream: _,
            } => self.recent_memory(tier, limit, peer).await,
            Request::RecentReceipts { limit, since_ms } => {
                self.recent_receipts(limit, since_ms, peer).await
            }
            Request::ChainStatus => self.chain_status(),
            Request::SapStatus => self.sap_status(),
            Request::SapPublishAgent { manifest_json } => {
                self.sap_publish_agent(manifest_json).await
            }
            Request::FlushReceipts { limit } => self.flush_receipts(limit, peer).await,
            Request::ReceiptBatches { limit } => self.receipt_batches(limit, peer).await,
            Request::PayX402 {
                provider,
                endpoint,
                method,
                body,
                network,
                asset,
                per_call_cap,
                credits,
            } => {
                self.pay_x402(
                    provider,
                    endpoint,
                    method,
                    body,
                    network,
                    asset,
                    per_call_cap,
                    credits,
                    peer,
                )
                .await
            }
            Request::BackfillSettlementReceipts {
                dry_run,
                scope_pubkey,
            } => {
                self.backfill_settlement_receipts(dry_run, scope_pubkey, peer)
                    .await
            }
            Request::BackfillMemoryRecords {
                dry_run,
                scope_pubkey,
            } => {
                self.backfill_memory_records(dry_run, scope_pubkey, peer)
                    .await
            }
            Request::RecentCapabilities { limit } => self.recent_capabilities(limit, peer).await,
            Request::GrantCapability {
                action,
                scope,
                expires_at,
            } => self.grant_capability(action, scope, expires_at, peer).await,
            Request::RevokeCapability { signature_b58 } => {
                self.revoke_capability(signature_b58, peer).await
            }
            Request::SearchMemory {
                query,
                tier,
                limit,
                min_relevance,
            } => {
                self.search_memory(query, tier, limit, min_relevance, peer)
                    .await
            }
            Request::PurgeMemory { tier, before_ms } => {
                self.purge_memory(tier, before_ms, peer).await
            }
            Request::RepairMemory { request } => self.repair_memory(request, peer).await,
            Request::CompactMemory { request } => self.compact_memory(request, peer).await,
            Request::Verify { window } => self.verify_recent(window).await,
            Request::IgnoreCheck { text } => self.check_ignore(text),
            Request::ListTools => self.list_tools(),
            Request::CallTool { name, arguments } => self.call_tool(name, arguments, peer).await,
            Request::RecentAudit {
                limit,
                since_ms,
                prefer_stream: _,
            } => self.recent_audit(limit, since_ms, peer).await,
            Request::VerifyAuditIntegrity => self.verify_audit_integrity(peer).await,
            Request::PurgeAudit { before_ms } => self.purge_audit(before_ms, peer).await,
            Request::PurgeCapabilities { before_ms } => {
                self.purge_capabilities(before_ms, peer).await
            }
            Request::SendA2ATask { task } => self.send_a2a_task(task, peer).await,
            Request::TryRecvA2ATask => self.try_recv_a2a_task(peer).await,
            Request::PostA2AResult { result } => self.post_a2a_result(result, peer).await,
            Request::TryRecvA2AResult => self.try_recv_a2a_result(peer).await,
            Request::RecentA2ATasks { limit } => self.recent_a2a_tasks(limit, peer).await,
            Request::RecentA2AResults { limit } => self.recent_a2a_results(limit, peer).await,
            Request::A2AQueue {
                limit,
                min_lease_age_ms,
                deadline_within_ms,
                state_filter,
            } => {
                self.a2a_queue(
                    limit,
                    min_lease_age_ms,
                    deadline_within_ms,
                    state_filter,
                    peer,
                )
                .await
            }
            Request::RepairA2ATask { request } => self.repair_a2a_task(request, peer).await,
            Request::RetryA2AStale { policy } => self.retry_a2a_stale(policy, peer).await,
            Request::CompactA2A => self.compact_a2a(peer).await,
            Request::PurgePeers { before_ms } => self.purge_peers(before_ms, peer).await,
            Request::ResumeIntent { intent_id } => self.resume_intent(intent_id, peer).await,
            Request::RecentDebits { limit } => self.recent_debits(limit).await,
            Request::RotateOperatorToken => self.rotate_operator_token(peer).await,
            Request::ListPeers {
                limit,
                pubkey_prefix,
                status_filter,
            } => {
                self.list_peers(limit, pubkey_prefix, status_filter, peer)
                    .await
            }
            Request::RevokePeer {
                token_prefix,
                force,
                match_limit,
            } => {
                self.revoke_peer(token_prefix, force, match_limit, peer)
                    .await
            }
        }
    }

    async fn send_a2a_task(&self, task: covenant_a2a::A2ATask, peer: &AgentId) -> Response {
        if task.sender != *peer {
            let event = AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: peer.clone(),
                kind: AuditKind::A2ASenderMismatch {
                    peer_display: peer.display.clone(),
                    claimed_sender_display: task.sender.display.clone(),
                },
            };
            if let Err(e) = self.record_peer_event_required(peer, event).await {
                return audit_failure_response(e);
            }
            return Response::Error {
                message: format!(
                    "a2a send rejected: task.sender {:?} does not match \
                     authenticated peer {:?}",
                    task.sender.display, peer.display
                ),
            };
        }
        let task_id = task.id;
        let recipient = task.recipient.display.clone();
        let alternatives = task.recipient.scoped_action_alternatives("a2a.send");
        let display_action = alternatives[0].clone();
        let check = self
            .check_capabilities_any_of(
                format!("a2a-send:{recipient}"),
                vec![alternatives.to_vec()],
                peer,
            )
            .await;
        if !check.passed {
            return Response::Error {
                message: format!(
                    "a2a send to {recipient} requires capability {display_action:?}. \
                     Grant it with `covenant capabilities grant {display_action}`."
                ),
            };
        }
        let task_id_s = task_id.to_string();
        let recipient_b58 = task.recipient.pubkey_base58();
        let send_scope = A2aScopeRequest {
            peer_pubkey_b58: Some(&recipient_b58),
            task_id: Some(&task_id_s),
            lease_id: None,
            duplicate_risk: None,
        };
        match self.a2a_scope_check(&alternatives, peer, send_scope).await {
            Ok(A2aScopeCheck { allowed: true, .. }) => {}
            Ok(_) => {
                let reason = "peer_pubkey_b58 or task_id does not match capability scope";
                self.record_capability_scope_rejected(
                    peer,
                    format!("a2a-send:{recipient}"),
                    display_action.clone(),
                    reason,
                )
                .await;
                return Response::Error {
                    message: format!("a2a send rejected by capability scope: {reason}"),
                };
            }
            Err(reason) => {
                self.record_capability_scope_rejected(
                    peer,
                    format!("a2a-send:{recipient}"),
                    display_action.clone(),
                    reason.clone(),
                )
                .await;
                return Response::Error {
                    message: format!("a2a send rejected by invalid capability scope: {reason}"),
                };
            }
        }
        // Recipient admission gate: when sender ≠ recipient (cross-peer
        // send), the recipient peer must have granted `a2a.recv.<sender>`
        // to themselves. v0 single-peer is loopback (peer == recipient),
        // so the gate is a no-op there. The pubkey-byte compare defeats
        // display spoofing. The grant satisfies the gate under either
        // the sender's display or pubkey-b58 form.
        if peer.pubkey != task.recipient.pubkey {
            let recv_alternatives = peer.scoped_action_alternatives("a2a.recv");
            let recv_display_action = recv_alternatives[0].clone();
            let sender_b58 = peer.pubkey_base58();
            let recv_scope = A2aScopeRequest {
                peer_pubkey_b58: Some(&sender_b58),
                task_id: Some(&task_id_s),
                lease_id: None,
                duplicate_risk: None,
            };
            match self
                .recipient_has_recv_for(&task.recipient, &recv_alternatives, recv_scope)
                .await
            {
                Ok(A2aScopeCheck { allowed: true, .. }) => {}
                Ok(A2aScopeCheck {
                    has_matching_action: true,
                    ..
                }) => {
                    let reason =
                        "peer_pubkey_b58 or task_id does not match recipient capability scope";
                    self.record_capability_scope_rejected(
                        peer,
                        format!("a2a-recv-gate:{}", task.recipient.display),
                        recv_display_action.clone(),
                        reason,
                    )
                    .await;
                    return Response::Error {
                        message: format!(
                            "a2a send to {} rejected by recipient capability scope: {reason}",
                            task.recipient.display
                        ),
                    };
                }
                Err(reason) => {
                    self.record_capability_scope_rejected(
                        peer,
                        format!("a2a-recv-gate:{}", task.recipient.display),
                        recv_display_action.clone(),
                        reason.clone(),
                    )
                    .await;
                    return Response::Error {
                        message: format!(
                            "a2a send to {} rejected by invalid recipient capability scope: {reason}",
                            task.recipient.display
                        ),
                    };
                }
                Ok(A2aScopeCheck { .. }) => {
                    let event = AuditEvent {
                        id: Uuid::new_v4(),
                        timestamp_ms: epoch_ms(),
                        issuer: peer.clone(),
                        kind: AuditKind::A2ARecipientRejected {
                            sender_display: peer.display.clone(),
                            recipient_display: task.recipient.display.clone(),
                            action: recv_display_action.clone(),
                        },
                    };
                    if let Err(e) = self.record_peer_event_required(peer, event).await {
                        return audit_failure_response(e);
                    }
                    return Response::Error {
                        message: format!(
                            "a2a send to {} rejected: recipient has not granted \
                             capability {recv_display_action:?}",
                            task.recipient.display
                        ),
                    };
                }
            }
        }
        match self.mailbox.send_task(task).await {
            Ok(()) => Response::A2ATaskQueued { task_id },
            Err(e) => Response::Error {
                message: format!("a2a: {e}"),
            },
        }
    }

    /// Checks whether the capability store has a non-revoked,
    /// non-expired grant for `a2a.recv.<sender>` (under either the
    /// display or pubkey-b58 form — both equivalent action shapes are
    /// accepted) with `subject = recipient.pubkey`, and whether its
    /// signed scope admits this concrete task. Used by the recipient
    /// admission gate. The subject lookup keys on the 32-byte pubkey, not
    /// the wire-supplied display.
    async fn recipient_has_recv_for(
        &self,
        recipient: &AgentId,
        alternatives: &[String],
        request: A2aScopeRequest<'_>,
    ) -> Result<A2aScopeCheck, String> {
        self.a2a_scope_check_for_subject(recipient.pubkey, alternatives, request)
            .await
    }

    async fn try_recv_a2a_task(&self, peer: &AgentId) -> Response {
        match self.mailbox.try_recv_task_for(peer).await {
            Ok(task) => Response::A2ATaskOpt { task },
            Err(e) => Response::Error {
                message: format!("a2a: {e}"),
            },
        }
    }

    async fn post_a2a_result(
        &self,
        result: covenant_a2a::A2ATaskResult,
        peer: &AgentId,
    ) -> Response {
        let task_id = result.task_id;
        let sender = match self.mailbox.lookup_task_sender(task_id).await {
            Ok(Some(s)) => s,
            Ok(None) => {
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::A2AResultRejected {
                        task_id,
                        reason: "unknown_task".into(),
                    },
                };
                self.record_peer_event(peer, event).await;
                return Response::Error {
                    message: format!(
                        "a2a respond rejected: task_id {task_id} was never \
                         dispatched through this daemon"
                    ),
                };
            }
            Err(e) => {
                return Response::Error {
                    message: format!("a2a: {e}"),
                };
            }
        };
        let alternatives = sender.scoped_action_alternatives("a2a.respond");
        let display_action = alternatives[0].clone();
        let check = self
            .check_capabilities_any_of(
                format!("a2a-respond:{task_id}"),
                vec![alternatives.to_vec()],
                peer,
            )
            .await;
        if !check.passed {
            return Response::Error {
                message: format!(
                    "a2a respond to {} requires capability {display_action:?}. \
                     Grant it with `covenant capabilities grant {display_action}`.",
                    sender.display
                ),
            };
        }
        let sender_b58 = sender.pubkey_base58();
        let task_id_s = task_id.to_string();
        let respond_scope = A2aScopeRequest {
            peer_pubkey_b58: Some(&sender_b58),
            task_id: Some(&task_id_s),
            lease_id: None,
            duplicate_risk: None,
        };
        match self
            .a2a_scope_check(&alternatives, peer, respond_scope)
            .await
        {
            Ok(A2aScopeCheck { allowed: true, .. }) => {}
            Ok(_) => {
                let reason = "peer_pubkey_b58 or task_id does not match capability scope";
                self.record_capability_scope_rejected(
                    peer,
                    format!("a2a-respond:{task_id}"),
                    display_action.clone(),
                    reason,
                )
                .await;
                return Response::Error {
                    message: format!("a2a respond rejected by capability scope: {reason}"),
                };
            }
            Err(reason) => {
                self.record_capability_scope_rejected(
                    peer,
                    format!("a2a-respond:{task_id}"),
                    display_action.clone(),
                    reason.clone(),
                )
                .await;
                return Response::Error {
                    message: format!("a2a respond rejected by invalid capability scope: {reason}"),
                };
            }
        }
        match self.mailbox.send_result(result).await {
            Ok(()) => Response::A2AResultPosted { task_id },
            Err(e) => Response::Error {
                message: format!("a2a: {e}"),
            },
        }
    }

    async fn try_recv_a2a_result(&self, peer: &AgentId) -> Response {
        match self.mailbox.try_recv_result_for(peer).await {
            Ok(result) => Response::A2AResultOpt { result },
            Err(e) => Response::Error {
                message: format!("a2a: {e}"),
            },
        }
    }

    /// Returns A2A tasks where `peer` is either the sender or recipient.
    /// Bidirectional filter — a peer's natural view spans tasks they sent
    /// (their outbound queue) and tasks addressed to them (their inbox).
    /// The hard `task.sender == peer` send-time invariant means the
    /// sender direction is forge-resistant; recipient is wire-supplied
    /// at send time so an adversarial peer cannot craft a recipient match
    /// that wasn't already routed to them at send. Compared on the 32-byte
    /// pubkey, not the display string. Per-peer filter.
    async fn recent_a2a_tasks(&self, limit: usize, peer: &AgentId) -> Response {
        match self.mailbox.recent_tasks(limit).await {
            Ok(tasks) => {
                let tasks = tasks
                    .into_iter()
                    .filter(|t| t.sender.pubkey == peer.pubkey || t.recipient.pubkey == peer.pubkey)
                    .collect();
                Response::A2ATasks { tasks }
            }
            Err(e) => Response::Error {
                message: format!("a2a: {e}"),
            },
        }
    }

    /// Returns A2A results whose original task sender matches `peer`.
    /// Joins each `result.task_id` against `Mailbox::lookup_task_sender`
    /// (senders-map invariant: `senders[task_id] ==
    /// authenticated_peer_at_send`); rows whose lookup returns `None` (the
    /// task pre-dates the senders map, or was compacted) drop, matching
    /// `try_recv_a2a_result_for`'s posture. Lookup errors drop the row
    /// and warn — the operator dashboard prefers a missing row over a
    /// leaked one. Compared on the 32-byte pubkey.
    async fn recent_a2a_results(&self, limit: usize, peer: &AgentId) -> Response {
        let results = match self.mailbox.recent_results(limit).await {
            Ok(r) => r,
            Err(e) => {
                return Response::Error {
                    message: format!("a2a: {e}"),
                };
            }
        };
        let mut filtered = Vec::with_capacity(results.len());
        for result in results {
            match self.mailbox.lookup_task_sender(result.task_id).await {
                Ok(Some(sender)) if sender.pubkey == peer.pubkey => filtered.push(result),
                Ok(_) => {}
                Err(e) => {
                    warn!(error = %e, task_id = %result.task_id, "a2a: lookup_task_sender failed; dropping row");
                }
            }
        }
        Response::A2AResults { results: filtered }
    }

    async fn a2a_queue(
        &self,
        limit: usize,
        min_lease_age_ms: Option<u64>,
        deadline_within_ms: Option<u64>,
        state_filter: Option<covenant_a2a::A2ATaskQueueState>,
        peer: &AgentId,
    ) -> Response {
        let task_limit =
            if min_lease_age_ms.is_some() || deadline_within_ms.is_some() || state_filter.is_some()
            {
                usize::MAX
            } else {
                limit
            };
        let now_ms = epoch_ms();
        let tasks = match self.mailbox.task_queue(task_limit).await {
            Ok(tasks) => tasks
                .into_iter()
                .filter(|entry| a2a_entry_visible_to_peer(entry, peer))
                .filter(|entry| a2a_entry_matches_min_lease_age(entry, min_lease_age_ms, now_ms))
                .filter(|entry| {
                    a2a_entry_matches_deadline_within(entry, deadline_within_ms, now_ms)
                })
                .filter(|entry| a2a_entry_matches_state(entry, state_filter))
                .take(limit)
                .collect(),
            Err(e) => {
                return Response::Error {
                    message: format!("a2a: {e}"),
                };
            }
        };

        let results = match self.mailbox.recent_results(limit).await {
            Ok(r) => r,
            Err(e) => {
                return Response::Error {
                    message: format!("a2a: {e}"),
                };
            }
        };
        let mut filtered_results = Vec::with_capacity(results.len());
        for result in results {
            match self.mailbox.lookup_task_sender(result.task_id).await {
                Ok(Some(sender)) if sender.pubkey == peer.pubkey => filtered_results.push(result),
                Ok(_) => {}
                Err(e) => {
                    warn!(error = %e, task_id = %result.task_id, "a2a: lookup_task_sender failed; dropping row");
                }
            }
        }

        Response::A2AQueue {
            tasks,
            results: filtered_results,
        }
    }

    async fn repair_a2a_task(
        &self,
        request: covenant_a2a::A2ARepairRequest,
        peer: &AgentId,
    ) -> Response {
        let action = a2a_repair_action(&request.command);
        let required = format!("a2a.repair.{action}");
        let check = self
            .check_capabilities(
                format!("a2a-repair:{}", request.task_id),
                vec![required.clone()],
                peer,
            )
            .await;
        if !check.passed {
            return Response::Error {
                message: format!(
                    "a2a repair {action} requires capability {required:?}. Grant it with `covenant capabilities grant {required}`."
                ),
            };
        }

        let queue = match self.mailbox.task_queue(usize::MAX).await {
            Ok(queue) => queue,
            Err(e) => {
                return Response::Error {
                    message: format!("a2a: {e}"),
                };
            }
        };
        let visible = queue.iter().find(|entry| {
            entry.task.id == request.task_id && a2a_entry_visible_to_peer(entry, peer)
        });
        let Some(entry) = visible else {
            return Response::Error {
                message: format!(
                    "a2a repair rejected: task {} is not visible to the authenticated peer or is no longer queued",
                    request.task_id
                ),
            };
        };
        if entry.state != covenant_a2a::A2ATaskQueueState::InFlight {
            return Response::Error {
                message: format!(
                    "a2a repair rejected: task {} is not currently in flight",
                    request.task_id
                ),
            };
        }

        let task_id = request.task_id;
        let reason = request.reason.clone();
        let lease_id = a2a_repair_lease_id(&request.command);
        let duplicate_risk = a2a_duplicate_risk(&request.command).map(str::to_string);
        let task_id_s = task_id.to_string();
        let lease_id_s = lease_id.map(|id| id.to_string());
        let peer_pubkey_b58 = if peer.pubkey == entry.task.sender.pubkey {
            entry.task.recipient.pubkey_base58()
        } else {
            entry.task.sender.pubkey_base58()
        };
        let repair_scope = A2aScopeRequest {
            peer_pubkey_b58: Some(&peer_pubkey_b58),
            task_id: Some(&task_id_s),
            lease_id: lease_id_s.as_deref(),
            duplicate_risk: duplicate_risk.as_deref(),
        };
        let repair_actions = [required.clone()];
        match self
            .a2a_scope_check(&repair_actions, peer, repair_scope)
            .await
        {
            Ok(A2aScopeCheck { allowed: true, .. }) => {}
            Ok(_) => {
                let reason = "peer_pubkey_b58, task_id, lease_id, or duplicate_risk does not match capability scope";
                self.record_capability_scope_rejected(
                    peer,
                    format!("a2a-repair:{task_id}"),
                    required.clone(),
                    reason,
                )
                .await;
                return Response::Error {
                    message: format!("a2a repair rejected by capability scope: {reason}"),
                };
            }
            Err(reason) => {
                self.record_capability_scope_rejected(
                    peer,
                    format!("a2a-repair:{task_id}"),
                    required.clone(),
                    reason.clone(),
                )
                .await;
                return Response::Error {
                    message: format!("a2a repair rejected by invalid capability scope: {reason}"),
                };
            }
        }
        let action = action.to_string();

        match self.mailbox.repair_task(request).await {
            Ok(outcome) => {
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::A2ARepairApplied {
                        task_id,
                        action,
                        reason,
                        lease_id,
                        duplicate_risk,
                        attempt: outcome.attempt,
                    },
                };
                self.record_peer_event(peer, event).await;
                Response::A2ARepaired { outcome }
            }
            Err(e) => Response::Error {
                message: format!("a2a: {e}"),
            },
        }
    }

    async fn retry_a2a_stale(
        &self,
        policy: covenant_a2a::A2AAutoRetryPolicy,
        peer: &AgentId,
    ) -> Response {
        if peer.pubkey != self.identity.agent_id().pubkey {
            return Response::Error {
                message: "a2a auto retry requires the operator identity".into(),
            };
        }

        let required = "a2a.repair.requeue".to_string();
        if policy.enabled {
            let check = self
                .check_capabilities("a2a-auto-retry".into(), vec![required.clone()], peer)
                .await;
            if !check.passed {
                return Response::Error {
                    message: "a2a auto retry requires capability \"a2a.repair.requeue\". \
                         Grant it with `covenant capabilities grant a2a.repair.requeue`."
                        .into(),
                };
            }
        }

        let queue = match self.mailbox.task_queue(policy.scan_limit).await {
            Ok(queue) => queue,
            Err(e) => {
                return Response::Error {
                    message: format!("a2a: {e}"),
                };
            }
        };

        let now_ms = epoch_ms();
        let mut report = covenant_a2a::A2AAutoRetryReport::new(policy);
        for entry in queue {
            report.considered += 1;
            let task_id = entry.task.id;
            let attempt = entry.attempt;
            match covenant_a2a::evaluate_auto_retry(&entry, &policy, now_ms) {
                covenant_a2a::A2AAutoRetryDecision::Skip {
                    reason,
                    lease_age_ms,
                } => {
                    report.skipped.push(covenant_a2a::A2AAutoRetrySkipped {
                        task_id,
                        reason,
                        attempt,
                        lease_age_ms,
                    });
                }
                covenant_a2a::A2AAutoRetryDecision::Requeue {
                    lease_id,
                    lease_age_ms,
                    idempotency_key,
                } => {
                    if report.requeued.len() >= policy.max_requeues {
                        report.skipped.push(covenant_a2a::A2AAutoRetrySkipped {
                            task_id,
                            reason: covenant_a2a::A2AAutoRetrySkipReason::LimitReached,
                            attempt,
                            lease_age_ms: Some(lease_age_ms),
                        });
                        continue;
                    }

                    let task_id_s = task_id.to_string();
                    let lease_id_s = lease_id.to_string();
                    let peer_pubkey_b58 = if peer.pubkey == entry.task.sender.pubkey {
                        entry.task.recipient.pubkey_base58()
                    } else if peer.pubkey == entry.task.recipient.pubkey {
                        entry.task.sender.pubkey_base58()
                    } else {
                        entry.task.recipient.pubkey_base58()
                    };
                    let repair_scope = A2aScopeRequest {
                        peer_pubkey_b58: Some(&peer_pubkey_b58),
                        task_id: Some(&task_id_s),
                        lease_id: Some(&lease_id_s),
                        duplicate_risk: Some("idempotent"),
                    };
                    match self
                        .a2a_scope_check(std::slice::from_ref(&required), peer, repair_scope)
                        .await
                    {
                        Ok(A2aScopeCheck { allowed: true, .. }) => {}
                        Ok(_) => {
                            report.skipped.push(covenant_a2a::A2AAutoRetrySkipped {
                                task_id,
                                reason:
                                    covenant_a2a::A2AAutoRetrySkipReason::CapabilityScopeMismatch,
                                attempt,
                                lease_age_ms: Some(lease_age_ms),
                            });
                            continue;
                        }
                        Err(reason) => {
                            self.record_capability_scope_rejected(
                                peer,
                                format!("a2a-auto-retry:{task_id}"),
                                required.clone(),
                                reason,
                            )
                            .await;
                            report.skipped.push(covenant_a2a::A2AAutoRetrySkipped {
                                task_id,
                                reason:
                                    covenant_a2a::A2AAutoRetrySkipReason::CapabilityScopeMismatch,
                                attempt,
                                lease_age_ms: Some(lease_age_ms),
                            });
                            continue;
                        }
                    }

                    let request = covenant_a2a::A2ARepairRequest {
                        task_id,
                        command: covenant_a2a::A2ARepairCommand::Requeue {
                            lease_id: Some(lease_id),
                            duplicate_risk: covenant_a2a::A2ADuplicateRisk::Idempotent,
                        },
                        reason: "automatic retry policy requeued stale idempotent lease".into(),
                    };
                    match self.mailbox.repair_task(request).await {
                        Ok(outcome) => {
                            let event = AuditEvent {
                                id: Uuid::new_v4(),
                                timestamp_ms: epoch_ms(),
                                issuer: peer.clone(),
                                kind: AuditKind::A2ARepairApplied {
                                    task_id,
                                    action: "auto_requeue".into(),
                                    reason:
                                        "automatic retry policy requeued stale idempotent lease"
                                            .into(),
                                    lease_id: Some(lease_id),
                                    duplicate_risk: Some("idempotent".into()),
                                    attempt: outcome.attempt,
                                },
                            };
                            self.record_peer_event(peer, event).await;
                            report.requeued.push(covenant_a2a::A2AAutoRetryRequeued {
                                task_id,
                                lease_id,
                                attempt: outcome.attempt,
                                idempotency_key,
                            });
                        }
                        Err(e) => {
                            warn!(error = %e, task_id = %task_id, "a2a auto retry failed after eligibility check");
                            report.skipped.push(covenant_a2a::A2AAutoRetrySkipped {
                                task_id,
                                reason: covenant_a2a::A2AAutoRetrySkipReason::MissingLease,
                                attempt,
                                lease_age_ms: Some(lease_age_ms),
                            });
                        }
                    }
                }
            }
        }

        Response::A2AAutoRetried { report }
    }

    /// Daemon-side fan-out across `router.agents()` for the operator
    /// budget dashboard. Per-agent buckets live in [`BudgetLedger`] and
    /// the trait method takes a single agent — for the flat operator
    /// view we walk the registered cards (skipping zero-budget Phase-0
    /// manifests, same predicate as [`Self::register_agent_budgets`]),
    /// pull `limit` debits per agent, merge, sort newest-first, and
    /// truncate. Read-only; no capability gate, same posture as
    /// `RecentMemory` / `RecentReceipts` / `RecentAudit`.
    ///
    /// **No per-peer filter:** `BudgetDebit.agent` is the rate-limited
    /// *agent* (e.g. `research@agent`), not the dispatcher peer. The
    /// budget belongs to the agent and is shared across every peer that
    /// dispatches through it. Per-peer attribution requires extending
    /// `BudgetDebit` with `dispatched_by: Option<AgentId>` and threading
    /// it through `try_debit`; that lands when the budget itself becomes
    /// per-peer (Phase-1 multi-tenant migration). v0 single-peer makes
    /// the leak surface non-existent.
    async fn recent_debits(&self, limit: usize) -> Response {
        let mut all: Vec<covenant_budget::BudgetDebit> = Vec::new();
        for card in self.router.agents() {
            if card.manifest.settlement.budget_credits_per_hour == 0 {
                continue;
            }
            let agent = agent_id_for_card(card);
            match self.budget.recent_debits(&agent, limit).await {
                Ok(debits) => all.extend(debits),
                Err(e) => {
                    return Response::Error {
                        message: format!("budget: {e}"),
                    };
                }
            }
        }
        all.sort_by_key(|d| std::cmp::Reverse(d.at_ms));
        all.truncate(limit);
        Response::Debits { debits: all }
    }

    async fn compact_a2a(&self, peer: &AgentId) -> Response {
        let required = vec!["a2a.compact".to_string()];
        let check = self
            .check_capabilities("a2a:compact".into(), required, peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: "a2a compact requires capability \"a2a.compact\". \
                     Grant it with `covenant capabilities grant a2a.compact`."
                    .into(),
            };
        }
        match self.mailbox.compact().await {
            Ok(dropped) => Response::A2ACompacted { dropped },
            Err(e) => Response::Error {
                message: format!("a2a: {e}"),
            },
        }
    }

    /// Rotate the operator's bootstrap token.
    ///
    /// Order is load-bearing:
    /// 1. Mint a fresh `PeerToken`.
    /// 2. Read the current token off disk so we know which one to revoke.
    /// 3. Register the new entry under the operator's `AgentId`.
    /// 4. Write the new token to `<home>/peers/operator.token` (mode 0600).
    /// 5. Revoke the old token in the registry.
    /// 6. Record an `OperatorTokenRotated` audit event.
    ///
    /// Crashing between (3) and (4) leaves the new token registered but
    /// not on disk; the next daemon boot reads the old (still-valid)
    /// token from disk and the orphan registry entry is harmless. The
    /// inverse — write-disk-before-register — would expose a window where
    /// the on-disk token resolves to nothing, locking the operator out.
    ///
    /// Gated to the operator's own identity — `peer.pubkey ==
    /// self.identity.pubkey`. v0 has only one peer (the operator), so any
    /// authenticated caller would pass a `peers.rotate` capability check
    /// anyway; the identity gate is the right invariant going into Phase-1
    /// multi-peer where a guest peer must not rotate the operator's token.
    async fn rotate_operator_token(&self, peer: &AgentId) -> Response {
        let identity_pubkey = self.identity.agent_id().pubkey;
        if peer.pubkey != identity_pubkey {
            // Surface the rejected attempt in the audit log so probes
            // are visible to the operator. Issuer is the daemon identity
            // (matching `AuthenticationFailed`) so the row passes the
            // operator-feed filter (`issuer.pubkey == peer.pubkey` where
            // peer == operator on the operator's `/audit` call). The
            // rejected peer's identity is preserved in the kind payload
            // — `peer_pubkey_b58` is the unforgeable identifier because
            // `.display` is wire-supplied and a colliding display string
            // is exactly the kind of probe this row exists to surface.
            // The natural mirror of `A2ARecipientRejected` gets the
            // audience wrong here: the operator is the security audience,
            // not the rejected peer.
            let event = AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: self.identity.agent_id(),
                kind: AuditKind::OperatorTokenRotationRejected {
                    peer_display: peer.display.clone(),
                    peer_pubkey_b58: bs58::encode(peer.pubkey).into_string(),
                },
            };
            if let Err(e) = self.record_daemon_event_required(event).await {
                return audit_failure_response(e);
            }
            return Response::Error {
                message: "operator token rotation requires the operator identity".into(),
            };
        }
        let Some(home) = self.home.clone() else {
            return Response::Error {
                message:
                    "operator token rotation unavailable: server has no home directory configured"
                        .into(),
            };
        };
        let token_path = home.join("peers").join("operator.token");

        let old_token = match read_operator_token_b58(&token_path) {
            Ok(t) => t,
            Err(e) => {
                return Response::Error {
                    message: format!("read operator token at {}: {e}", token_path.display()),
                };
            }
        };

        let new_token = PeerToken::generate();
        let new_entry = PeerEntry {
            token: new_token,
            agent_id: peer.clone(),
            registered_at: epoch_ms(),
        };
        if let Err(e) = self.peers.register(new_entry).await {
            return Response::Error {
                message: format!("register new operator token: {e}"),
            };
        }
        if let Err(e) = write_operator_token_0600(&token_path, &new_token.to_b58()) {
            // Best-effort rollback: the new token is registered but the
            // on-disk write failed, so the old token still resolves and is
            // still on disk. Leave the new entry in the registry — it
            // costs one row and a future rotation (or peer purge) cleans
            // it up. Surfacing the error preserves operator agency.
            return Response::Error {
                message: format!("write new operator token to {}: {e}", token_path.display()),
            };
        }
        match self.peers.revoke(&old_token).await {
            Ok(_) => {}
            Err(e) => {
                // The new token is on disk and registered; the old token
                // failed to revoke. The operator's next boot reads the new
                // token from disk and authenticates fine; the old one is
                // still live as a registry record but the operator no
                // longer has its bytes. Audit + warn rather than error
                // out — the rotation succeeded for every observable use.
                warn!(error = %e, "rotate: revoke old token failed; new token is live");
            }
        }

        let event = AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: epoch_ms(),
            issuer: peer.clone(),
            kind: AuditKind::OperatorTokenRotated {
                peer_display: peer.display.clone(),
                old_token_prefix: token_b58_prefix(&old_token),
                new_token_prefix: token_b58_prefix(&new_token),
            },
        };
        self.record_peer_event(peer, event).await;

        Response::OperatorTokenRotated {
            token_b58: new_token.to_b58(),
        }
    }

    async fn purge_peers(&self, before_ms: u64, peer: &AgentId) -> Response {
        let required = vec!["peers.purge".to_string()];
        let check = self
            .check_capabilities("peers:purge".into(), required, peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: "peers purge requires capability \"peers.purge\". \
                     Grant it with `covenant capabilities grant peers.purge`."
                    .into(),
            };
        }
        match self
            .peer_scope_check(
                "peers.purge",
                peer,
                PeerScopeRequest {
                    before_ms: Some(before_ms),
                    ..PeerScopeRequest::default()
                },
            )
            .await
        {
            Ok(PeerScopeCheck { allowed: true, .. }) => {}
            Ok(PeerScopeCheck { .. }) => {
                let reason = format!("before_ms {before_ms} exceeds capability scope");
                self.record_capability_scope_rejected(peer, "peers:purge", "peers.purge", &reason)
                    .await;
                return Response::Error {
                    message: format!("peers purge rejected by capability scope: {reason}"),
                };
            }
            Err(reason) => {
                self.record_capability_scope_rejected(peer, "peers:purge", "peers.purge", &reason)
                    .await;
                return Response::Error {
                    message: format!("peers purge rejected by invalid capability scope: {reason}"),
                };
            }
        }
        match self.peers.purge_revoked_older_than(before_ms).await {
            Ok(purged) => Response::PeersPurged { purged },
            Err(e) => Response::Error {
                message: format!("peers: {e}"),
            },
        }
    }

    /// Triage view of the peer registry.
    ///
    /// Closes the display-collision probe post-incident response gap:
    /// an `OperatorTokenRotationRejected` audit row carries
    /// `peer_pubkey_b58`, and the operator pastes that prefix into
    /// `covenant peers list --prefix <b58>` to identify which registry
    /// entry to revoke (or confirm already-revoked).
    ///
    /// The operator identity remains the root authority. Non-operator
    /// peers need `peers.list`; scoped grants must either be unscoped or
    /// match the concrete full `peer_pubkey_b58` requested through
    /// `pubkey_prefix`, so a narrow grant cannot enumerate unrelated
    /// registry rows.
    ///
    /// Missing-delegation rejection records `OperatorPeersListRejected` via
    /// `record_daemon_event` (issuer = daemon identity), mirroring the
    /// `OperatorTokenRotationRejected` audience model so the row passes
    /// the operator-feed filter and the rejected peer's `/audit` does
    /// not double as a probe-was-logged oracle.
    async fn list_peers(
        &self,
        limit: usize,
        pubkey_prefix: Option<String>,
        status_filter: Option<covenant_peer_auth::PeerStatusFilter>,
        peer: &AgentId,
    ) -> Response {
        let operator_pubkey = self.identity.agent_id().pubkey;
        if peer.pubkey != operator_pubkey {
            let check = self
                .check_capabilities("peers:list".into(), vec!["peers.list".into()], peer)
                .await;
            if !check.passed {
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: self.identity.agent_id(),
                    kind: AuditKind::OperatorPeersListRejected {
                        peer_display: peer.display.clone(),
                        peer_pubkey_b58: bs58::encode(peer.pubkey).into_string(),
                    },
                };
                if let Err(e) = self.record_daemon_event_required(event).await {
                    return audit_failure_response(e);
                }
                return Response::Error {
                    message:
                        "peers list requires the operator identity or capability \"peers.list\""
                            .into(),
                };
            }

            let peer_pubkey_b58 = bs58::encode(peer.pubkey).into_string();
            let self_target = pubkey_prefix
                .as_deref()
                .map(|prefix| prefix == peer_pubkey_b58);
            match self
                .peer_scope_check(
                    "peers.list",
                    peer,
                    PeerScopeRequest {
                        peer_pubkey_b58: pubkey_prefix.as_deref(),
                        self_target,
                        ..PeerScopeRequest::default()
                    },
                )
                .await
            {
                Ok(PeerScopeCheck { allowed: true, .. }) => {}
                Ok(PeerScopeCheck { .. }) => {
                    let reason = "peer_pubkey_b58 or self does not match capability scope";
                    self.record_capability_scope_rejected(peer, "peers:list", "peers.list", reason)
                        .await;
                    return Response::Error {
                        message: format!("peers list rejected by capability scope: {reason}"),
                    };
                }
                Err(reason) => {
                    self.record_capability_scope_rejected(
                        peer,
                        "peers:list",
                        "peers.list",
                        &reason,
                    )
                    .await;
                    return Response::Error {
                        message: format!(
                            "peers list rejected by invalid capability scope: {reason}"
                        ),
                    };
                }
            }
        }
        match self
            .peers
            .list_summaries(limit, pubkey_prefix.as_deref(), status_filter)
            .await
        {
            Ok((peers, truncated)) => Response::PeerList {
                peers,
                operator_pubkey_b58: bs58::encode(operator_pubkey).into_string(),
                truncated,
            },
            Err(e) => Response::Error {
                message: format!("peers: {e}"),
            },
        }
    }

    /// Revoke a single peer registry entry by token-prefix.
    ///
    /// Closes the post-incident response loop opened by `peers list` and
    /// the registry triage views: the operator pastes the 6-char
    /// `token_prefix` from `peers list`
    /// output (or any longer leading substring of the full base58 token)
    /// to tombstone exactly one entry. The five [`RevokeOutcome`] cases
    /// (Revoked / AlreadyRevoked / NotFound / Ambiguous /
    /// SelfRevokeForbidden) survive on the wire so the CLI can render
    /// each clearly without re-calling `peers list` for narrowing.
    ///
    /// `Ambiguous.matches` is bounded at the caller's `match_limit` if
    /// set, falling back to [`PEER_MATCH_LIMIT`] when `None`; when more
    /// than the cap matches, `Ambiguous.truncated` is `true` and the
    /// displayed list carries exactly the cap. The operator narrows by
    /// re-running with a longer prefix or by raising `--limit-matches`
    /// on the CLI.
    ///
    /// The operator identity remains the root authority. Non-operator
    /// peers need `peers.revoke`; scoped grants must admit the requested
    /// token prefix and force posture before the daemon can mutate the
    /// registry.
    ///
    /// Missing-delegation rejection records `OperatorPeerRevokeRejected` via
    /// `record_daemon_event` (issuer = daemon identity), mirroring the
    /// daemon-issuer audience model so the row passes the operator-feed
    /// filter and the rejected peer's `/audit` does not double as a
    /// probe-was-logged oracle. Only the `Revoked` outcome
    /// emits a `PeerRevoked` audit row (success); `NotFound`,
    /// `Ambiguous`, and `AlreadyRevoked` are operator-narrowing events,
    /// not security events, and the response itself is the operator's
    /// signal.
    ///
    /// Empty prefix is rejected with a specific `Response::Error` —
    /// otherwise the registry would return `Ambiguous { matches: <every
    /// entry> }`, which is operationally a footgun.
    ///
    /// Daemon-side self-revoke guard. After the C3 gate and empty-prefix
    /// check, the daemon peeks the registry via
    /// [`PeerRegistry::find_unique_live_by_token_prefix`]; if the unique
    /// live match's `agent_id.pubkey` equals
    /// `self.identity.agent_id().pubkey` (operator-identity-centric, not
    /// caller-centric — the predicate reads correctly in v0 and in
    /// Phase-1 multi-peer where a guest peer asks "are you trying to
    /// revoke yourself?" — wrong question) AND `force == false`, the
    /// daemon emits a `PeerSelfRevokeBlocked` audit row via
    /// `record_peer_event` (issuer = peer = operator, distinct from
    /// `OperatorPeerRevokeRejected`'s daemon-issuer audience because
    /// here the operator IS both issuer and audience — a self-fat-finger,
    /// not a probe) and returns `RevokeOutcome::SelfRevokeForbidden`
    /// without mutating. The CLI's preferred recovery path is `peers
    /// rotate`; `--force` exists for the "deliberately brick auth to
    /// test the recovery flow" use case. The peek's TOCTOU is benign
    /// because `self.identity.agent_id().pubkey` is stable across token
    /// rotation (rotation rotates the token, not the keypair).
    async fn revoke_peer(
        &self,
        token_prefix: String,
        force: bool,
        match_limit: Option<usize>,
        peer: &AgentId,
    ) -> Response {
        let operator_pubkey = self.identity.agent_id().pubkey;
        if peer.pubkey != operator_pubkey {
            let check = self
                .check_capabilities("peers:revoke".into(), vec!["peers.revoke".into()], peer)
                .await;
            if !check.passed {
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: self.identity.agent_id(),
                    kind: AuditKind::OperatorPeerRevokeRejected {
                        peer_display: peer.display.clone(),
                        peer_pubkey_b58: bs58::encode(peer.pubkey).into_string(),
                    },
                };
                if let Err(e) = self.record_daemon_event_required(event).await {
                    return audit_failure_response(e);
                }
                return Response::Error {
                    message:
                        "peer revoke requires the operator identity or capability \"peers.revoke\""
                            .into(),
                };
            }
        }
        if token_prefix.is_empty() {
            return Response::Error {
                message: "peer revoke requires a non-empty token prefix".into(),
            };
        }
        if matches!(match_limit, Some(0)) {
            // Defence-in-depth against the bypass-CLI footgun: the CLI
            // rejects 0 client-side, but a direct HTTP caller (curl with
            // the operator bearer) could send `{"match_limit":0}` and
            // silently collapse `Ambiguous` detection — `take(0 + 1)`
            // would collect a single arbitrary match and the unique-match
            // path would tombstone the wrong row. Mirrors the empty-prefix
            // rejection above; same posture, same response shape.
            return Response::Error {
                message: "peer revoke match_limit must be at least 1".into(),
            };
        }
        if peer.pubkey != operator_pubkey {
            match self
                .peer_scope_check(
                    "peers.revoke",
                    peer,
                    PeerScopeRequest {
                        token_prefix: Some(&token_prefix),
                        force: Some(force),
                        ..PeerScopeRequest::default()
                    },
                )
                .await
            {
                Ok(PeerScopeCheck { allowed: true, .. }) => {}
                Ok(PeerScopeCheck { .. }) => {
                    let reason = "token_prefix or force does not match capability scope";
                    self.record_capability_scope_rejected(
                        peer,
                        "peers:revoke",
                        "peers.revoke",
                        reason,
                    )
                    .await;
                    return Response::Error {
                        message: format!("peer revoke rejected by capability scope: {reason}"),
                    };
                }
                Err(reason) => {
                    self.record_capability_scope_rejected(
                        peer,
                        "peers:revoke",
                        "peers.revoke",
                        &reason,
                    )
                    .await;
                    return Response::Error {
                        message: format!(
                            "peer revoke rejected by invalid capability scope: {reason}"
                        ),
                    };
                }
            }
        }
        if !force {
            match self
                .peers
                .find_unique_live_by_token_prefix(&token_prefix)
                .await
            {
                Ok(Some(summary)) if summary.agent_id.pubkey == self.identity.agent_id().pubkey => {
                    let event = AuditEvent {
                        id: Uuid::new_v4(),
                        timestamp_ms: epoch_ms(),
                        issuer: peer.clone(),
                        kind: AuditKind::PeerSelfRevokeBlocked {
                            peer_display: summary.agent_id.display.clone(),
                            peer_pubkey_b58: bs58::encode(summary.agent_id.pubkey).into_string(),
                            token_prefix: summary.token_prefix.clone(),
                        },
                    };
                    self.record_peer_event(peer, event).await;
                    return Response::PeerRevoked {
                        outcome: RevokeOutcome::SelfRevokeForbidden(summary),
                    };
                }
                Ok(_) => {}
                Err(e) => {
                    return Response::Error {
                        message: format!("peers: {e}"),
                    };
                }
            }
        }
        let limit = match_limit.unwrap_or(PEER_MATCH_LIMIT);
        let outcome = match self
            .peers
            .revoke_by_token_prefix(&token_prefix, limit)
            .await
        {
            Ok(o) => o,
            Err(e) => {
                return Response::Error {
                    message: format!("peers: {e}"),
                };
            }
        };
        if let RevokeOutcome::Revoked(summary) = &outcome {
            let event = AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: peer.clone(),
                kind: AuditKind::PeerRevoked {
                    peer_display: summary.agent_id.display.clone(),
                    peer_pubkey_b58: bs58::encode(summary.agent_id.pubkey).into_string(),
                    token_prefix: summary.token_prefix.clone(),
                },
            };
            self.record_peer_event(peer, event).await;
        }
        Response::PeerRevoked { outcome }
    }

    /// Returns audit rows whose `issuer.pubkey` matches `peer.pubkey`.
    /// Filtering at the Server boundary (not in the storage trait) keeps
    /// `AuditLog` peer-agnostic and lets every read surface re-use the
    /// same predicate. Compared on the 32-byte pubkey, not the display
    /// string, because the display can be re-used across pubkeys at the
    /// wire boundary even with `validate_agent_id_display`.
    /// In v0 every authenticated caller is the operator and `peer.pubkey
    /// == identity.pubkey`, so the filter degenerates to a no-op — the
    /// behaviour change matters only once a second peer authenticates.
    /// `AuthenticationFailed` rows have `issuer == identity` (no
    /// authenticated peer at the moment of rejection) and so naturally
    /// remain visible only to the operator.
    async fn recent_audit(&self, limit: usize, since_ms: Option<u64>, peer: &AgentId) -> Response {
        let read_limit = if since_ms.is_some() {
            usize::MAX
        } else {
            limit
        };
        match self.audit.recent(read_limit).await {
            Ok(events) => {
                let mut filtered: Vec<AuditEvent> = events
                    .into_iter()
                    .filter(|e| e.issuer.pubkey == peer.pubkey)
                    .filter(|e| match since_ms {
                        Some(threshold) => e.timestamp_ms >= threshold,
                        None => true,
                    })
                    .collect();
                let start = filtered.len().saturating_sub(limit);
                let events = filtered.split_off(start);
                Response::AuditEvents { events }
            }
            Err(e) => Response::Error {
                message: format!("audit: {e}"),
            },
        }
    }

    async fn purge_audit(&self, before_ms: u64, peer: &AgentId) -> Response {
        let required = vec!["audit.purge".to_string()];
        let check = self
            .check_capabilities("audit:purge".into(), required, peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: "audit purge requires capability \"audit.purge\". \
                     Grant it with `covenant capabilities grant audit.purge`."
                    .into(),
            };
        }
        match self.audit_purge_scope_allows(before_ms, peer).await {
            Ok(true) => {}
            Ok(false) => {
                let reason = format!("before_ms {before_ms} exceeds capability scope");
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::CapabilityScopeRejected {
                        agent_id: "audit:purge".into(),
                        action: "audit.purge".into(),
                        reason: reason.clone(),
                    },
                };
                self.record_peer_event(peer, event).await;
                return Response::Error {
                    message: format!("audit purge rejected by capability scope: {reason}"),
                };
            }
            Err(reason) => {
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::CapabilityScopeRejected {
                        agent_id: "audit:purge".into(),
                        action: "audit.purge".into(),
                        reason: reason.clone(),
                    },
                };
                self.record_peer_event(peer, event).await;
                return Response::Error {
                    message: format!("audit purge rejected by invalid capability scope: {reason}"),
                };
            }
        }
        match self.audit.purge_older_than(before_ms).await {
            Ok(purged) => Response::AuditPurged { purged },
            Err(e) => Response::Error {
                message: format!("audit: {e}"),
            },
        }
    }

    async fn audit_purge_scope_allows(
        &self,
        before_ms: u64,
        peer: &AgentId,
    ) -> Result<bool, String> {
        let now = epoch_ms();
        let trust_root = self.identity.agent_id().pubkey;
        let user_caps = self
            .capabilities
            .list_for_subject(peer.pubkey)
            .await
            .map_err(|e| e.to_string())?;
        let mut invalid_scope = None;
        for cap in user_caps.iter().filter(|cap| {
            cap.capability.action == "audit.purge"
                && verify_with_clock_and_trust_root(cap, now, trust_root).is_ok()
        }) {
            match permission_audit_purge_scope_allows(
                &cap.capability.action,
                &cap.capability.scope,
                before_ms,
            ) {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(e) => {
                    invalid_scope.get_or_insert_with(|| e.to_string());
                }
            }
        }
        if let Some(reason) = invalid_scope {
            return Err(reason);
        }
        Ok(false)
    }

    async fn capabilities_purge_scope_allows(
        &self,
        before_ms: u64,
        peer: &AgentId,
    ) -> Result<bool, String> {
        let now = epoch_ms();
        let trust_root = self.identity.agent_id().pubkey;
        let user_caps = self
            .capabilities
            .list_for_subject(peer.pubkey)
            .await
            .map_err(|e| e.to_string())?;
        let mut invalid_scope = None;
        for cap in user_caps.iter().filter(|cap| {
            cap.capability.action == "capabilities.purge"
                && verify_with_clock_and_trust_root(cap, now, trust_root).is_ok()
        }) {
            match permission_capabilities_purge_scope_allows(
                &cap.capability.action,
                &cap.capability.scope,
                before_ms,
            ) {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(e) => {
                    invalid_scope.get_or_insert_with(|| e.to_string());
                }
            }
        }
        if let Some(reason) = invalid_scope {
            return Err(reason);
        }
        Ok(false)
    }

    async fn verify_audit_integrity(&self, peer: &AgentId) -> Response {
        if peer.pubkey != self.identity.agent_id().pubkey {
            return Response::Error {
                message: "audit integrity verification requires the operator identity".into(),
            };
        }
        match self.audit.verify_integrity().await {
            Ok(report) => Response::AuditIntegrity { report },
            Err(e) => Response::Error {
                message: format!("audit: {e}"),
            },
        }
    }

    async fn purge_capabilities(&self, before_ms: u64, peer: &AgentId) -> Response {
        let required = vec!["capabilities.purge".to_string()];
        let check = self
            .check_capabilities("capabilities:purge".into(), required, peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: "capabilities purge requires capability \"capabilities.purge\". \
                     Grant it with `covenant capabilities grant capabilities.purge`."
                    .into(),
            };
        }
        match self.capabilities_purge_scope_allows(before_ms, peer).await {
            Ok(true) => {}
            Ok(false) => {
                let reason = format!("before_ms {before_ms} exceeds capability scope");
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::CapabilityScopeRejected {
                        agent_id: "capabilities:purge".into(),
                        action: "capabilities.purge".into(),
                        reason: reason.clone(),
                    },
                };
                self.record_peer_event(peer, event).await;
                return Response::Error {
                    message: format!("capabilities purge rejected by capability scope: {reason}"),
                };
            }
            Err(reason) => {
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::CapabilityScopeRejected {
                        agent_id: "capabilities:purge".into(),
                        action: "capabilities.purge".into(),
                        reason: reason.clone(),
                    },
                };
                self.record_peer_event(peer, event).await;
                return Response::Error {
                    message: format!(
                        "capabilities purge rejected by invalid capability scope: {reason}"
                    ),
                };
            }
        }
        match self.capabilities.purge_revoked_older_than(before_ms).await {
            Ok(purged) => Response::CapabilitiesPurged { purged },
            Err(e) => Response::Error {
                message: format!("permissions: {e}"),
            },
        }
    }

    fn list_tools(&self) -> Response {
        let mut tools = self.tools.list_specs();
        if let Some(state) = &self.hyre {
            tools.extend(covenant_hyre::hyre_specs(&state.catalog, &state.config));
        }
        Response::ToolList { tools }
    }

    async fn call_tool(
        &self,
        name: String,
        arguments: serde_json::Value,
        peer: &AgentId,
    ) -> Response {
        let action = format!("tool.call.{name}");
        let required = vec![action.clone()];
        let check = self
            .check_capabilities(format!("tool:{name}"), required, peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: format!(
                    "tool {name} requires capability {:?}. Grant it with `covenant capabilities grant {}`.",
                    check.missing,
                    check.missing.first().cloned().unwrap_or_default()
                ),
            };
        }
        match self
            .tool_call_scope_allows(&action, &name, &arguments, peer)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                let reason = "arguments do not match capability scope".to_string();
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::CapabilityScopeRejected {
                        agent_id: format!("tool:{name}"),
                        action: action.clone(),
                        reason: reason.clone(),
                    },
                };
                self.record_peer_event(peer, event).await;
                return Response::Error {
                    message: format!("tool {name} rejected by capability scope: {reason}"),
                };
            }
            Err(reason) => {
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::CapabilityScopeRejected {
                        agent_id: format!("tool:{name}"),
                        action: action.clone(),
                        reason: reason.clone(),
                    },
                };
                self.record_peer_event(peer, event).await;
                return Response::Error {
                    message: format!("tool {name} rejected by invalid capability scope: {reason}"),
                };
            }
        }
        if name.starts_with("hyre.") {
            return self.hyre_tool_call(name, arguments, peer).await;
        }

        match self.tools.call(&name, arguments).await {
            Ok(r) => Response::ToolResult {
                content: r.content,
                is_error: r.is_error,
            },
            Err(e) => Response::Error {
                message: format!("tool: {e}"),
            },
        }
    }

    /// Execute a Hyre tool on the caller's behalf. The `tool.call.<name>`
    /// capability and scope are already enforced by [`Self::call_tool`];
    /// this binds the caller as payer and runs the resolved call through
    /// the outbound x402 path, so the budget debit, settlement receipt,
    /// and audit event land against the agent that invoked the tool.
    async fn hyre_tool_call(
        &self,
        name: String,
        arguments: serde_json::Value,
        peer: &AgentId,
    ) -> Response {
        let Some(state) = self.hyre.clone() else {
            return Response::Error {
                message: "hyre provider is not enabled on this daemon.".into(),
            };
        };
        let Some(x402) = self.x402_dispatch.clone() else {
            return Response::Error {
                message: "hyre requires the x402 funding-key sidecar. \
                          Wire it via Server::with_x402_dispatch and restart."
                    .into(),
            };
        };

        let executor = Arc::new(hyre::DaemonHyreExecutor::new(
            self.settlement.clone(),
            self.audit.clone(),
            self.budget.clone(),
            x402,
            self.identity.agent_id(),
            peer.clone(),
        ));
        let Some(tool) = covenant_hyre::hyre_tool(&state.catalog, &state.config, &name, executor)
        else {
            return Response::Error {
                message: format!("unknown hyre tool: {name}"),
            };
        };
        match tool.call(arguments).await {
            Ok(r) => Response::ToolResult {
                content: r.content,
                is_error: r.is_error,
            },
            Err(e) => Response::Error {
                message: format!("tool: {e}"),
            },
        }
    }

    async fn tool_call_scope_allows(
        &self,
        action: &str,
        name: &str,
        arguments: &serde_json::Value,
        peer: &AgentId,
    ) -> Result<bool, String> {
        let now = epoch_ms();
        let trust_root = self.identity.agent_id().pubkey;
        let user_caps = self
            .capabilities
            .list_for_subject(peer.pubkey)
            .await
            .map_err(|e| e.to_string())?;
        let mut invalid_scope = None;
        for cap in user_caps.iter().filter(|cap| {
            cap.capability.action == action
                && verify_with_clock_and_trust_root(cap, now, trust_root).is_ok()
        }) {
            match permission_tool_call_scope_allows(
                &cap.capability.action,
                &cap.capability.scope,
                name,
                arguments,
            ) {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(e) => {
                    invalid_scope.get_or_insert_with(|| e.to_string());
                }
            }
        }
        if let Some(reason) = invalid_scope {
            return Err(reason);
        }
        Ok(false)
    }

    fn check_ignore(&self, text: String) -> Response {
        let v = self.ignore.check(&text);
        Response::IgnoreReport {
            ignored: v.ignored,
            matched_pattern: v.matched.map(|p| p.raw().trim().to_string()),
            rules_loaded: self.ignore.len(),
        }
    }

    /// Entry point for intent dispatch. Hermes runs (sandboxed coding builds)
    /// can take minutes — far past the front door's idle window — so when the
    /// routed agent is hermes and `allow_async` is set, the slow work is moved
    /// to a spawned task that records its outcome in `intent_outcomes`; the
    /// verb returns `status:"running"` immediately and the client polls
    /// `/intents/:id/result` while the audit step-trail accrues. Every other
    /// agent (and resume) runs synchronously. Split from `dispatch_intent_run`
    /// so the spawned task awaits a non-recursive future that resolves `Send`.
    async fn dispatch_intent(
        &self,
        intent_id: Uuid,
        text: String,
        peer: &AgentId,
        allow_async: bool,
    ) -> Response {
        if allow_async {
            let hermes_agent = self
                .router
                .route(&text)
                .and_then(|m| self.router.find_by_id(&m.agent_id))
                .filter(|c| c.manifest.agent.runtime == covenant_manifest::Runtime::Hermes)
                .map(|c| c.id.clone());
            if let Some(agent_id) = hermes_agent {
                if let Ok(mut store) = self.intent_outcomes.lock() {
                    store.insert_running(intent_id, &text, Some(agent_id));
                }
                let me = self.clone();
                let peer = peer.clone();
                tokio::spawn(async move {
                    let resp = me.dispatch_intent_run(intent_id, text, &peer).await;
                    if let Ok(mut store) = me.intent_outcomes.lock() {
                        store.complete(intent_id, &resp);
                    }
                });
                return Response::IntentResult {
                    intent_id,
                    status: "running".into(),
                    text: String::new(),
                    sources: Vec::new(),
                    settlement: None,
                };
            }
        }
        self.dispatch_intent_run(intent_id, text, peer).await
    }

    async fn dispatch_intent_run(&self, intent_id: Uuid, text: String, peer: &AgentId) -> Response {
        // Pre-allocated so the budget debit's `paired_receipt` and the
        // settlement receipt's `id` agree — joining the budget log to
        // the receipt log on this UUID matches 1:1 instead of producing
        // zero matches (security-review L1 closure).
        let receipt_id = Uuid::new_v4();
        let issued_at = epoch_ms();

        let issuer = peer.clone();

        let ignore_check = self.ignore.check(&text);
        if ignore_check.ignored {
            let matched_pattern = ignore_check
                .matched
                .map(|p| p.raw().trim().to_string())
                .unwrap_or_default();
            let event = AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: issuer.clone(),
                kind: AuditKind::IntentIgnored {
                    intent_id,
                    intent_text: text.clone(),
                    matched_pattern: matched_pattern.clone(),
                },
            };
            self.record_peer_event(peer, event).await;
            return Response::IntentResult {
                intent_id,
                status: "ignored".into(),
                text: format!("ignored by .covenantignore rule: {matched_pattern}"),
                sources: Vec::new(),
                settlement: None,
            };
        }

        let matched = self.router.route(&text);
        let card = matched
            .as_ref()
            .and_then(|m| self.router.find_by_id(&m.agent_id));

        let write_check = self
            .check_capabilities("memory:write".into(), vec!["memory.write".into()], peer)
            .await;
        if !write_check.passed {
            return Response::Error {
                message: "to send a task, your agents need permission to save to memory \
                     (requires capability \"memory.write\"). \
                     Run `covenant bootstrap` to grant the defaults, or \
                     `covenant capabilities grant memory.write`."
                    .into(),
            };
        }
        match self
            .memory_write_scope_allows(&intent_id.to_string(), MemoryTier::Working, issued_at, peer)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                let reason =
                    "record, tier, mode, or age does not match capability scope".to_string();
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::CapabilityScopeRejected {
                        agent_id: format!("memory-write:{intent_id}"),
                        action: "memory.write".into(),
                        reason: reason.clone(),
                    },
                };
                self.record_peer_event(peer, event).await;
                return Response::Error {
                    message: format!("memory write rejected by capability scope: {reason}"),
                };
            }
            Err(reason) => {
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::CapabilityScopeRejected {
                        agent_id: format!("memory-write:{intent_id}"),
                        action: "memory.write".into(),
                        reason: reason.clone(),
                    },
                };
                self.record_peer_event(peer, event).await;
                return Response::Error {
                    message: format!("memory write rejected by invalid capability scope: {reason}"),
                };
            }
        }

        let (text_out, sources_out, runtime_events) = if let Some(card) = card {
            let required = card
                .manifest
                .capabilities
                .required
                .iter()
                .filter(|action| action.as_str() != "memory.write")
                .cloned()
                .collect::<Vec<_>>();
            let check = self
                .check_capabilities(card.id.clone(), required, peer)
                .await;
            if !check.passed {
                return Response::Error {
                    message: format!(
                        "the agent \"{}\" isn't allowed to handle this task yet (missing capabilities: {:?}). \
                         Run `covenant bootstrap` to grant the defaults, or \
                         `covenant capabilities grant <action>` for each one.",
                        card.id, check.missing
                    ),
                };
            }
            // Phase-0 manifests still default `budget_credits_per_hour = 0`.
            // The `Settlement` rustdoc says "Phase 0 tolerates 0; enforced
            // from Phase 1." So skip the debit path entirely when the
            // manifest opts out of budget enforcement. Non-zero capacity
            // gets the full token-bucket gate.
            if card.manifest.settlement.budget_credits_per_hour > 0 {
                let agent = agent_id_for_card(card);
                let requested = intent_dispatch_credits();
                match self.budget.try_debit(&agent, requested, receipt_id).await {
                    Ok(()) => {
                        let tokens_remaining =
                            self.budget.tokens_remaining(&agent).await.unwrap_or_else(|e| {
                                warn!(agent = %card.id, error = %e, "budget token read failed after debit");
                                0
                            });
                        let checkpoint = budget_pause_checkpoint(
                            intent_id,
                            agent.clone(),
                            BudgetPauseReason::Shutdown,
                            requested,
                            tokens_remaining,
                            issued_at,
                            issued_at,
                            budget_resume_state(&text, &card.id, "active_dispatch"),
                        );
                        self.remember_active_budget_checkpoint(checkpoint).await;
                    }
                    Err(BudgetError::NoCapacity(_)) => {
                        // Manifest opts in to budget but the bucket was never
                        // seeded — operator forgot to call
                        // `register_agent_budgets`, or a hot-reload added the
                        // manifest without re-seeding. v0 still passes
                        // (don't block dispatch on a misconfigured daemon)
                        // but the bypass now lands in /audit/recent so the
                        // operator sees it.
                        warn!(
                            agent = %card.id,
                            "no budget capacity registered for agent; \
                             dispatching without debit (call register_agent_budgets at startup)"
                        );
                        let event = AuditEvent {
                            id: Uuid::new_v4(),
                            timestamp_ms: epoch_ms(),
                            issuer: issuer.clone(),
                            kind: AuditKind::BudgetUnseeded {
                                agent_display: agent.display.clone(),
                                intent_id,
                                requested,
                            },
                        };
                        self.record_peer_event(peer, event).await;
                    }
                    Err(BudgetError::Exhausted {
                        tokens_remaining,
                        refill_eta_ms,
                    }) => {
                        let checkpoint = budget_pause_checkpoint(
                            intent_id,
                            agent.clone(),
                            BudgetPauseReason::BudgetExhausted,
                            requested,
                            tokens_remaining,
                            refill_eta_ms,
                            epoch_ms(),
                            budget_resume_state(&text, &card.id, "budget_exhausted"),
                        );
                        self.save_budget_pause_checkpoint(checkpoint).await;
                        let event = AuditEvent {
                            id: Uuid::new_v4(),
                            timestamp_ms: epoch_ms(),
                            issuer: issuer.clone(),
                            kind: AuditKind::BudgetExhausted {
                                agent_display: agent.display.clone(),
                                intent_id,
                                intent_text: text.clone(),
                                requested,
                                tokens_remaining,
                                refill_eta_ms,
                            },
                        };
                        if let Err(e) = self.record_peer_event_required(peer, event).await {
                            return audit_failure_response(e);
                        }
                        // Wire response rounds tokens_remaining to a coarse
                        // bucket; the audit row above keeps the precise u64.
                        // Coarsening the wire response avoids leaking precise
                        // bucket levels across peers.
                        let coarse = round_tokens_remaining(tokens_remaining);
                        return Response::Error {
                            message: format!(
                                "agent {} budget exhausted: requested {requested} credit(s), \
                                 ≥{coarse} remaining, refill eta {refill_eta_ms}ms",
                                card.id
                            ),
                        };
                    }
                    Err(e) => {
                        warn!(agent = %card.id, error = %e, "budget debit failed");
                        return Response::Error {
                            message: format!("budget debit failed for {}: {e}", card.id),
                        };
                    }
                }
            }
            let intent = Intent {
                id: intent_id,
                text: text.clone(),
                issuer: issuer.clone(),
                issued_at,
                priority: Priority::Normal,
                parent: None,
            };
            let run_result = self.runner.run(card, &intent).await;
            self.clear_active_budget_checkpoint(intent_id).await;
            match run_result {
                Ok(result) => {
                    // Stash captured workspace files on the async outcome (no-op
                    // for the synchronous path, which has no outcome entry) so
                    // the UI can show a file tree / preview.
                    if !result.files.is_empty() {
                        if let Ok(mut store) = self.intent_outcomes.lock() {
                            store.set_files(intent_id, result.files);
                        }
                    }
                    (result.text, result.sources, result.runtime_events)
                }
                Err(e) => {
                    warn!(agent = %card.id, error = %e, "agent run failed");
                    return Response::Error {
                        message: format!("agent {} failed: {e}", card.id),
                    };
                }
            }
        } else {
            (
                format!("phase 0 echo (no agent matched): {text}"),
                Vec::new(),
                Vec::new(),
            )
        };

        let embedding = match self.embedder.embed(&text_out).await {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, embedder = %self.embedder.name(), "embed failed; storing without vector");
                Vec::new()
            }
        };
        let record = MemoryRecord {
            id: intent_id,
            tier: MemoryTier::Working,
            owner: issuer.clone(),
            text: text_out.clone(),
            embedding,
            metadata: serde_json::json!({
                "intent_text": text,
                "agent_id": card.map(|c| c.id.clone()),
                "status": "ok",
            }),
            created_at: issued_at,
            parent: None,
        };
        let memory_record_id = record.id;
        let bytes_written = record.text.len();
        if let Err(e) = self.memory.put(record).await {
            warn!(error = %e, "memory write failed");
        } else {
            let receipt = SettlementReceipt {
                id: receipt_id,
                payer: issuer.clone(),
                resource: ResourceKind::Memory,
                memory_record_id: Some(memory_record_id),
                credits_consumed: memory_write_credits(bytes_written),
                settled_at: epoch_ms(),
                chain: None,
                cluster: None,
                batch_id: None,
                merkle_root: None,
                tx_sig: None,
                slot: None,
                confirmed_at: None,
                onchain_sig: None,
            };
            if let Err(e) = self.settlement.record(receipt).await {
                warn!(error = %e, "settlement record failed");
            }
        }

        let audit_event = AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: epoch_ms(),
            issuer: issuer.clone(),
            kind: AuditKind::IntentDispatched {
                intent_id,
                intent_text: text.clone(),
                matched_agent: card.map(|c| c.id.clone()),
                result_hash_hex: hash_hex(text_out.as_bytes()),
                status: "ok".into(),
            },
        };
        self.record_peer_event(peer, audit_event).await;

        // Fold runtime-side events (currently only Hermes) into the
        // chain after the dispatch row so the audit log captures the
        // step trail under the same issuer that submitted the intent.
        for trace in runtime_events {
            let row = AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: issuer.clone(),
                kind: runtime_trace_to_audit_kind(intent_id, trace),
            };
            self.record_peer_event(peer, row).await;
        }

        Response::IntentResult {
            intent_id,
            status: "ok".into(),
            text: text_out,
            sources: sources_out,
            settlement: None,
        }
    }

    /// Re-dispatch a previously budget-rejected intent. The audit log's
    /// `BudgetExhausted` row carries the original `intent_text`, so the
    /// resume verb scans recent audit, finds the matching `intent_id`,
    /// and runs the text through `dispatch_intent` like any fresh
    /// `SubmitIntent` (capability check, ignore rules, and budget gate
    /// all re-run — the bucket may have refilled). Returns
    /// `Response::Error` if no `BudgetExhausted` row matches the supplied
    /// `intent_id`. Covers the "queue a resume" path for Phase-0
    /// single-shot agents (Phase-1 multi-step agents will need an actual
    /// checkpoint/restart mechanism on top of this).
    async fn resume_intent(&self, intent_id: Uuid, peer: &AgentId) -> Response {
        // Recent-audit window: 1024 events covers the typical operator
        // turnaround (a few minutes of feed) without scanning the whole
        // log. If the row has aged out, the operator must re-submit
        // with the original text via SubmitIntent.
        let window = 1024usize;
        let events = match self.audit.recent(window).await {
            Ok(es) => es,
            Err(e) => {
                warn!(error = %e, "audit recent failed during resume");
                return Response::Error {
                    message: format!("resume: audit read failed: {e}"),
                };
            }
        };
        // Filter to the resuming peer's own rows BEFORE the find_map.
        // Resuming someone else's `BudgetExhausted` would otherwise leak
        // their `intent_text` through `dispatch_intent`'s code path. Same
        // pubkey-equality predicate as `recent_audit`.
        let row = events
            .iter()
            .filter(|e| e.issuer.pubkey == peer.pubkey)
            .rev()
            .find_map(|e| match &e.kind {
                AuditKind::BudgetExhausted {
                    intent_id: row_id,
                    agent_display,
                    intent_text,
                    ..
                } if *row_id == intent_id => Some((agent_display.clone(), intent_text.clone())),
                _ => None,
            });
        match row {
            Some((agent_display, t)) => {
                if let Some(store) = &self.budget_checkpoints {
                    let Some(agent) = self.budget_agent_by_display(&agent_display) else {
                        return Response::Error {
                            message: format!(
                                "resume: no registered budget agent matches {agent_display:?}"
                            ),
                        };
                    };
                    match store.claim_resume(intent_id, &agent, epoch_ms()).await {
                        Ok(_) => {}
                        Err(BudgetCheckpointError::NotFound(_)) => {
                            warn!(
                                intent_id = %intent_id,
                                agent = %agent_display,
                                "resume checkpoint missing; falling back to legacy audit-row resume"
                            );
                        }
                        Err(BudgetCheckpointError::AlreadyResumed(_)) => {
                            return Response::Error {
                                message: format!(
                                    "resume: checkpoint for intent {intent_id} was already claimed"
                                ),
                            };
                        }
                        Err(e) => {
                            return Response::Error {
                                message: format!("resume: checkpoint claim failed: {e}"),
                            };
                        }
                    }
                }
                self.dispatch_intent(Uuid::new_v4(), t, peer, false).await
            }
            None => Response::Error {
                message: format!(
                    "resume: no BudgetExhausted audit row for intent {intent_id} \
                     within last {window} events"
                ),
            },
        }
    }

    fn budget_agent_by_display(&self, display: &str) -> Option<AgentId> {
        self.router
            .agents()
            .iter()
            .map(agent_id_for_card)
            .find(|agent| agent.display == display)
    }

    /// Capability check for plain (single-form) actions. Thin wrapper
    /// over [`Self::check_capabilities_any_of`] — each required action
    /// becomes a single-element alternative group.
    async fn check_capabilities(
        &self,
        scope_id: String,
        required: Vec<String>,
        peer: &AgentId,
    ) -> CapabilityCheckOutcome {
        let groups = required.into_iter().map(|a| vec![a]).collect();
        self.check_capabilities_any_of(scope_id, groups, peer).await
    }

    /// Capability check where each requirement is satisfied by **any**
    /// of a list of equivalent action forms — used for accept-both-shapes
    /// on peer-scoped actions (`a2a.send.<display>` is satisfied by
    /// either `a2a.send.<display>` or `a2a.send.<pubkey_b58>` per
    /// [`AgentId::scoped_action_alternatives`]).
    ///
    /// Audit attribution: when an alternative group has a granted match,
    /// `required_actions` records the form that actually matched; on a
    /// miss it records the first element of the group (the display form
    /// by helper convention) so operator-facing rendering stays
    /// display-form on the failure path.
    async fn check_capabilities_any_of(
        &self,
        scope_id: String,
        alternatives_per_required: Vec<Vec<String>>,
        peer: &AgentId,
    ) -> CapabilityCheckOutcome {
        let now = epoch_ms();
        if alternatives_per_required.is_empty() {
            return CapabilityCheckOutcome {
                passed: true,
                required: Vec::new(),
                missing: Vec::new(),
            };
        }
        let trust_root = self.identity.agent_id().pubkey;
        let user_caps = self
            .capabilities
            .list_for_subject(peer.pubkey)
            .await
            .unwrap_or_default();
        let valid_actions: Vec<String> = user_caps
            .iter()
            .filter(|c| verify_with_clock_and_trust_root(c, now, trust_root).is_ok())
            .map(|c| c.capability.action.clone())
            .collect();
        let mut required: Vec<String> = Vec::with_capacity(alternatives_per_required.len());
        let mut missing: Vec<String> = Vec::new();
        for group in &alternatives_per_required {
            let matched = group.iter().find(|a| valid_actions.iter().any(|v| v == *a));
            match matched {
                Some(form) => required.push(form.clone()),
                None => {
                    let canonical = group.first().cloned().unwrap_or_default();
                    required.push(canonical.clone());
                    missing.push(canonical);
                }
            }
        }
        let passed = missing.is_empty();
        let event = AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: now,
            issuer: peer.clone(),
            kind: AuditKind::CapabilityCheck {
                agent_id: scope_id,
                required_actions: required.clone(),
                missing_actions: missing.clone(),
                passed,
            },
        };
        self.record_peer_event(peer, event).await;
        CapabilityCheckOutcome {
            passed,
            required,
            missing,
        }
    }

    async fn record_capability_scope_rejected(
        &self,
        peer: &AgentId,
        agent_id: impl Into<String>,
        action: impl Into<String>,
        reason: impl Into<String>,
    ) {
        let event = AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: epoch_ms(),
            issuer: peer.clone(),
            kind: AuditKind::CapabilityScopeRejected {
                agent_id: agent_id.into(),
                action: action.into(),
                reason: reason.into(),
            },
        };
        self.record_peer_event(peer, event).await;
    }

    async fn a2a_scope_check_for_subject(
        &self,
        subject_pubkey: [u8; 32],
        alternatives: &[String],
        request: A2aScopeRequest<'_>,
    ) -> Result<A2aScopeCheck, String> {
        let now = epoch_ms();
        let trust_root = self.identity.agent_id().pubkey;
        let user_caps = self
            .capabilities
            .list_for_subject(subject_pubkey)
            .await
            .map_err(|e| e.to_string())?;
        let mut invalid_scope = None;
        let mut has_matching_action = false;

        for cap in user_caps.iter().filter(|cap| {
            alternatives
                .iter()
                .any(|action| action == &cap.capability.action)
                && verify_with_clock_and_trust_root(cap, now, trust_root).is_ok()
        }) {
            has_matching_action = true;
            match permission_a2a_scope_allows(
                &cap.capability.action,
                &cap.capability.scope,
                &cap.capability.action,
                request,
            ) {
                Ok(true) => {
                    return Ok(A2aScopeCheck {
                        allowed: true,
                        has_matching_action: true,
                    });
                }
                Ok(false) => {}
                Err(e) => {
                    invalid_scope.get_or_insert_with(|| e.to_string());
                }
            }
        }

        if let Some(reason) = invalid_scope {
            return Err(reason);
        }
        Ok(A2aScopeCheck {
            allowed: false,
            has_matching_action,
        })
    }

    async fn a2a_scope_check(
        &self,
        alternatives: &[String],
        peer: &AgentId,
        request: A2aScopeRequest<'_>,
    ) -> Result<A2aScopeCheck, String> {
        self.a2a_scope_check_for_subject(peer.pubkey, alternatives, request)
            .await
    }

    async fn peer_scope_check(
        &self,
        action: &str,
        peer: &AgentId,
        request: PeerScopeRequest<'_>,
    ) -> Result<PeerScopeCheck, String> {
        let now = epoch_ms();
        let trust_root = self.identity.agent_id().pubkey;
        let user_caps = self
            .capabilities
            .list_for_subject(peer.pubkey)
            .await
            .map_err(|e| e.to_string())?;
        let mut invalid_scope = None;
        let mut has_matching_action = false;

        for cap in user_caps.iter().filter(|cap| {
            cap.capability.action == action
                && verify_with_clock_and_trust_root(cap, now, trust_root).is_ok()
        }) {
            has_matching_action = true;
            match permission_peer_scope_allows(
                &cap.capability.action,
                &cap.capability.scope,
                action,
                request,
            ) {
                Ok(true) => {
                    return Ok(PeerScopeCheck {
                        allowed: true,
                        has_matching_action: true,
                    });
                }
                Ok(false) => {}
                Err(e) => {
                    invalid_scope.get_or_insert_with(|| e.to_string());
                }
            }
        }

        if let Some(reason) = invalid_scope {
            return Err(reason);
        }
        Ok(PeerScopeCheck {
            allowed: false,
            has_matching_action,
        })
    }

    async fn chain_scopes(
        &self,
        action: &str,
        peer: &AgentId,
        request: ChainScopeRequest<'_>,
    ) -> Result<Vec<(String, serde_json::Value)>, String> {
        let now = epoch_ms();
        let trust_root = self.identity.agent_id().pubkey;
        let user_caps = self
            .capabilities
            .list_for_subject(peer.pubkey)
            .await
            .map_err(|e| e.to_string())?;
        let mut scopes = Vec::new();
        let mut invalid_scope = None;

        for cap in user_caps.iter().filter(|cap| {
            cap.capability.action == action
                && verify_with_clock_and_trust_root(cap, now, trust_root).is_ok()
        }) {
            match permission_chain_scope_allows(
                &cap.capability.action,
                &cap.capability.scope,
                action,
                request,
            ) {
                Ok(true) => {
                    scopes.push((cap.capability.action.clone(), cap.capability.scope.clone()))
                }
                Ok(false) => {}
                Err(e) => {
                    invalid_scope.get_or_insert_with(|| e.to_string());
                }
            }
        }

        if scopes.is_empty() {
            if let Some(reason) = invalid_scope {
                return Err(reason);
            }
        }
        Ok(scopes)
    }

    async fn grant_capability(
        &self,
        action: String,
        scope: Option<serde_json::Value>,
        expires_at: Option<u64>,
        peer: &AgentId,
    ) -> Response {
        let granted_by = self.identity.agent_id();
        let scope = scope.unwrap_or_else(|| serde_json::json!({}));
        if let Err(e) = validate_scope(&action, &scope) {
            let reason = e.to_string();
            let event = AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: peer.clone(),
                kind: AuditKind::CapabilityGrantRejected {
                    subject_display: peer.display.clone(),
                    action: action.clone(),
                    reason: reason.clone(),
                },
            };
            self.record_peer_event(peer, event).await;
            return Response::Error {
                message: format!("permissions: {reason}"),
            };
        }
        let cap = Capability {
            subject: peer.clone(),
            action: action.clone(),
            scope,
            granted_by: granted_by.clone(),
            expires_at,
        };
        let signed = sign_capability(cap, self.identity.signing_key());
        let signature_b58 = bs58::encode(signed.signature).into_string();

        if let Err(e) = self.capabilities.record(signed.clone()).await {
            return Response::Error {
                message: format!("permissions: failed to record capability: {e}"),
            };
        }

        let event = AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: epoch_ms(),
            issuer: peer.clone(),
            kind: AuditKind::CapabilityGranted {
                subject_display: peer.display.clone(),
                action: action.clone(),
                granted_by_display: granted_by.display.clone(),
                signature_b58: signature_b58.clone(),
            },
        };
        self.record_peer_event(peer, event).await;

        Response::CapabilityGranted {
            signature_b58,
            subject_display: peer.display.clone(),
            action,
        }
    }

    /// Returns memory records owned by `peer`. `MemoryRecord.owner` is
    /// set to the authenticated peer in `dispatch_intent`, so the filter
    /// keys directly off the dispatch attribution. Compared on the
    /// 32-byte pubkey. Per-peer filter.
    async fn recent_memory(
        &self,
        tier: Option<MemoryTier>,
        limit: usize,
        peer: &AgentId,
    ) -> Response {
        let actions = memory_read_actions(tier);
        let check = self
            .check_capabilities_any_of("memory:recent".into(), vec![actions], peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: "memory read requires capability \"memory.read\" or a tier-specific memory.read.<tier> capability. \
                     Grant it with `covenant capabilities grant memory.read`."
                    .into(),
            };
        }

        let scopes = match self.memory_read_scopes(tier, peer).await {
            Ok(scopes) if !scopes.is_empty() => scopes,
            Ok(_) => {
                let reason =
                    "tier, record, mode, or age does not match capability scope".to_string();
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::CapabilityScopeRejected {
                        agent_id: "memory:recent".into(),
                        action: "memory.read".into(),
                        reason: reason.clone(),
                    },
                };
                self.record_peer_event(peer, event).await;
                return Response::Error {
                    message: format!("memory read rejected by capability scope: {reason}"),
                };
            }
            Err(reason) => {
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::CapabilityScopeRejected {
                        agent_id: "memory:recent".into(),
                        action: "memory.read".into(),
                        reason: reason.clone(),
                    },
                };
                self.record_peer_event(peer, event).await;
                return Response::Error {
                    message: format!("memory read rejected by invalid capability scope: {reason}"),
                };
            }
        };
        match self.memory.recent(tier, limit).await {
            Ok(records) => {
                let records = records
                    .into_iter()
                    .filter(|r| r.owner.pubkey == peer.pubkey)
                    .filter(|r| memory_read_record_allowed(&scopes, r))
                    .collect();
                Response::Memories { records }
            }
            Err(e) => Response::Error {
                message: format!("memory: {e}"),
            },
        }
    }

    /// ADR 0010 streaming orchestrator for `Request::RecentMemory`
    /// with `prefer_stream: Some(true)`. Wraps `Self::recent_memory`
    /// so capability and scope checks stay defined in one place: on
    /// `Response::Memories { records }`, allocates a fresh
    /// `stream_id`, registers a [`stream_tracker::StreamEntry`] with
    /// `verb = "RecentMemory"`, drives
    /// [`stream_dispatch::emit_memory_stream`] to write the
    /// `StreamBegin`/`StreamChunk*`/`StreamEnd` sequence, then
    /// unregisters the tracker entry regardless of the emit result.
    ///
    /// Any non-`Memories` variant — including `Response::Error` from
    /// the capability gate — is written as a v1-shape terminal
    /// `Response` frame and skips tracker bookkeeping entirely. ADR
    /// 0010 explicitly allows the daemon to decide per-request
    /// whether to honor the streaming preference; a capability
    /// failure surfaces as the same `Response::Error` the v1 client
    /// already handles, so a v2-aware client codepath stays
    /// identical between streamable and non-streamable verbs on the
    /// failure path.
    ///
    /// Register-before-emit, unregister-after-emit ordering is
    /// load-bearing: a future operator-snapshot endpoint reads
    /// in-flight streams from the tracker, so registration must
    /// happen before the first frame goes out and unregistration
    /// must run on every exit (Ok and Err alike). The emit result
    /// is captured into a local so an `?`-propagated error from
    /// `write_frame` does not skip the unregister.
    ///
    /// Wired into `Self::handle` by the ADR 0010 slice 3.d dispatch
    /// fork, which routes `RecentMemory { prefer_stream: Some(true) }`
    /// here. Staying method-level also keeps it reachable from unit
    /// tests without requiring an IPC handshake.
    pub async fn stream_recent_memory<W>(
        &self,
        writer: &mut W,
        connection_id: Uuid,
        tier: Option<MemoryTier>,
        limit: usize,
        peer: &AgentId,
    ) -> Result<(), IpcError>
    where
        W: tokio::io::AsyncWriteExt + Unpin,
    {
        let response = self.recent_memory(tier, limit, peer).await;
        let records = match response {
            Response::Memories { records } => records,
            other => return write_frame(writer, &other).await,
        };

        let stream_id = Uuid::new_v4();
        self.stream_tracker.register(
            connection_id,
            stream_id,
            stream_tracker::StreamEntry {
                verb: "RecentMemory".into(),
                schema: stream_dispatch::MEMORY_CHUNK_SCHEMA.into(),
                started_at_ms: epoch_ms(),
            },
        );
        let result = stream_dispatch::emit_memory_stream(writer, stream_id, &records).await;
        self.stream_tracker.unregister(connection_id, stream_id);
        result
    }

    /// ADR 0010 slice 6.d — Vec-based sibling of
    /// [`Self::stream_recent_memory`] for the HTTP SSE response path.
    ///
    /// The writer-based form is right for the IPC socket: the daemon
    /// writes length-prefixed JSON frames as they're emitted. The HTTP
    /// gateway can't use that contract directly because axum needs the
    /// streamed bytes assembled into a body before the response is
    /// returned. This method performs the same capability check,
    /// tracker register/unregister bracketing, and chunk construction
    /// but returns the `StreamBegin` / `StreamChunk*` / `StreamEnd`
    /// sequence as a `Vec<StreamEnvelope>`. The HTTP SSE route
    /// handlers encode each entry with
    /// [`crate::sse::encode_stream_envelope_as_sse`] and concatenate
    /// the bytes into the response body.
    ///
    /// The error arm is the daemon's "streaming refused, render this
    /// as a buffered response" signal, not a generic error. A
    /// `Response::Error` from the capability gate is returned as
    /// `Err(Response::Error)` so the HTTP handler can fall back to a
    /// regular JSON response with the same payload. A future
    /// unification slice can re-express the writer-based form as a
    /// wrapper around this method; for now the two methods coexist so
    /// integrated code is untouched.
    pub async fn recent_memory_envelopes(
        &self,
        tier: Option<MemoryTier>,
        limit: usize,
        peer: &AgentId,
        connection_id: Uuid,
    ) -> Result<Vec<StreamEnvelope>, Response> {
        let response = self.recent_memory(tier, limit, peer).await;
        let records = match response {
            Response::Memories { records } => records,
            other => return Err(other),
        };

        let stream_id = Uuid::new_v4();
        self.stream_tracker.register(
            connection_id,
            stream_id,
            stream_tracker::StreamEntry {
                verb: "RecentMemory".into(),
                schema: stream_dispatch::MEMORY_CHUNK_SCHEMA.into(),
                started_at_ms: epoch_ms(),
            },
        );

        let mut envelopes = Vec::with_capacity(records.len() + 2);
        envelopes.push(StreamEnvelope::StreamBegin {
            stream_id,
            response_kind: stream_dispatch::MEMORY_RESPONSE_KIND.to_string(),
        });
        for (sequence, record) in records.iter().enumerate() {
            let chunk = match serde_json::to_value(record) {
                Ok(v) => v,
                Err(e) => {
                    self.stream_tracker.unregister(connection_id, stream_id);
                    return Err(Response::Error {
                        message: format!("memory stream serialize: {e}"),
                    });
                }
            };
            envelopes.push(StreamEnvelope::StreamChunk {
                stream_id,
                sequence: sequence as u32,
                chunk,
            });
        }
        envelopes.push(StreamEnvelope::StreamEnd {
            stream_id,
            summary: None,
        });

        self.stream_tracker.unregister(connection_id, stream_id);
        Ok(envelopes)
    }

    /// ADR 0010 streaming orchestrator for `Request::RecentAudit`
    /// with `prefer_stream: Some(true)`. Symmetric with
    /// [`Self::stream_recent_memory`]: wraps `Self::recent_audit`
    /// so the peer-scoped filter and `since_ms` truncation stay
    /// defined in one place, then forks on the response variant.
    ///
    /// On `Response::AuditEvents { events }`, allocates a fresh
    /// `stream_id`, registers a [`stream_tracker::StreamEntry`] with
    /// `verb = "RecentAudit"` and
    /// `schema = stream_dispatch::AUDIT_CHUNK_SCHEMA`, drives
    /// [`stream_dispatch::emit_audit_stream`] through the caller's
    /// writer, then unregisters the tracker entry regardless of the
    /// emit result. Any other `Response` variant is written as a
    /// v1-shape terminal frame and skips tracker bookkeeping.
    ///
    /// Unlike memory, `recent_audit` has no capability gate — it
    /// filters by `peer.pubkey == event.issuer.pubkey` instead — so
    /// the orchestrator's "fresh, no rows" path returns
    /// `Response::AuditEvents { events: [] }` and produces the
    /// begin+end pair. There is no audit equivalent of the memory
    /// capability-failure path.
    ///
    /// Wired into `Self::handle` by the ADR 0010 slice 4.d dispatch
    /// fork, which routes `RecentAudit { prefer_stream: Some(true) }` here.
    pub async fn stream_recent_audit<W>(
        &self,
        writer: &mut W,
        connection_id: Uuid,
        limit: usize,
        since_ms: Option<u64>,
        peer: &AgentId,
    ) -> Result<(), IpcError>
    where
        W: tokio::io::AsyncWriteExt + Unpin,
    {
        let response = self.recent_audit(limit, since_ms, peer).await;
        let events = match response {
            Response::AuditEvents { events } => events,
            other => return write_frame(writer, &other).await,
        };

        let stream_id = Uuid::new_v4();
        self.stream_tracker.register(
            connection_id,
            stream_id,
            stream_tracker::StreamEntry {
                verb: "RecentAudit".into(),
                schema: stream_dispatch::AUDIT_CHUNK_SCHEMA.into(),
                started_at_ms: epoch_ms(),
            },
        );
        let result = stream_dispatch::emit_audit_stream(writer, stream_id, &events).await;
        self.stream_tracker.unregister(connection_id, stream_id);
        result
    }

    /// ADR 0010 slice 6.e — Vec-based sibling of
    /// [`Self::stream_recent_audit`] for the HTTP SSE response path.
    /// Symmetric with [`Self::recent_memory_envelopes`]; the contract
    /// is identical at the type level, with `Ok(envelopes)` for the
    /// streamable path and `Err(Response)` reserved for the daemon's
    /// "streaming refused, render as buffered" signal.
    ///
    /// Unlike memory, `recent_audit` has no capability gate — it
    /// filters by `peer.pubkey == event.issuer.pubkey` inside
    /// `Self::recent_audit` — so the `Err(Response::Error)` arm is
    /// unreachable in practice on this verb. The Result shape stays
    /// for symmetry so the HTTP route handlers use one
    /// common pattern across memory and audit. An empty page (no
    /// events visible to the peer) is a happy-path `Ok` with the
    /// begin+end pair (no chunks); a stream that never opens is never
    /// indistinguishable from a dead daemon at the protocol layer.
    pub async fn recent_audit_envelopes(
        &self,
        limit: usize,
        since_ms: Option<u64>,
        peer: &AgentId,
        connection_id: Uuid,
    ) -> Result<Vec<StreamEnvelope>, Response> {
        let response = self.recent_audit(limit, since_ms, peer).await;
        let events = match response {
            Response::AuditEvents { events } => events,
            other => return Err(other),
        };

        let stream_id = Uuid::new_v4();
        self.stream_tracker.register(
            connection_id,
            stream_id,
            stream_tracker::StreamEntry {
                verb: "RecentAudit".into(),
                schema: stream_dispatch::AUDIT_CHUNK_SCHEMA.into(),
                started_at_ms: epoch_ms(),
            },
        );

        let mut envelopes = Vec::with_capacity(events.len() + 2);
        envelopes.push(StreamEnvelope::StreamBegin {
            stream_id,
            response_kind: stream_dispatch::AUDIT_RESPONSE_KIND.to_string(),
        });
        for (sequence, event) in events.iter().enumerate() {
            let chunk = match serde_json::to_value(event) {
                Ok(v) => v,
                Err(e) => {
                    self.stream_tracker.unregister(connection_id, stream_id);
                    return Err(Response::Error {
                        message: format!("audit stream serialize: {e}"),
                    });
                }
            };
            envelopes.push(StreamEnvelope::StreamChunk {
                stream_id,
                sequence: sequence as u32,
                chunk,
            });
        }
        envelopes.push(StreamEnvelope::StreamEnd {
            stream_id,
            summary: None,
        });

        self.stream_tracker.unregister(connection_id, stream_id);
        Ok(envelopes)
    }

    /// ADR 0010 streaming orchestrator for `Request::SubmitIntent`
    /// with `prefer_stream: Some(true)`. Symmetric with
    /// [`Self::stream_recent_memory`] and [`Self::stream_recent_audit`]
    /// in structure: dispatches the intent via
    /// `Self::dispatch_intent` (which owns capability checks,
    /// ignore-rule enforcement, runner invocation, audit recording,
    /// memory writes, and budget metering), then forks on the
    /// response variant.
    ///
    /// On `Response::IntentResult { intent_id, status, text, sources,
    /// settlement }`, builds an [`AgentResult`] chunk carrying
    /// `text` and `sources` (and an empty `runtime_events` vec
    /// because `dispatch_intent` already folded runtime events into
    /// the audit chain — emitting them again here would double-publish
    /// on the wire). Packs `intent_id`, `status`, and `settlement`
    /// into a `serde_json::Value` summary because those fields are
    /// IntentResult-only bookkeeping that doesn't fit in an
    /// `AgentResult` chunk. Allocates a fresh `stream_id`, registers
    /// a [`stream_tracker::StreamEntry`] with `verb = "SubmitIntent"`
    /// and `schema = stream_dispatch::INTENT_RESULT_CHUNK_SCHEMA`,
    /// drives [`stream_dispatch::emit_intent_stream`] through the
    /// caller's writer, then unregisters the tracker entry regardless
    /// of the emit result. Any other `Response` variant (capability
    /// failure, ignore-rule match, budget exhaustion) is written as a
    /// v1-shape terminal frame and skips tracker bookkeeping —
    /// ADR 0010 explicitly allows daemon-decides-not-to-stream by
    /// falling back to v1 shape.
    ///
    /// Wired into `Self::handle` by the ADR 0010 slice 5.d dispatch
    /// fork, which routes `SubmitIntent { prefer_stream: Some(true) }` here.
    pub async fn stream_submit_intent<W>(
        &self,
        writer: &mut W,
        connection_id: Uuid,
        text: String,
        peer: &AgentId,
    ) -> Result<(), IpcError>
    where
        W: tokio::io::AsyncWriteExt + Unpin,
    {
        let response = self.dispatch_intent(Uuid::new_v4(), text, peer, true).await;
        let (result, summary) = match response {
            Response::IntentResult {
                intent_id,
                status,
                text,
                sources,
                settlement,
            } => {
                let result = AgentResult {
                    text,
                    sources,
                    runtime_events: Vec::new(),
                    files: Vec::new(),
                };
                let summary = serde_json::json!({
                    "intent_id": intent_id,
                    "status": status,
                    "settlement": settlement,
                });
                (result, summary)
            }
            other => return write_frame(writer, &other).await,
        };

        let stream_id = Uuid::new_v4();
        self.stream_tracker.register(
            connection_id,
            stream_id,
            stream_tracker::StreamEntry {
                verb: "SubmitIntent".into(),
                schema: stream_dispatch::INTENT_RESULT_CHUNK_SCHEMA.into(),
                started_at_ms: epoch_ms(),
            },
        );
        let emit_result =
            stream_dispatch::emit_intent_stream(writer, stream_id, &[result], Some(summary)).await;
        self.stream_tracker.unregister(connection_id, stream_id);
        emit_result
    }

    /// ADR 0010 slice 6.f — Vec-based sibling of
    /// [`Self::stream_submit_intent`] for the HTTP SSE response path.
    /// Symmetric with [`Self::recent_memory_envelopes`] and
    /// [`Self::recent_audit_envelopes`] in shape but specialized for
    /// the intent-result chunk transformation.
    ///
    /// On a successful `Response::IntentResult`, builds one
    /// [`AgentResult`] chunk carrying `text` and `sources` with an
    /// empty `runtime_events` Vec — `dispatch_intent` already folded
    /// runtime events into the audit chain, so re-emitting them in
    /// the chunk would double-publish on the wire. The `StreamEnd`
    /// carries a summary `serde_json::Value` packing `intent_id`,
    /// `status`, and `settlement`: IntentResult-only bookkeeping that
    /// doesn't fit in an AgentResult chunk.
    ///
    /// Any other `Response` variant (capability failure, ignore-rule
    /// match, budget exhaustion) returns `Err(Response)` so the HTTP
    /// handler renders a buffered JSON response with the same payload.
    ///
    /// The Vec is sized for the current single-chunk shape (begin + 1
    /// chunk + end). A future streaming runtime extension that emits
    /// multiple partial `AgentResult` chunks is its own slice and
    /// updates this allocation accordingly.
    pub async fn submit_intent_envelopes(
        &self,
        text: String,
        peer: &AgentId,
        connection_id: Uuid,
    ) -> Result<Vec<StreamEnvelope>, Response> {
        let response = self.dispatch_intent(Uuid::new_v4(), text, peer, true).await;
        let (result, summary) = match response {
            Response::IntentResult {
                intent_id,
                status,
                text,
                sources,
                settlement,
            } => {
                let result = AgentResult {
                    text,
                    sources,
                    runtime_events: Vec::new(),
                    files: Vec::new(),
                };
                let summary = serde_json::json!({
                    "intent_id": intent_id,
                    "status": status,
                    "settlement": settlement,
                });
                (result, summary)
            }
            other => return Err(other),
        };

        let stream_id = Uuid::new_v4();
        self.stream_tracker.register(
            connection_id,
            stream_id,
            stream_tracker::StreamEntry {
                verb: "SubmitIntent".into(),
                schema: stream_dispatch::INTENT_RESULT_CHUNK_SCHEMA.into(),
                started_at_ms: epoch_ms(),
            },
        );

        let mut envelopes = Vec::with_capacity(3);
        envelopes.push(StreamEnvelope::StreamBegin {
            stream_id,
            response_kind: stream_dispatch::INTENT_RESPONSE_KIND.to_string(),
        });
        let chunk = match serde_json::to_value(&result) {
            Ok(v) => v,
            Err(e) => {
                self.stream_tracker.unregister(connection_id, stream_id);
                return Err(Response::Error {
                    message: format!("intent stream serialize: {e}"),
                });
            }
        };
        envelopes.push(StreamEnvelope::StreamChunk {
            stream_id,
            sequence: 0,
            chunk,
        });
        envelopes.push(StreamEnvelope::StreamEnd {
            stream_id,
            summary: Some(summary),
        });

        self.stream_tracker.unregister(connection_id, stream_id);
        Ok(envelopes)
    }

    /// Returns settlement receipts where `peer` is the payer.
    /// `SettlementReceipt.payer` is set to the authenticated peer in
    /// `dispatch_intent`, so the filter keys directly off the dispatch
    /// attribution. Compared on the 32-byte pubkey.
    ///
    /// `since_ms` drops receipts whose `settled_at` is strictly less
    /// than the threshold. The store read window is expanded to
    /// `usize::MAX` when a threshold is set so the predicate applies
    /// before the final `limit` truncation — otherwise a recent burst
    /// could push older-but-still-in-window receipts out of the slice
    /// before the filter sees them.
    async fn recent_receipts(
        &self,
        limit: usize,
        since_ms: Option<u64>,
        peer: &AgentId,
    ) -> Response {
        let check = self
            .check_capabilities("chain:receipts".into(), vec!["chain.receipts".into()], peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: "receipt reads require capability \"chain.receipts\". \
                     Grant it with `covenant capabilities grant chain.receipts`."
                    .into(),
            };
        }
        // The COVNT mint is environment-level (receipts carry no mint field), so
        // a mint-bound scope can only be enforced here at the gather stage, the
        // way flush_receipts does it. Per-item dimensions (payer/resource/cluster/
        // batch_id) are enforced per receipt by chain_receipt_allowed below.
        let status = chain_status_from_env();
        let mint = status.covnt_mint.as_deref().unwrap_or("");
        let scopes = match self
            .chain_scopes(
                "chain.receipts",
                peer,
                ChainScopeRequest {
                    limit: Some(limit),
                    mint: Some(mint),
                    ..ChainScopeRequest::default()
                },
            )
            .await
        {
            Ok(scopes) if !scopes.is_empty() => scopes,
            Ok(_) => {
                let reason = format!("limit {limit} or mint does not match capability scope");
                self.record_capability_scope_rejected(
                    peer,
                    "chain:receipts",
                    "chain.receipts",
                    &reason,
                )
                .await;
                return Response::Error {
                    message: format!("receipt reads rejected by capability scope: {reason}"),
                };
            }
            Err(reason) => {
                self.record_capability_scope_rejected(
                    peer,
                    "chain:receipts",
                    "chain.receipts",
                    &reason,
                )
                .await;
                return Response::Error {
                    message: format!(
                        "receipt reads rejected by invalid capability scope: {reason}"
                    ),
                };
            }
        };
        let read_limit = if since_ms.is_some() {
            usize::MAX
        } else {
            limit
        };
        match self.settlement.recent(read_limit).await {
            Ok(receipts) => {
                let mut filtered: Vec<SettlementReceipt> = receipts
                    .into_iter()
                    .filter(|r| r.payer.pubkey == peer.pubkey)
                    .filter(|r| chain_receipt_allowed(&scopes, r))
                    .filter(|r| match since_ms {
                        Some(threshold) => r.settled_at >= threshold,
                        None => true,
                    })
                    .collect();
                let start = filtered.len().saturating_sub(limit);
                let receipts = filtered.split_off(start);
                Response::Receipts { receipts }
            }
            Err(e) => Response::Error {
                message: format!("settlement: {e}"),
            },
        }
    }

    fn chain_status(&self) -> Response {
        Response::ChainStatus {
            status: chain_status_from_env(),
        }
    }

    async fn flush_receipts(&self, limit: usize, peer: &AgentId) -> Response {
        if peer.pubkey != self.identity.agent_id().pubkey {
            return Response::Error {
                message: "receipt flushing requires the operator identity".into(),
            };
        }
        let check = self
            .check_capabilities("chain:flush".into(), vec!["chain.flush".into()], peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: "receipt flushing requires capability \"chain.flush\". \
                     Grant it with `covenant capabilities grant chain.flush`."
                    .into(),
            };
        }
        let status = chain_status_from_env();
        let mint = status.covnt_mint.as_deref().unwrap_or("");
        let scopes = match self
            .chain_scopes(
                "chain.flush",
                peer,
                ChainScopeRequest {
                    limit: Some(limit),
                    cluster: Some(&status.cluster),
                    mint: Some(mint),
                    ..ChainScopeRequest::default()
                },
            )
            .await
        {
            Ok(scopes) if !scopes.is_empty() => scopes,
            Ok(_) => {
                let reason =
                    format!("limit {limit}, cluster, or mint does not match capability scope");
                self.record_capability_scope_rejected(peer, "chain:flush", "chain.flush", &reason)
                    .await;
                return Response::Error {
                    message: format!("receipt flushing rejected by capability scope: {reason}"),
                };
            }
            Err(reason) => {
                self.record_capability_scope_rejected(peer, "chain:flush", "chain.flush", &reason)
                    .await;
                return Response::Error {
                    message: format!(
                        "receipt flushing rejected by invalid capability scope: {reason}"
                    ),
                };
            }
        };

        let receipts = match self.settlement.recent(limit).await {
            Ok(receipts) => receipts
                .into_iter()
                .filter(|receipt| receipt.payer.pubkey == peer.pubkey)
                .filter(|receipt| chain_receipt_allowed(&scopes, receipt))
                .collect::<Vec<_>>(),
            Err(e) => {
                return Response::Error {
                    message: format!("settlement: {e}"),
                };
            }
        };

        let batch = match build_receipt_batch(&receipts) {
            Ok(batch) => batch,
            Err(e) => {
                return Response::Error {
                    message: format!("receipt batch: {e}"),
                };
            }
        };

        let confirmation = ChainConfirmation {
            chain: "solana".to_string(),
            cluster: status.cluster,
            batch_id: batch.batch_id.clone(),
            merkle_root: batch.merkle_root.clone(),
            tx_sig: None,
            slot: None,
            confirmed_at: None,
        };
        let receipts_updated = match self
            .settlement
            .mark_batch_confirmed(&batch.receipt_ids, confirmation)
            .await
        {
            Ok(updated) => updated,
            Err(e) => {
                return Response::Error {
                    message: format!("mark receipt batch: {e}"),
                };
            }
        };

        Response::ReceiptBatchFlushed {
            batch: ReceiptBatchSummary {
                batch_id: batch.batch_id,
                merkle_root: batch.merkle_root,
                receipt_count: batch.receipt_count,
                tx_sig: None,
                slot: None,
            },
            receipts_updated,
        }
    }

    /// Dispatch an outbound x402 paid call on the peer's behalf.
    ///
    /// Gated by the `x402.outbound.pay` capability. Builds a
    /// [`x402::SubprocessSigner`] from the daemon's
    /// [`x402::X402Config`] (the funding key never enters the daemon
    /// process), runs the 402-then-pay loop, and records the linked
    /// budget debit + settlement receipt + audit event on success.
    /// The receipt id surfaces back to the caller for join-keys.
    ///
    /// v1: the receipt amount is recorded as the operator-authorized
    /// `per_call_cap`, not the live signed amount. A follow-up that
    /// surfaces the chosen [`covenant_x402::PaymentRequirements`]
    /// from `request_paid` will tighten that to the exact amount.
    #[allow(clippy::too_many_arguments)]
    async fn pay_x402(
        &self,
        provider: String,
        endpoint: String,
        method: String,
        body: Option<serde_json::Value>,
        network: String,
        asset: String,
        per_call_cap: String,
        credits: u64,
        peer: &AgentId,
    ) -> Response {
        let check = self
            .check_capabilities("x402:pay".into(), vec!["x402.outbound.pay".into()], peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: "x402 dispatch requires capability \"x402.outbound.pay\". \
                          Grant it with `covenant capabilities grant x402.outbound.pay`."
                    .into(),
            };
        }

        let Some(config) = self.x402_dispatch.clone() else {
            return Response::Error {
                message: "x402 dispatch is not configured on this daemon. \
                          Wire the funding-key sidecar via Server::with_x402_dispatch \
                          and restart."
                    .into(),
            };
        };
        if !config.enabled {
            return Response::Error {
                message: "x402 dispatch is disabled in this daemon's config.".into(),
            };
        }

        let http_method = match method.parse::<reqwest::Method>() {
            Ok(m) => m,
            Err(_) => {
                return Response::Error {
                    message: format!("invalid HTTP method: {method:?}"),
                }
            }
        };
        let per_call_cap_u: u128 = match per_call_cap.parse() {
            Ok(n) => n,
            Err(_) => {
                return Response::Error {
                    message: format!(
                        "invalid per_call_cap (must be decimal u128): {per_call_cap:?}"
                    ),
                }
            }
        };

        let mut signer = x402::SubprocessSigner::new(&config.signer_binary);
        for (k, v) in &config.signer_env {
            signer = signer.env(k.clone(), v.clone());
        }

        let capability = covenant_x402::Capability {
            provider: provider.clone(),
            network: network.clone(),
            asset: asset.clone(),
            per_call_cap: per_call_cap_u,
        };
        let call = x402::PaidCall {
            provider: &provider,
            endpoint: &endpoint,
            method: http_method,
            capability,
            body: body.as_ref(),
            amount: per_call_cap.clone(),
            network: network.clone(),
            asset: asset.clone(),
            credits,
        };

        let issuer = self.identity.agent_id();
        let context = x402::SettlementContext {
            settlement: self.settlement.as_ref(),
            audit: self.audit.as_ref(),
            budget: self.budget.as_ref(),
            issuer: &issuer,
        };

        let client = covenant_x402::Client::new(reqwest::Client::new());
        let outcome =
            match x402::pay_and_record(&context, &config, &client, &signer, peer, &call).await {
                Ok(outcome) => outcome,
                Err(e) => {
                    return Response::Error {
                        message: format!("x402 dispatch failed: {e}"),
                    }
                }
            };
        let status = outcome.response.status().as_u16();
        let body_text = match outcome.response.text().await {
            Ok(t) => t,
            Err(e) => {
                return Response::Error {
                    message: format!("read upstream body: {e}"),
                }
            }
        };
        let receipt_id = outcome.receipt_id.unwrap_or_else(Uuid::nil);
        Response::X402Paid {
            receipt_id,
            status,
            body: body_text,
        }
    }

    async fn receipt_batches(&self, limit: usize, peer: &AgentId) -> Response {
        let check = self
            .check_capabilities("chain:batches".into(), vec!["chain.batches".into()], peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: "receipt batch reads require capability \"chain.batches\". \
                     Grant it with `covenant capabilities grant chain.batches`."
                    .into(),
            };
        }
        // See recent_receipts: mint is environment-level and must be enforced at
        // the gather stage; per-item fields are filtered by chain_receipt_allowed.
        let status = chain_status_from_env();
        let mint = status.covnt_mint.as_deref().unwrap_or("");
        let scopes = match self
            .chain_scopes(
                "chain.batches",
                peer,
                ChainScopeRequest {
                    limit: Some(limit),
                    mint: Some(mint),
                    ..ChainScopeRequest::default()
                },
            )
            .await
        {
            Ok(scopes) if !scopes.is_empty() => scopes,
            Ok(_) => {
                let reason = format!("limit {limit} or mint does not match capability scope");
                self.record_capability_scope_rejected(
                    peer,
                    "chain:batches",
                    "chain.batches",
                    &reason,
                )
                .await;
                return Response::Error {
                    message: format!("receipt batch reads rejected by capability scope: {reason}"),
                };
            }
            Err(reason) => {
                self.record_capability_scope_rejected(
                    peer,
                    "chain:batches",
                    "chain.batches",
                    &reason,
                )
                .await;
                return Response::Error {
                    message: format!(
                        "receipt batch reads rejected by invalid capability scope: {reason}"
                    ),
                };
            }
        };
        let receipts = match self.settlement.recent(limit).await {
            Ok(receipts) => receipts,
            Err(e) => {
                return Response::Error {
                    message: format!("settlement: {e}"),
                };
            }
        };

        let mut batches: BTreeMap<String, ReceiptBatchSummary> = BTreeMap::new();
        for receipt in receipts
            .into_iter()
            .filter(|receipt| receipt.payer.pubkey == peer.pubkey)
            .filter(|receipt| chain_receipt_allowed(&scopes, receipt))
        {
            let Some(batch_id) = receipt.batch_id.clone() else {
                continue;
            };
            let entry = batches
                .entry(batch_id.clone())
                .or_insert_with(|| ReceiptBatchSummary {
                    batch_id,
                    merkle_root: receipt.merkle_root.clone().unwrap_or_default(),
                    receipt_count: 0,
                    tx_sig: receipt.tx_sig.clone(),
                    slot: receipt.slot,
                });
            entry.receipt_count = entry.receipt_count.saturating_add(1);
            if entry.tx_sig.is_none() {
                entry.tx_sig = receipt.tx_sig.clone();
            }
            if entry.slot.is_none() {
                entry.slot = receipt.slot;
            }
        }

        Response::ReceiptBatches {
            batches: batches.into_values().rev().take(limit).collect(),
        }
    }

    async fn search_memory(
        &self,
        query: String,
        tier: Option<MemoryTier>,
        limit: usize,
        min_relevance: Option<f32>,
        peer: &AgentId,
    ) -> Response {
        let actions = memory_read_actions(tier);
        let check = self
            .check_capabilities_any_of("memory:search".into(), vec![actions], peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: "memory search requires capability \"memory.read\" or a tier-specific memory.read.<tier> capability. \
                     Grant it with `covenant capabilities grant memory.read`."
                    .into(),
            };
        }

        let scopes = match self.memory_read_scopes(tier, peer).await {
            Ok(scopes) if !scopes.is_empty() => scopes,
            Ok(_) => {
                let reason =
                    "tier, record, mode, or age does not match capability scope".to_string();
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::CapabilityScopeRejected {
                        agent_id: "memory:search".into(),
                        action: "memory.read".into(),
                        reason: reason.clone(),
                    },
                };
                self.record_peer_event(peer, event).await;
                return Response::Error {
                    message: format!("memory search rejected by capability scope: {reason}"),
                };
            }
            Err(reason) => {
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::CapabilityScopeRejected {
                        agent_id: "memory:search".into(),
                        action: "memory.read".into(),
                        reason: reason.clone(),
                    },
                };
                self.record_peer_event(peer, event).await;
                return Response::Error {
                    message: format!(
                        "memory search rejected by invalid capability scope: {reason}"
                    ),
                };
            }
        };
        let q_emb = match self.embedder.embed(&query).await {
            Ok(v) => v,
            Err(e) => {
                return Response::Error {
                    message: format!("embed: {e}"),
                };
            }
        };
        match self
            .memory
            .search_similar(q_emb, tier, limit, min_relevance)
            .await
        {
            Ok(records) => Response::Memories {
                records: records
                    .into_iter()
                    .filter(|record| record.owner.pubkey == peer.pubkey)
                    .filter(|record| memory_read_record_allowed(&scopes, record))
                    .collect(),
            },
            Err(e) => Response::Error {
                message: format!("memory: {e}"),
            },
        }
    }

    /// Cross-check the last `window` records of memory / audit / receipts /
    /// capabilities for drift. Returns a report with per-check pass/fail and
    /// a total orphan count. Useful for paranoid operator inspection.
    async fn verify_recent(&self, window: usize) -> Response {
        use covenant_audit::AuditKind;
        use covenant_ipc::{VerifyCheck, VerifyDrift};
        use std::collections::{HashMap, HashSet};

        let mut checks: Vec<VerifyCheck> = Vec::new();
        let mut drift: Vec<VerifyDrift> = Vec::new();
        let mut orphans_total: u64 = 0;

        let memories = match self.memory.recent(None, window).await {
            Ok(records) => records,
            Err(e) => {
                return Response::Error {
                    message: format!("memory: {e}"),
                };
            }
        };
        let audits = match self.audit.recent(window).await {
            Ok(events) => events,
            Err(e) => {
                return Response::Error {
                    message: format!("audit: {e}"),
                };
            }
        };
        let receipts = match self.settlement.recent(window).await {
            Ok(receipts) => receipts,
            Err(e) => {
                return Response::Error {
                    message: format!("settlement: {e}"),
                };
            }
        };
        let caps = match self.capabilities.recent(window).await {
            Ok(caps) => caps,
            Err(e) => {
                return Response::Error {
                    message: format!("capabilities: {e}"),
                };
            }
        };

        // Check 1: every memory record's id appears as an IntentDispatched
        // audit event's intent_id. Track per-intent_id counts so a replay or
        // out-of-band append that records the same intent_id twice surfaces
        // as intent_dispatched_duplicate; collapsing it into a HashSet would
        // hide the second row entirely.
        let memory_ids: HashSet<Uuid> = memories.iter().map(|m| m.id).collect();
        let mut dispatched_intent_counts: HashMap<Uuid, usize> = HashMap::new();
        for event in &audits {
            if let AuditKind::IntentDispatched { intent_id, .. } = &event.kind {
                *dispatched_intent_counts.entry(*intent_id).or_insert(0) += 1;
            }
        }
        let memory_orphans: u64 = memory_ids
            .iter()
            .filter(|id| !dispatched_intent_counts.contains_key(id))
            .count() as u64;
        for id in memory_ids
            .iter()
            .filter(|id| !dispatched_intent_counts.contains_key(id))
        {
            drift.push(VerifyDrift {
                kind: "memory_without_audit".into(),
                id: Some(id.to_string()),
                message: "memory record has no matching IntentDispatched audit row".into(),
                repair: "inspect the record; preserve it if still useful, otherwise delete only through an explicit repair command".into(),
            });
        }
        let audit_orphans: u64 = dispatched_intent_counts
            .keys()
            .filter(|id| !memory_ids.contains(id))
            .count() as u64;
        for id in dispatched_intent_counts
            .keys()
            .filter(|id| !memory_ids.contains(id))
        {
            drift.push(VerifyDrift {
                kind: "audit_without_memory".into(),
                id: Some(id.to_string()),
                message: "IntentDispatched audit row has no matching memory record".into(),
                repair: "inspect audit and receipt rows before deciding whether to backfill memory or mark the dispatch intentionally memoryless".into(),
            });
        }
        let mut duplicate_intent_refs = 0_u64;
        for (intent_id, count) in &dispatched_intent_counts {
            if *count > 1 {
                duplicate_intent_refs += 1;
                drift.push(VerifyDrift {
                    kind: "intent_dispatched_duplicate".into(),
                    id: Some(intent_id.to_string()),
                    message: format!(
                        "{count} IntentDispatched audit rows share intent_id {intent_id}"
                    ),
                    repair: "review the audit log for replay or duplicate dispatch; identify the canonical row before truncating".into(),
                });
            }
        }
        orphans_total += memory_orphans + audit_orphans + duplicate_intent_refs;
        checks.push(VerifyCheck {
            name: "memory ↔ audit".into(),
            passed: memory_orphans == 0 && audit_orphans == 0 && duplicate_intent_refs == 0,
            message: format!(
                "{memory_orphans} memory orphan(s), {audit_orphans} audit orphan(s), {duplicate_intent_refs} duplicate intent(s)"
            ),
        });

        // Check 2: parent references should resolve against the memory store,
        // even when the parent sits outside the sampled recent window. A
        // self-parent (parent == id) resolves via get() and would otherwise
        // hide the cycle behind memory_stale_parent's None branch, so guard
        // it explicitly before the lookup. When the direct parent resolves,
        // keep walking the chain so multi-hop cycles (A->B->A or longer)
        // surface as memory_parent_cycle instead of silently passing.
        const MAX_PARENT_HOPS: usize = 32;
        let mut stale_parent_refs = 0_u64;
        let mut self_parent_refs = 0_u64;
        let mut cycle_parent_refs = 0_u64;
        for record in &memories {
            let Some(parent) = record.parent else {
                continue;
            };
            if parent == record.id {
                self_parent_refs += 1;
                drift.push(VerifyDrift {
                    kind: "memory_self_parent".into(),
                    id: Some(record.id.to_string()),
                    message: "memory record's parent references itself".into(),
                    repair: "detach the self-referential parent through an explicit detach_parent repair command".into(),
                });
                continue;
            }
            match self.memory.get(parent).await {
                Ok(Some(direct)) => {
                    let mut visited: HashSet<Uuid> = HashSet::new();
                    visited.insert(record.id);
                    visited.insert(parent);
                    let mut cursor = direct.parent;
                    let mut hops = 1_usize;
                    while let Some(next) = cursor {
                        if !visited.insert(next) {
                            cycle_parent_refs += 1;
                            drift.push(VerifyDrift {
                                kind: "memory_parent_cycle".into(),
                                id: Some(record.id.to_string()),
                                message: format!(
                                    "memory record's parent chain forms a cycle through {next}"
                                ),
                                repair: "detach a node in the cycle through an explicit detach_parent repair command".into(),
                            });
                            break;
                        }
                        hops += 1;
                        if hops > MAX_PARENT_HOPS {
                            break;
                        }
                        match self.memory.get(next).await {
                            Ok(Some(rec)) => cursor = rec.parent,
                            Ok(None) => break,
                            Err(e) => {
                                return Response::Error {
                                    message: format!("memory: {e}"),
                                };
                            }
                        }
                    }
                }
                Ok(None) => {
                    stale_parent_refs += 1;
                    drift.push(VerifyDrift {
                        kind: "memory_stale_parent".into(),
                        id: Some(record.id.to_string()),
                        message: format!("memory parent reference {parent} does not resolve"),
                        repair: "inspect the child record and either restore its parent or detach the parent reference through an explicit repair command".into(),
                    });
                }
                Err(e) => {
                    return Response::Error {
                        message: format!("memory: {e}"),
                    };
                }
            }
        }
        orphans_total += stale_parent_refs + self_parent_refs + cycle_parent_refs;
        checks.push(VerifyCheck {
            name: "memory parent references".into(),
            passed: stale_parent_refs == 0
                && self_parent_refs == 0
                && cycle_parent_refs == 0,
            message: format!(
                "{stale_parent_refs} stale parent reference(s), {self_parent_refs} self-parent reference(s), {cycle_parent_refs} parent cycle(s)"
            ),
        });

        // Check 3: every capability in the granted set has a matching
        // CapabilityGranted audit event. Mismatch means an out-of-band write
        // to granted.jsonl that didn't go through the daemon.
        let audited_grant_sigs: HashSet<String> = audits
            .iter()
            .filter_map(|e| match &e.kind {
                AuditKind::CapabilityGranted { signature_b58, .. } => Some(signature_b58.clone()),
                _ => None,
            })
            .collect();
        let cap_orphans: u64 = caps
            .iter()
            .filter(|c| {
                let s = bs58::encode(c.signature).into_string();
                !audited_grant_sigs.contains(&s)
            })
            .count() as u64;
        for cap in caps.iter().filter(|c| {
            let sig = bs58::encode(c.signature).into_string();
            !audited_grant_sigs.contains(&sig)
        }) {
            let signature_b58 = bs58::encode(cap.signature).into_string();
            drift.push(VerifyDrift {
                kind: "capability_without_audit".into(),
                id: Some(signature_b58),
                message: format!(
                    "capability grant for action {} has no matching audit row",
                    cap.capability.action
                ),
                repair: "treat as out-of-band mutation; revoke if untrusted or backfill provenance before retaining".into(),
            });
        }
        orphans_total += cap_orphans;
        checks.push(VerifyCheck {
            name: "capability ↔ audit".into(),
            passed: cap_orphans == 0,
            message: format!(
                "{} capabilit(ies) without matching grant audit event",
                cap_orphans
            ),
        });

        // Check 4: memory writes and settlement receipts should be 1:1.
        // New receipts carry `memory_record_id` for exact joins; legacy rows
        // without it fall back to owner/resource counts so old stores keep
        // passing when their aggregate accounting is still balanced.
        let memory_by_id: HashMap<Uuid, &MemoryRecord> =
            memories.iter().map(|memory| (memory.id, memory)).collect();
        // memory_record_id is only set by memory writes, so a receipt that
        // carries one while reporting a non-Memory resource is out-of-band
        // mutation evidence. Pre-scan before the memory-resource filter so
        // these surface as drift instead of being silently dropped.
        let mut resource_mismatch_refs = 0_u64;
        for receipt in &receipts {
            if receipt.memory_record_id.is_some() && receipt.resource != ResourceKind::Memory {
                resource_mismatch_refs += 1;
                drift.push(VerifyDrift {
                    kind: "memory_receipt_resource_mismatch".into(),
                    id: Some(receipt.id.to_string()),
                    message: format!(
                        "receipt {} carries memory_record_id but resource is {:?}",
                        receipt.id, receipt.resource
                    ),
                    repair: "review settlement provenance before retaining; memory_record_id is only set by memory writes".into(),
                });
            }
        }
        let memory_receipts: Vec<&SettlementReceipt> = receipts
            .iter()
            .filter(|receipt| receipt.resource == ResourceKind::Memory)
            .collect();
        let mut receipts_by_memory_id: HashMap<Uuid, Vec<&SettlementReceipt>> = HashMap::new();
        let mut legacy_receipts_by_owner: HashMap<String, usize> = HashMap::new();
        for receipt in &memory_receipts {
            if let Some(memory_record_id) = receipt.memory_record_id {
                receipts_by_memory_id
                    .entry(memory_record_id)
                    .or_default()
                    .push(*receipt);
            } else {
                *legacy_receipts_by_owner
                    .entry(receipt.payer.pubkey_base58())
                    .or_insert(0) += 1;
            }
        }

        let mut exact_diff = 0_u64;
        for (memory_id, matched_receipts) in &receipts_by_memory_id {
            match memory_by_id.get(memory_id) {
                Some(memory) => {
                    if matched_receipts.len() > 1 {
                        exact_diff += matched_receipts.len().saturating_sub(1) as u64;
                        drift.push(VerifyDrift {
                            kind: "memory_receipt_duplicate".into(),
                            id: Some(memory_id.to_string()),
                            message: format!(
                                "{} receipts reference memory record {memory_id}",
                                matched_receipts.len()
                            ),
                            repair: "review duplicate settlement receipts and retain only the authoritative accounting row".into(),
                        });
                    }
                    for receipt in matched_receipts {
                        if receipt.payer.pubkey != memory.owner.pubkey {
                            exact_diff += 1;
                            drift.push(VerifyDrift {
                                kind: "memory_receipt_owner_mismatch".into(),
                                id: Some(receipt.id.to_string()),
                                message: format!(
                                    "receipt {} references memory record {memory_id} but payer differs from memory owner",
                                    receipt.id
                                ),
                                repair: "treat as out-of-band settlement mutation; backfill only after confirming the intended payer".into(),
                            });
                        }
                        if receipt.settled_at < memory.created_at {
                            exact_diff += 1;
                            drift.push(VerifyDrift {
                                kind: "memory_receipt_settled_before_created".into(),
                                id: Some(receipt.id.to_string()),
                                message: format!(
                                    "receipt {} settled_at={} precedes memory record {memory_id}.created_at={}",
                                    receipt.id, receipt.settled_at, memory.created_at
                                ),
                                repair: "review receipt correlation: settled_at < memory.created_at indicates a backfill correlation mistake or a clock-tamper restore".into(),
                            });
                        }
                    }
                }
                None => {
                    exact_diff += matched_receipts.len() as u64;
                    for receipt in matched_receipts {
                        drift.push(VerifyDrift {
                            kind: "receipt_without_memory_record".into(),
                            id: Some(receipt.id.to_string()),
                            message: format!(
                                "receipt {} references missing memory record {memory_id}",
                                receipt.id
                            ),
                            repair: "review the receipt before settlement; missing memory may require accounting reversal or provenance backfill".into(),
                        });
                    }
                }
            }
        }

        let mut legacy_fallback_used = 0_usize;
        for memory in &memories {
            if receipts_by_memory_id.contains_key(&memory.id) {
                continue;
            }
            let owner = memory.owner.pubkey_base58();
            let available_legacy = legacy_receipts_by_owner.entry(owner).or_insert(0);
            if *available_legacy > 0 {
                *available_legacy -= 1;
                legacy_fallback_used += 1;
                continue;
            }

            exact_diff += 1;
            drift.push(VerifyDrift {
                kind: "memory_without_receipt".into(),
                id: Some(memory.id.to_string()),
                message: format!("memory record {} has no settlement receipt", memory.id),
                repair: "reconcile settlement before mutating memory; missing receipts may require backfill".into(),
            });
        }

        let mut memory_by_owner: HashMap<String, usize> = HashMap::new();
        for record in &memories {
            *memory_by_owner
                .entry(record.owner.pubkey_base58())
                .or_insert(0) += 1;
        }
        let mut receipt_by_owner: HashMap<String, usize> = HashMap::new();
        for receipt in &memory_receipts {
            *receipt_by_owner
                .entry(receipt.payer.pubkey_base58())
                .or_insert(0) += 1;
        }
        let owners: HashSet<String> = memory_by_owner
            .keys()
            .chain(receipt_by_owner.keys())
            .cloned()
            .collect();
        let mut pair_diff = 0_u64;
        for owner in owners {
            let memory_count = memory_by_owner.get(&owner).copied().unwrap_or(0);
            let receipt_count = receipt_by_owner.get(&owner).copied().unwrap_or(0);
            if memory_count == receipt_count {
                continue;
            }
            pair_diff += memory_count.abs_diff(receipt_count) as u64;
            drift.push(VerifyDrift {
                kind: "memory_receipt_mismatch".into(),
                id: Some(owner),
                message: format!(
                    "{memory_count} memory record(s) vs {receipt_count} memory receipt(s) for owner"
                ),
                repair: "reconcile settlement before mutating memory; missing receipts may require backfill, extra receipts may require accounting review".into(),
            });
        }
        let receipt_drift = exact_diff.max(pair_diff);
        orphans_total += receipt_drift + resource_mismatch_refs;
        checks.push(VerifyCheck {
            name: "memory ↔ receipts".into(),
            passed: exact_diff == 0 && pair_diff == 0 && resource_mismatch_refs == 0,
            message: format!(
                "{} memory record(s) vs {} receipt(s); count diff = {}; exact drift = {}; legacy fallback = {}; resource mismatch = {}",
                memories.len(),
                memory_receipts.len(),
                pair_diff,
                exact_diff,
                legacy_fallback_used,
                resource_mismatch_refs
            ),
        });

        // Check 5: memory record integrity. Two signals fold into this row:
        //   - empty text. The SQLite schema only enforces TEXT NOT NULL, so
        //     empty strings round-trip through put() today. An empty body
        //     produces a noise embedding and cannot anchor retrieval.
        //   - NaN inside the embedding vector. cosine() short-circuits on
        //     na/nb == 0.0 but NaN never satisfies that comparison, so a
        //     single NaN poisons every similarity that record competes in.
        //     SQLite stores the embedding as a raw f32 BLOB and
        //     embedding_to_bytes/from_bytes preserve NaN bit patterns intact.
        // The existing delete_record repair handles both cases.
        let mut empty_text_refs = 0_u64;
        let mut nan_embedding_refs = 0_u64;
        for record in &memories {
            if record.text.is_empty() {
                empty_text_refs += 1;
                drift.push(VerifyDrift {
                    kind: "memory_empty_text".into(),
                    id: Some(record.id.to_string()),
                    message: format!("memory record {} has empty text", record.id),
                    repair: "review the record source; safe removals go through an explicit delete_record repair command".into(),
                });
            }
            if record.embedding.iter().any(|v| v.is_nan()) {
                nan_embedding_refs += 1;
                drift.push(VerifyDrift {
                    kind: "memory_nan_embedding".into(),
                    id: Some(record.id.to_string()),
                    message: format!(
                        "memory record {} embedding contains NaN values",
                        record.id
                    ),
                    repair: "the embedding is unusable for cosine ranking; safe removals go through an explicit delete_record repair command".into(),
                });
            }
        }
        orphans_total += empty_text_refs + nan_embedding_refs;
        checks.push(VerifyCheck {
            name: "memory record integrity".into(),
            passed: empty_text_refs == 0 && nan_embedding_refs == 0,
            message: format!(
                "{empty_text_refs} empty-text record(s), {nan_embedding_refs} NaN-embedding record(s)"
            ),
        });

        // Check 6: settlement receipt integrity. annotate_receipt is the
        // sole production path that fills chain provenance, and it sets the
        // bundle (chain, cluster, batch_id, merkle_root) together from the
        // same ChainConfirmation. It also writes both receipt.tx_sig and
        // receipt.onchain_sig from the same confirmation.tx_sig.clone(), so
        // when both fields are populated they must be byte-identical. Three
        // signals fold into this row:
        //   - confirmed_at = Some(_) with chain = None. annotate_receipt
        //     would have set chain too, so this is out-of-band evidence.
        //   - chain-bundle partial state: a strict subset (1-3 of the four
        //     fields) is Some. A bundle that is fully unset (0) or fully
        //     set (4) is fine; anything in between is a half-torn provenance
        //     anchor.
        //   - tx_sig / onchain_sig divergence: both fields are Some(_) but
        //     disagree. covenant-settlement treats Some+None in either
        //     direction as legacy-compatible "onchain settled" state
        //     (covenant-settlement/src/lib.rs:164), so partial population
        //     is tolerated; only a two-Some disagreement is out-of-band.
        // Remediation is a settlement-team decision and intentionally
        // outside the memory-side repair set.
        let mut confirmed_without_chain_refs = 0_u64;
        let mut chain_partial_refs = 0_u64;
        let mut tx_sig_onchain_sig_diverged_refs = 0_u64;
        for receipt in &receipts {
            if receipt.confirmed_at.is_some() && receipt.chain.is_none() {
                confirmed_without_chain_refs += 1;
                drift.push(VerifyDrift {
                    kind: "receipt_confirmed_without_chain".into(),
                    id: Some(receipt.id.to_string()),
                    message: format!(
                        "receipt {} carries confirmed_at but chain is unset",
                        receipt.id
                    ),
                    repair: "review settlement provenance before retaining; confirmed_at is only set by annotate_receipt alongside chain/cluster/batch_id/merkle_root".into(),
                });
            }
            let bundle_set = [
                receipt.chain.is_some(),
                receipt.cluster.is_some(),
                receipt.batch_id.is_some(),
                receipt.merkle_root.is_some(),
            ];
            let set_count = bundle_set.iter().filter(|present| **present).count();
            if set_count != 0 && set_count != 4 {
                chain_partial_refs += 1;
                drift.push(VerifyDrift {
                    kind: "receipt_chain_partial".into(),
                    id: Some(receipt.id.to_string()),
                    message: format!(
                        "receipt {} chain provenance is partial: chain={} cluster={} batch_id={} merkle_root={}",
                        receipt.id,
                        bundle_set[0],
                        bundle_set[1],
                        bundle_set[2],
                        bundle_set[3]
                    ),
                    repair: "review settlement provenance before retaining; annotate_receipt fills chain/cluster/batch_id/merkle_root as a single bundle".into(),
                });
            }
            if let (Some(tx_sig), Some(onchain_sig)) =
                (receipt.tx_sig.as_deref(), receipt.onchain_sig.as_deref())
            {
                if tx_sig != onchain_sig {
                    tx_sig_onchain_sig_diverged_refs += 1;
                    drift.push(VerifyDrift {
                        kind: "receipt_tx_sig_onchain_sig_diverged".into(),
                        id: Some(receipt.id.to_string()),
                        message: format!(
                            "receipt {} tx_sig and onchain_sig disagree: tx_sig={tx_sig} onchain_sig={onchain_sig}",
                            receipt.id
                        ),
                        repair: "review settlement provenance before retaining; annotate_receipt writes tx_sig and onchain_sig from the same confirmation.tx_sig.clone()".into(),
                    });
                }
            }
        }
        orphans_total +=
            confirmed_without_chain_refs + chain_partial_refs + tx_sig_onchain_sig_diverged_refs;
        checks.push(VerifyCheck {
            name: "settlement receipt integrity".into(),
            passed: confirmed_without_chain_refs == 0
                && chain_partial_refs == 0
                && tx_sig_onchain_sig_diverged_refs == 0,
            message: format!(
                "{confirmed_without_chain_refs} confirmed-without-chain receipt(s), {chain_partial_refs} partial-chain-bundle receipt(s), {tx_sig_onchain_sig_diverged_refs} tx-sig/onchain-sig-diverged receipt(s)"
            ),
        });

        Response::VerifyReport {
            window,
            checks,
            drift,
            orphans_total,
        }
    }

    async fn purge_memory(
        &self,
        tier: Option<MemoryTier>,
        before_ms: u64,
        peer: &AgentId,
    ) -> Response {
        let required = "memory.purge".to_string();
        let check = self
            .check_capabilities("memory:purge".into(), vec![required.clone()], peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: "memory purge requires capability \"memory.purge\". \
                     Grant it with `covenant capabilities grant memory.purge`."
                    .into(),
            };
        }
        match self.memory_purge_scope_allows(tier, before_ms, peer).await {
            Ok(true) => {}
            Ok(false) => {
                let reason = "tier or before_ms does not match capability scope".to_string();
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::CapabilityScopeRejected {
                        agent_id: "memory:purge".into(),
                        action: required,
                        reason: reason.clone(),
                    },
                };
                self.record_peer_event(peer, event).await;
                return Response::Error {
                    message: format!("memory purge rejected by capability scope: {reason}"),
                };
            }
            Err(reason) => {
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::CapabilityScopeRejected {
                        agent_id: "memory:purge".into(),
                        action: required,
                        reason: reason.clone(),
                    },
                };
                self.record_peer_event(peer, event).await;
                return Response::Error {
                    message: format!("memory purge rejected by invalid capability scope: {reason}"),
                };
            }
        }
        match self.memory.purge_older_than(tier, before_ms).await {
            Ok(purged) => Response::MemoryPurged { purged },
            Err(e) => Response::Error {
                message: format!("memory: {e}"),
            },
        }
    }

    async fn memory_scope_allows<F>(
        &self,
        action: &str,
        peer: &AgentId,
        mut allows: F,
    ) -> Result<bool, String>
    where
        F: FnMut(&serde_json::Value) -> Result<bool, covenant_permissions::PermissionError>,
    {
        let now = epoch_ms();
        let trust_root = self.identity.agent_id().pubkey;
        let user_caps = self
            .capabilities
            .list_for_subject(peer.pubkey)
            .await
            .map_err(|e| e.to_string())?;
        let mut invalid_scope = None;
        for cap in user_caps.iter().filter(|cap| {
            cap.capability.action == action
                && verify_with_clock_and_trust_root(cap, now, trust_root).is_ok()
        }) {
            match allows(&cap.capability.scope) {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(e) => {
                    invalid_scope.get_or_insert_with(|| e.to_string());
                }
            }
        }
        if let Some(reason) = invalid_scope {
            return Err(reason);
        }
        Ok(false)
    }

    async fn memory_purge_scope_allows(
        &self,
        tier: Option<MemoryTier>,
        before_ms: u64,
        peer: &AgentId,
    ) -> Result<bool, String> {
        let tier_name = tier.map(memory_tier_name);
        self.memory_scope_allows("memory.purge", peer, |scope| {
            permission_memory_purge_scope_allows("memory.purge", scope, tier_name, before_ms)
        })
        .await
    }

    async fn memory_read_scopes(
        &self,
        tier: Option<MemoryTier>,
        peer: &AgentId,
    ) -> Result<Vec<(String, serde_json::Value)>, String> {
        let actions = memory_read_actions(tier);
        let now = epoch_ms();
        let trust_root = self.identity.agent_id().pubkey;
        let user_caps = self
            .capabilities
            .list_for_subject(peer.pubkey)
            .await
            .map_err(|e| e.to_string())?;
        let mut scopes = Vec::new();
        let mut invalid_scope = None;
        let tier_name = tier.map(memory_tier_name);

        for cap in user_caps.iter().filter(|cap| {
            actions
                .iter()
                .any(|action| action == &cap.capability.action)
                && verify_with_clock_and_trust_root(cap, now, trust_root).is_ok()
        }) {
            match permission_memory_read_scope_allows(
                &cap.capability.action,
                &cap.capability.scope,
                tier_name,
            ) {
                Ok(true) => {
                    scopes.push((cap.capability.action.clone(), cap.capability.scope.clone()))
                }
                Ok(false) => {}
                Err(e) => {
                    invalid_scope.get_or_insert_with(|| e.to_string());
                }
            }
        }

        if scopes.is_empty() {
            if let Some(reason) = invalid_scope {
                return Err(reason);
            }
        }
        Ok(scopes)
    }

    async fn memory_write_scope_allows(
        &self,
        record_id: &str,
        tier: MemoryTier,
        created_at_ms: u64,
        peer: &AgentId,
    ) -> Result<bool, String> {
        let tier_name = memory_tier_name(tier);
        self.memory_scope_allows("memory.write", peer, |scope| {
            permission_memory_write_scope_allows(
                "memory.write",
                scope,
                record_id,
                tier_name,
                created_at_ms,
            )
        })
        .await
    }

    async fn repair_memory(
        &self,
        request: covenant_types::MemoryRepairRequest,
        peer: &AgentId,
    ) -> Response {
        let mode = memory_repair_mode(request.mode);
        let action = memory_repair_action(&request.command);
        let required = format!("memory.repair.{mode}");
        let check = self
            .check_capabilities(
                format!(
                    "memory-repair:{action}:{}",
                    memory_repair_id(&request.command)
                ),
                vec![required.clone()],
                peer,
            )
            .await;
        if !check.passed {
            return Response::Error {
                message: format!(
                    "memory repair {mode} requires capability {required:?}. Grant it with `covenant capabilities grant {required}`."
                ),
            };
        }

        let id = memory_repair_id(&request.command);
        let record = match self.memory.get(id).await {
            Ok(Some(record)) if record.owner.pubkey == peer.pubkey => record,
            Ok(Some(_)) => {
                return Response::Error {
                    message: format!("memory repair rejected: record {id} is not visible to the authenticated peer"),
                };
            }
            Ok(None) => {
                return Response::Error {
                    message: format!("memory: memory record {id} not found"),
                };
            }
            Err(e) => {
                return Response::Error {
                    message: format!("memory: {e}"),
                };
            }
        };

        match self
            .memory_repair_scope_allows(
                &required,
                &id.to_string(),
                record.tier,
                record.created_at,
                request.mode == MemoryRepairMode::Apply,
                peer,
            )
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                let reason =
                    "record, tier, mode, or age does not match capability scope".to_string();
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::CapabilityScopeRejected {
                        agent_id: format!("memory-repair:{action}:{}", id),
                        action: required,
                        reason: reason.clone(),
                    },
                };
                self.record_peer_event(peer, event).await;
                return Response::Error {
                    message: format!("memory repair rejected by capability scope: {reason}"),
                };
            }
            Err(reason) => {
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::CapabilityScopeRejected {
                        agent_id: format!("memory-repair:{action}:{}", id),
                        action: required,
                        reason: reason.clone(),
                    },
                };
                self.record_peer_event(peer, event).await;
                return Response::Error {
                    message: format!(
                        "memory repair rejected by invalid capability scope: {reason}"
                    ),
                };
            }
        }

        let reason = request.reason.clone();
        match self.memory.repair(request).await {
            Ok(outcome) => {
                self.record_peer_event(
                    peer,
                    AuditEvent {
                        id: Uuid::new_v4(),
                        timestamp_ms: epoch_ms(),
                        issuer: peer.clone(),
                        kind: AuditKind::MemoryRepairApplied {
                            memory_id: outcome.id,
                            action: action.into(),
                            mode: mode.into(),
                            changed: outcome.changed,
                            reason,
                        },
                    },
                )
                .await;
                Response::MemoryRepaired { outcome }
            }
            Err(e) => Response::Error {
                message: format!("memory: {e}"),
            },
        }
    }

    async fn compact_memory(
        &self,
        mut request: MemoryCompactionRequest,
        peer: &AgentId,
    ) -> Response {
        let mode = memory_repair_mode(request.mode);
        let required = format!("memory.compact.{mode}");
        let check = self
            .check_capabilities("memory-compact".into(), vec![required.clone()], peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: format!(
                    "memory compaction {mode} requires capability {required:?}. Grant it with `covenant capabilities grant {required}`."
                ),
            };
        }

        if peer.pubkey != self.identity.agent_id().pubkey {
            return Response::Error {
                message: "memory compaction requires the operator identity".into(),
            };
        }

        match self
            .memory_compaction_scope_allows(
                &required,
                MemoryCompactionScopeRequest {
                    apply: request.mode == MemoryRepairMode::Apply,
                    delete_working_before_ms: request.policy.delete_working_before_ms,
                    delete_episodic_before_ms: request.policy.delete_episodic_before_ms,
                    mark_longterm_stale_before_ms: request.policy.mark_longterm_stale_before_ms,
                    detach_stale_parents: request.policy.detach_stale_parents,
                },
                peer,
            )
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                let reason =
                    "policy tiers, mode, or cutoffs do not match capability scope".to_string();
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::CapabilityScopeRejected {
                        agent_id: "memory-compact".into(),
                        action: required,
                        reason: reason.clone(),
                    },
                };
                self.record_peer_event(peer, event).await;
                return Response::Error {
                    message: format!("memory compaction rejected by capability scope: {reason}"),
                };
            }
            Err(reason) => {
                let event = AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: peer.clone(),
                    kind: AuditKind::CapabilityScopeRejected {
                        agent_id: "memory-compact".into(),
                        action: required,
                        reason: reason.clone(),
                    },
                };
                self.record_peer_event(peer, event).await;
                return Response::Error {
                    message: format!(
                        "memory compaction rejected by invalid capability scope: {reason}"
                    ),
                };
            }
        }

        if request.policy.mark_longterm_stale_before_ms.is_some()
            && request.policy.marked_at_ms.is_none()
        {
            request.policy.marked_at_ms = Some(epoch_ms());
        }

        let reason = request.reason.clone();
        match self.memory.compact(request).await {
            Ok(outcome) => {
                self.record_peer_event(
                    peer,
                    AuditEvent {
                        id: Uuid::new_v4(),
                        timestamp_ms: epoch_ms(),
                        issuer: peer.clone(),
                        kind: AuditKind::MemoryCompactionApplied {
                            mode: mode.into(),
                            changed: outcome.changed,
                            reason,
                            deleted: outcome.deleted.clone(),
                            stale_marked: outcome.stale_marked.clone(),
                            parents_detached: outcome.parents_detached.clone(),
                        },
                    },
                )
                .await;
                Response::MemoryCompacted { outcome }
            }
            Err(e) => Response::Error {
                message: format!("memory: {e}"),
            },
        }
    }

    async fn backfill_settlement_receipts(
        &self,
        dry_run: bool,
        scope_pubkey: Option<String>,
        peer: &AgentId,
    ) -> Response {
        if scope_pubkey.is_some() {
            return Response::Error {
                message: "settlement backfill --scope-pubkey is not yet supported; the operation evaluates the authenticated operator's own capability grants".into(),
            };
        }

        let apply = !dry_run;
        let mode = if apply { "apply" } else { "dry_run" };
        let required = format!("settlement.backfill.{mode}");
        let check = self
            .check_capabilities("settlement-backfill".into(), vec![required.clone()], peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: format!(
                    "settlement backfill {mode} requires capability {required:?}. Grant it with `covenant capabilities grant {required}`."
                ),
            };
        }

        if peer.pubkey != self.identity.agent_id().pubkey {
            return Response::Error {
                message: "settlement backfill requires the operator identity".into(),
            };
        }

        // `backfill_receipts` repairs every legacy row with no recency
        // filter, so probe the scope with `before_ms = u64::MAX`: only an
        // unbounded grant (or one omitting `before_ms`) authorizes a full
        // repair, while a recency-bounded grant correctly denies it.
        match self
            .settlement_backfill_scope_allows(&required, apply, u64::MAX, peer)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                let reason = "mode or before_ms bound does not match capability scope".to_string();
                self.record_capability_scope_rejected(
                    peer,
                    "settlement-backfill",
                    &required,
                    &reason,
                )
                .await;
                return Response::Error {
                    message: format!("settlement backfill rejected by capability scope: {reason}"),
                };
            }
            Err(reason) => {
                self.record_capability_scope_rejected(
                    peer,
                    "settlement-backfill",
                    &required,
                    &reason,
                )
                .await;
                return Response::Error {
                    message: format!(
                        "settlement backfill rejected by invalid capability scope: {reason}"
                    ),
                };
            }
        }

        let Some(home) = self.home.clone() else {
            return Response::Error {
                message: "settlement backfill unavailable: server has no home directory configured"
                    .into(),
            };
        };
        let receipts_path = home.join("receipts").join("working.jsonl");

        match covenant_settlement::backfill_receipts(&receipts_path, dry_run).await {
            Ok(outcome) => {
                let rollback_path = outcome.rollback_path.map(|path| path.display().to_string());
                // Emitted only here, after backfill_receipts returned Ok —
                // i.e. after the rollback checkpoint, the rewritten store
                // contents, and the renamed store file are fsynced — so
                // the audit log never claims a mutation whose data did
                // not durably land.
                // Issuer is the acting operator (peer), matching the
                // MemoryRepairApplied audience: the row surfaces on the
                // operator's own /audit feed under the issuer==peer filter.
                self.record_peer_event(
                    peer,
                    AuditEvent {
                        id: Uuid::new_v4(),
                        timestamp_ms: epoch_ms(),
                        issuer: peer.clone(),
                        kind: AuditKind::SettlementReceiptBackfillApplied {
                            row_count: outcome.row_count,
                            rollback_path: rollback_path.clone(),
                            dry_run: outcome.dry_run,
                        },
                    },
                )
                .await;
                Response::SettlementReceiptsBackfilled {
                    row_count: outcome.row_count,
                    rollback_path,
                    dry_run: outcome.dry_run,
                }
            }
            Err(e) => Response::Error {
                message: format!("settlement: {e}"),
            },
        }
    }

    async fn settlement_backfill_scope_allows(
        &self,
        action: &str,
        apply: bool,
        before_ms: u64,
        peer: &AgentId,
    ) -> Result<bool, String> {
        self.memory_scope_allows(action, peer, |scope| {
            permission_settlement_backfill_scope_allows(action, scope, apply, before_ms)
        })
        .await
    }

    async fn backfill_memory_records(
        &self,
        dry_run: bool,
        scope_pubkey: Option<String>,
        peer: &AgentId,
    ) -> Response {
        if scope_pubkey.is_some() {
            return Response::Error {
                message: "memory backfill --scope-pubkey is not yet supported; the operation evaluates the authenticated operator's own capability grants".into(),
            };
        }

        let apply = !dry_run;
        let mode = if apply { "apply" } else { "dry_run" };
        let required = format!("memory.backfill.{mode}");
        let check = self
            .check_capabilities("memory-backfill".into(), vec![required.clone()], peer)
            .await;
        if !check.passed {
            return Response::Error {
                message: format!(
                    "memory backfill {mode} requires capability {required:?}. Grant it with `covenant capabilities grant {required}`."
                ),
            };
        }

        if peer.pubkey != self.identity.agent_id().pubkey {
            return Response::Error {
                message: "memory backfill requires the operator identity".into(),
            };
        }

        // memory_receipt_backfill_correlations runs against every row the
        // store returns with no recency filter, so probe the scope with
        // `before_ms = u64::MAX`: only an unbounded grant (or one
        // omitting `before_ms`) authorizes the full repair window.
        match self
            .memory_backfill_scope_allows(&required, apply, u64::MAX, peer)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                let reason = "mode or before_ms bound does not match capability scope".to_string();
                self.record_capability_scope_rejected(peer, "memory-backfill", &required, &reason)
                    .await;
                return Response::Error {
                    message: format!("memory backfill rejected by capability scope: {reason}"),
                };
            }
            Err(reason) => {
                self.record_capability_scope_rejected(peer, "memory-backfill", &required, &reason)
                    .await;
                return Response::Error {
                    message: format!(
                        "memory backfill rejected by invalid capability scope: {reason}"
                    ),
                };
            }
        }

        // Server-authoritative: fetch the operator's memory records and
        // receipts directly from the stores (filtering to the operator's
        // own pubkey so the same scoping that recent_memory/recent_receipts
        // enforce applies) and recompute correlations with the shared
        // covenant_memory planner. Never accept client-supplied
        // correlations — a peer holding memory.backfill.apply could
        // otherwise rewrite metadata.receipt_id on arbitrary memory_record
        // ids by inventing pairings.
        let memories = match self.memory.recent(None, usize::MAX).await {
            Ok(records) => records
                .into_iter()
                .filter(|r| r.owner.pubkey == peer.pubkey)
                .collect::<Vec<_>>(),
            Err(e) => {
                return Response::Error {
                    message: format!("memory: {e}"),
                };
            }
        };
        let receipts = match self.settlement.recent(usize::MAX).await {
            Ok(receipts) => receipts
                .into_iter()
                .filter(|r| r.payer.pubkey == peer.pubkey)
                .collect::<Vec<_>>(),
            Err(e) => {
                return Response::Error {
                    message: format!("settlement: {e}"),
                };
            }
        };
        let correlations = memory_receipt_backfill_correlations(&memories, &receipts);

        match self
            .memory
            .backfill_receipt_correlation(dry_run, correlations)
            .await
        {
            Ok(outcome) => {
                // Emitted only here, after backfill_receipt_correlation
                // returned Ok — i.e. after the SAVEPOINT released and
                // the surrounding transaction committed — so the audit
                // log never claims a mutation whose data did not durably
                // land. Issuer is the acting operator (peer), matching
                // the SettlementReceiptBackfillApplied audience: the row
                // surfaces on the operator's own /audit feed under the
                // issuer==peer filter.
                self.record_peer_event(
                    peer,
                    AuditEvent {
                        id: Uuid::new_v4(),
                        timestamp_ms: epoch_ms(),
                        issuer: peer.clone(),
                        kind: AuditKind::MemoryRecordBackfillApplied {
                            row_count: outcome.row_count,
                            savepoint_name: Some(outcome.savepoint_name.clone()),
                            dry_run: outcome.dry_run,
                        },
                    },
                )
                .await;
                Response::MemoryRecordsBackfilled {
                    row_count: outcome.row_count,
                    savepoint_name: outcome.savepoint_name,
                    dry_run: outcome.dry_run,
                }
            }
            Err(e) => Response::Error {
                message: format!("memory: {e}"),
            },
        }
    }

    async fn memory_backfill_scope_allows(
        &self,
        action: &str,
        apply: bool,
        before_ms: u64,
        peer: &AgentId,
    ) -> Result<bool, String> {
        self.memory_scope_allows(action, peer, |scope| {
            permission_memory_backfill_scope_allows(action, scope, apply, before_ms)
        })
        .await
    }

    async fn memory_repair_scope_allows(
        &self,
        action: &str,
        record_id: &str,
        tier: MemoryTier,
        created_at_ms: u64,
        apply: bool,
        peer: &AgentId,
    ) -> Result<bool, String> {
        self.memory_scope_allows(action, peer, |scope| {
            permission_memory_repair_scope_allows(
                action,
                scope,
                record_id,
                memory_tier_name(tier),
                created_at_ms,
                apply,
            )
        })
        .await
    }

    async fn memory_compaction_scope_allows(
        &self,
        action: &str,
        request: MemoryCompactionScopeRequest,
        peer: &AgentId,
    ) -> Result<bool, String> {
        self.memory_scope_allows(action, peer, |scope| {
            permission_memory_compaction_scope_allows(action, scope, request)
        })
        .await
    }

    /// Returns capabilities where `peer` is either the subject (caps held
    /// against them) or `granted_by` (caps they granted). Bidirectional
    /// because both ends of a delegation are natural privacy boundaries —
    /// a peer wants to see what they're authorised for AND what they
    /// delegated out. v0 single-peer collapses to operator's own grants
    /// (subject == granted_by). Compared on the 32-byte pubkey. Sprint
    /// 58g per-peer filter.
    async fn recent_capabilities(&self, limit: usize, peer: &AgentId) -> Response {
        match self.capabilities.recent(limit).await {
            Ok(capabilities) => {
                let capabilities = capabilities
                    .into_iter()
                    .filter(|c| {
                        c.capability.subject.pubkey == peer.pubkey
                            || c.capability.granted_by.pubkey == peer.pubkey
                    })
                    .collect();
                Response::Capabilities { capabilities }
            }
            Err(e) => Response::Error {
                message: format!("permissions: {e}"),
            },
        }
    }

    async fn revoke_capability(&self, signature_b58: String, peer: &AgentId) -> Response {
        let bytes = match bs58::decode(&signature_b58).into_vec() {
            Ok(b) if b.len() == 64 => {
                let mut arr = [0u8; 64];
                arr.copy_from_slice(&b);
                arr
            }
            Ok(_) => {
                return Response::Error {
                    message: "signature must decode to 64 bytes".into(),
                };
            }
            Err(e) => {
                return Response::Error {
                    message: format!("invalid base58 signature: {e}"),
                };
            }
        };
        // Peer can only revoke caps whose subject is peer. The daemon
        // is the trust root and signs every cap, but the cap is *for*
        // the subject — a different peer must not be able to revoke it
        // by replaying a signature visible on `/capabilities/recent`.
        let owned = match self.capabilities.list_for_subject(peer.pubkey).await {
            Ok(rows) => rows,
            Err(e) => {
                return Response::Error {
                    message: format!("permissions: {e}"),
                };
            }
        };
        if !owned.iter().any(|c| c.signature == bytes) {
            match self.capabilities.is_revoked(bytes).await {
                Ok(true) => {
                    return Response::CapabilityRevoked {
                        signature_b58,
                        removed: false,
                    };
                }
                Ok(false) => {}
                Err(e) => {
                    return Response::Error {
                        message: format!("permissions: {e}"),
                    };
                }
            }

            // Audit row is issued by the daemon identity so it surfaces on
            // the operator's `/audit/recent` feed rather than the rejected
            // peer's. Matches the audience model already used by
            // OperatorPeerRevokeRejected and OperatorTokenRotationRejected.
            let event = AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: self.identity.agent_id(),
                kind: AuditKind::CapabilityRevokeRejected {
                    signature_b58: signature_b58.clone(),
                    reason: "peer is not the subject of this capability".into(),
                },
            };
            if let Err(e) = self.record_daemon_event_required(event).await {
                return audit_failure_response(e);
            }
            return Response::Error {
                message: "revoke rejected: capability subject does not match authenticated peer"
                    .into(),
            };
        }
        match self.capabilities.revoke(bytes).await {
            Ok(removed) => Response::CapabilityRevoked {
                signature_b58,
                removed,
            },
            Err(e) => Response::Error {
                message: format!("permissions: {e}"),
            },
        }
    }
}

#[cfg(test)]
impl Server {
    /// Test convenience: respond with the daemon's own identity acting as
    /// the authenticated peer. In production v0 the operator peer is the
    /// daemon's own identity, so this matches what `handle` does after
    /// the auth handshake.
    async fn op_respond(&self, req: Request) -> Response {
        let peer = self.identity.agent_id();
        self.respond(req, &peer).await
    }
}

struct CapabilityCheckOutcome {
    passed: bool,
    #[allow(dead_code)]
    required: Vec<String>,
    missing: Vec<String>,
}

struct A2aScopeCheck {
    allowed: bool,
    has_matching_action: bool,
}

struct PeerScopeCheck {
    allowed: bool,
    #[allow(dead_code)]
    has_matching_action: bool,
}

/// Outcome surface for [`Server::preempt_intent`]. Distinguishes the
/// success arm (the dispatcher returned an outcome and the matching
/// audit row landed) from the failure modes a caller (today: tests;
/// soon: the projection tick) needs to handle independently:
///
/// - [`PreemptResult::NotInFlight`] means the tracker has no entry for
///   the requested intent_id — either it never spawned, it already
///   finished, or the runner unregistered it before the projection
///   tick fired. The caller should NOT retry as a kill — the intent
///   is no longer a subprocess to preempt.
/// - [`PreemptResult::UnsupportedPlatform`] means the runtime crate's
///   dispatcher returned [`covenant_runtime::PreemptOutcome::UnsupportedPlatform`]
///   on a non-Unix host. No audit row is emitted; this is a daemon
///   configuration error the operator must fix, not a per-call audit
///   event.
/// - [`PreemptResult::AuditWriteFailed`] means the kill landed (the
///   `outcome` reflects what happened to the subprocess) but the
///   matching audit row failed to persist. The caller can choose to
///   retry the append, escalate, or surface the discrepancy.
#[derive(Debug)]
pub enum PreemptResult {
    Preempted {
        outcome: covenant_runtime::PreemptOutcome,
    },
    NotInFlight,
    UnsupportedPlatform,
    AuditWriteFailed {
        outcome: covenant_runtime::PreemptOutcome,
        error: String,
    },
}

/// Wraps a [`BudgetError`] from [`Server::register_agent_budgets`] with
/// the manifest id that failed, so startup error messages name the
/// offending agent instead of dropping a bare `serde:` line on the
/// operator (security-review L5 closure).
#[derive(Debug)]
pub struct BudgetSeedError {
    pub agent_id: String,
    pub source: BudgetError,
}

impl std::fmt::Display for BudgetSeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "seed budget for agent {:?}: {}",
            self.agent_id, self.source
        )
    }
}

impl std::error::Error for BudgetSeedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// True for audit kinds where audit-write success is a precondition for
/// returning the standard response. An attacker who can suppress these
/// rows (filled disk, exhausted inodes, file-perm flip) would otherwise
/// be invisible to the operator's `/audit/recent` view while gates still
/// produce a rejection response indistinguishable from a normal rejection.
/// Callers of these kinds must use `record_*_event_required` and fall back
/// to `audit_failure_response` on error.
fn audit_kind_requires_persistence(kind: &AuditKind) -> bool {
    matches!(
        kind,
        AuditKind::AuthenticationFailed { .. }
            | AuditKind::OperatorTokenRotationRejected { .. }
            | AuditKind::OperatorPeersListRejected { .. }
            | AuditKind::OperatorPeerRevokeRejected { .. }
            | AuditKind::A2ASenderMismatch { .. }
            | AuditKind::A2ARecipientRejected { .. }
            | AuditKind::CapabilityRevokeRejected { .. }
            | AuditKind::BudgetExhausted { .. }
            | AuditKind::BudgetPreempted { .. }
            | AuditKind::BudgetPreemptFailed { .. }
    )
}

/// Standard response when an audit write fails on a must-record kind.
/// The wire message is intentionally generic so callers can't distinguish
/// "audit broken" from "request rejected" — both end the interaction.
fn audit_failure_response(_e: AuditError) -> Response {
    Response::Error {
        message: "audit write failed; refusing to proceed".into(),
    }
}

fn chain_status_from_env() -> ChainStatus {
    let cluster = std::env::var("COVENANT_SOLANA_CLUSTER").unwrap_or_else(|_| "devnet".into());
    let rpc_url = std::env::var("COVENANT_SOLANA_RPC_URL").ok();
    let ws_url = std::env::var("COVENANT_SOLANA_WS_URL").ok();
    let program_id = std::env::var("COVENANT_PROTOCOL_PROGRAM_ID").ok();
    let covnt_mint = std::env::var("COVNT_MINT").ok();

    let mut missing = Vec::new();
    if rpc_url.is_none() {
        missing.push("COVENANT_SOLANA_RPC_URL".to_string());
    }
    if program_id.is_none() {
        missing.push("COVENANT_PROTOCOL_PROGRAM_ID".to_string());
    }
    if covnt_mint.is_none() {
        missing.push("COVNT_MINT".to_string());
    }

    ChainStatus {
        chain: "solana".into(),
        cluster,
        rpc_url,
        ws_url,
        program_id,
        covnt_mint,
        ready: missing.is_empty(),
        missing,
    }
}

/// Read a base58-encoded operator token off disk and decode it. The file
/// is expected at `<home>/peers/operator.token` and must be mode 0600 —
/// any group/world bit is treated as a credential leak and the read
/// fails. Used by daemon boot to reuse an existing token and by
/// [`Server::rotate_operator_token`] to identify which token to revoke.
fn read_operator_token_b58(path: &std::path::Path) -> std::io::Result<PeerToken> {
    require_operator_token_mode_0600(path)?;
    let s = std::fs::read_to_string(path)?;
    PeerToken::from_b58(s.trim()).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("decode token: {e}"),
        )
    })
}

/// Atomically write `token_b58` to `path` with mode 0600. Reused by
/// daemon boot (`crate::main`'s `bootstrap_operator_token`) and
/// `Server::rotate_operator_token`.
///
/// `OpenOptionsExt::mode` is honoured only on file creation. If the
/// file already exists with a permissive mode, `O_CREAT|O_TRUNC` reuses
/// the inode and our `0o600` is silently ignored. We `remove_file`
/// first to force a fresh inode, then `set_permissions` after writing
/// to defend against any umask-overlay surprises.
pub fn write_operator_token_0600(path: &std::path::Path, token_b58: &str) -> std::io::Result<()> {
    use std::fs::Permissions;
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(token_b58.as_bytes())?;
    f.write_all(b"\n")?;
    f.flush()?;
    std::fs::set_permissions(path, Permissions::from_mode(0o600))?;
    Ok(())
}

/// Refuse to read a token whose mode is anything but `0o600`. Loud
/// failure forces operator action — silently regenerating would still
/// leak the prior token to whoever could read the permissive file.
pub fn require_operator_token_mode_0600(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} is a symlink; refusing to follow (operator-token path must be a real file)",
                path.display()
            ),
        ));
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} mode is {:#o}; expected 0o600 (any group/world bit is a credential leak)",
                path.display(),
                mode
            ),
        ));
    }
    Ok(())
}

/// 6-char base58 prefix of `token`. Matches `PeerToken::Debug`'s
/// redaction posture so audit rows stay grep-correlatable with debug
/// logs without ever recording the full secret.
fn token_b58_prefix(token: &PeerToken) -> String {
    let s = token.to_b58();
    s.chars().take(6).collect()
}

/// Coarse-bucket rounding for the `tokens_remaining` value embedded in
/// the wire `Response::Error` message of an exhausted dispatch.
/// Powers-of-5 sequence — operator-readable and defeats fine-grained
/// inference of peer state in the multi-peer build. The audit row keeps
/// the precise `u64`.
fn round_tokens_remaining(n: u64) -> u64 {
    const BUCKETS: &[u64] = &[
        0,
        1,
        5,
        10,
        50,
        100,
        500,
        1_000,
        5_000,
        10_000,
        50_000,
        100_000,
        500_000,
        1_000_000,
        5_000_000,
        10_000_000,
        50_000_000,
        100_000_000,
        500_000_000,
        1_000_000_000,
        5_000_000_000,
        10_000_000_000,
    ];
    BUCKETS.iter().rev().copied().find(|&b| b <= n).unwrap_or(0)
}

fn budget_resume_state(
    intent_text: &str,
    matched_agent: &str,
    source: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut state = serde_json::Map::new();
    state.insert("intent_text".into(), intent_text.into());
    state.insert("matched_agent".into(), matched_agent.into());
    state.insert("source".into(), source.into());
    state
}

#[allow(clippy::too_many_arguments)]
fn budget_pause_checkpoint(
    intent_id: Uuid,
    agent: AgentId,
    reason: BudgetPauseReason,
    requested_credits: u64,
    tokens_remaining: u64,
    refill_eta_ms: u64,
    saved_at_ms: u64,
    resume_state: serde_json::Map<String, serde_json::Value>,
) -> BudgetPauseCheckpoint {
    BudgetPauseCheckpoint {
        version: BudgetPauseCheckpoint::VERSION,
        intent_id,
        agent,
        reason,
        requested_credits,
        tokens_remaining,
        refill_eta_ms,
        saved_at_ms,
        resume_state,
    }
}

/// Map an `AgentCard` to a stable `AgentId` for budget keying. v0 agents
/// don't yet have their own ed25519 keypair (the daemon's identity is
/// reused across every agent at dispatch time), so we synthesise a
/// deterministic one from the manifest id: zero-padded bytes for the
/// pubkey, `<id>@agent` for the display. The pubkey is opaque to the
/// budget ledger — used only as a hash-map key — so collision risk is
/// bounded by manifest-id uniqueness, which the operator already
/// enforces. The `@agent` host segment satisfies `validate_agent_id_display`'s
/// `<local>@<host>` shape so the synthesised id round-trips through
/// `JsonlLedger`'s serde without rejection.
fn agent_id_for_card(card: &AgentCard) -> AgentId {
    agent_id_for_card_id(&card.id)
}

/// Same mapping as [`agent_id_for_card`] but keyed off the raw manifest
/// id rather than the full `AgentCard`. `SubprocessTracker` entries
/// store `card.id` as a `String` (`TrackedSubprocess::agent_id`); the
/// projection tick uses this helper to rederive the same budget key the
/// dispatch path produces, so a `would_exceed` lookup on the tracked
/// agent reads the bucket the dispatcher debits into.
fn agent_id_for_card_id(card_id: &str) -> AgentId {
    let mut pk = [0u8; 32];
    for (i, b) in card_id.bytes().take(32).enumerate() {
        pk[i] = b;
    }
    AgentId::new(format!("{card_id}@agent"), pk)
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_ipc::StreamEnvelope;
    use covenant_manifest::Manifest;
    use covenant_memory::{InMemoryStore, MEMORY_BACKFILL_SAVEPOINT_NAME};
    use covenant_router::AgentCard;
    use covenant_runtime::MockRunner;
    use covenant_settlement::InMemorySettlement;

    fn stub_card(id: &str, capabilities: Vec<&str>) -> AgentCard {
        let toml = format!(
            r#"
[agent]
id = "{id}"
name = "{id}"
version = "0.0.1"
runtime = "rust-bin"
entry = "./fake"

[capabilities]
required = {caps:?}
"#,
            caps = capabilities
        );
        let m = Manifest::parse(&toml).unwrap();
        AgentCard::from_manifest_and_dir(m, PathBuf::from("/tmp/nope"))
    }

    fn hermes_stub_card(id: &str, capabilities: Vec<&str>) -> AgentCard {
        let toml = format!(
            r#"
[agent]
id = "{id}"
name = "{id}"
version = "0.0.1"
runtime = "hermes"

[capabilities]
required = {caps:?}
"#,
            caps = capabilities
        );
        let m = Manifest::parse(&toml).unwrap();
        AgentCard::from_manifest_and_dir(m, PathBuf::from("/tmp/nope"))
    }

    fn server_with(cards: Vec<AgentCard>, runner_text: &str) -> Server {
        server_with_ignore(cards, runner_text, IgnoreSet::default())
    }

    #[test]
    fn runtime_trace_to_audit_kind_pins_each_variant_mapping_preview_redaction_and_responded_to_resolved_rename(
    ) {
        // runtime_trace_to_audit_kind (line 302-346) is the bridge that
        // lifts every covenant_runtime::RuntimeTrace variant into the
        // matching covenant_audit::AuditKind row when the daemon folds
        // Hermes traces into the audit chain. No direct test today.
        //
        // Two arms are load-bearing security/operator invariants:
        //   (1) HermesToolInvoked → preview is HASHED into
        //       preview_hash_hex via covenant_audit::hash_hex, NEVER
        //       persisted verbatim. The docstring at covenant-runtime
        //       line 53-55 documents 'the daemon hashes it before
        //       persisting so the chain never embeds raw tool input'.
        //   (2) HermesApprovalResponded → AuditKind::HermesApprovalResolved
        //       (the trace variant 'Responded' is renamed to the audit
        //       variant 'Resolved' at this boundary). Every operator
        //       dashboard joining on HermesApprovalResolved depends on
        //       this rename surviving refactors.
        use covenant_runtime::RuntimeTrace;

        let intent_id = Uuid::new_v4();

        let preview = "ls -la /workspace";
        let invoked = runtime_trace_to_audit_kind(
            intent_id,
            RuntimeTrace::HermesToolInvoked {
                run_id: "run-1".into(),
                tool: "terminal".into(),
                preview: preview.into(),
            },
        );
        match invoked {
            AuditKind::HermesToolInvoked {
                intent_id: stamped,
                run_id,
                tool,
                preview_hash_hex,
            } => {
                assert_eq!(
                    stamped, intent_id,
                    "HermesToolInvoked must stamp the function-argument intent_id so the audit row ties back to the parent intent — a refactor that dropped the intent_id stamping would strand every Hermes tool invocation from the broader intent context",
                );
                assert_eq!(run_id, "run-1");
                assert_eq!(tool, "terminal");
                assert_eq!(
                    preview_hash_hex,
                    hash_hex(preview.as_bytes()),
                    "HermesToolInvoked must hash preview into preview_hash_hex via covenant_audit::hash_hex — the redaction invariant documented on covenant_runtime::RuntimeTrace::HermesToolInvoked::preview. A refactor that 'simplified' by passing the raw preview through (e.g., under a 'preview already operator-facing' rationale) would silently leak every Hermes tool-input preview verbatim into the persisted audit chain",
                );
                assert_ne!(
                    preview_hash_hex, preview,
                    "preview_hash_hex must NOT equal the raw preview string — a refactor that bypassed hash_hex entirely (e.g., by setting preview_hash_hex = preview directly) would silently break the redaction floor; this independent assertion catches that case without relying on hash_hex returning anything in particular",
                );
            }
            other => panic!(
                "HermesToolInvoked trace must map to AuditKind::HermesToolInvoked, got {other:?}"
            ),
        }

        let completed = runtime_trace_to_audit_kind(
            intent_id,
            RuntimeTrace::HermesToolCompleted {
                run_id: "run-2".into(),
                tool: "fs".into(),
                duration_ms: 1_234,
                error: true,
            },
        );
        match completed {
            AuditKind::HermesToolCompleted {
                intent_id: stamped,
                run_id,
                tool,
                duration_ms,
                error,
            } => {
                assert_eq!(stamped, intent_id);
                assert_eq!(run_id, "run-2");
                assert_eq!(tool, "fs");
                assert_eq!(
                    duration_ms, 1_234,
                    "HermesToolCompleted must pass duration_ms through verbatim — the audit row carries the latency budget operators key on; a refactor that coerced to a different width or unit would silently shift every Hermes latency dashboard",
                );
                assert!(
                    error,
                    "HermesToolCompleted must pass error through verbatim — the audit row's error flag is what distinguishes a tool-raised failure from a successful tool whose downstream pipeline later failed; a refactor that defaulted to false would silently mask every tool failure as success",
                );
            }
            other => panic!(
                "HermesToolCompleted trace must map to AuditKind::HermesToolCompleted, got {other:?}"
            ),
        }

        let requested = runtime_trace_to_audit_kind(
            intent_id,
            RuntimeTrace::HermesApprovalRequested {
                run_id: "run-3".into(),
                choices: vec!["allow".into(), "deny".into()],
            },
        );
        match requested {
            AuditKind::HermesApprovalRequested {
                intent_id: stamped,
                run_id,
                choices,
            } => {
                assert_eq!(stamped, intent_id);
                assert_eq!(run_id, "run-3");
                assert_eq!(
                    choices,
                    vec!["allow".to_string(), "deny".to_string()],
                    "HermesApprovalRequested must pass choices through verbatim in order — operator approval UIs render the choice list in the audit-supplied order so a sort or dedup pass here would change every operator's approval prompt order",
                );
            }
            other => panic!(
                "HermesApprovalRequested trace must map to AuditKind::HermesApprovalRequested, got {other:?}"
            ),
        }

        let responded = runtime_trace_to_audit_kind(
            intent_id,
            RuntimeTrace::HermesApprovalResponded {
                run_id: "run-4".into(),
                choice: "allow".into(),
                resolved: 2,
            },
        );
        match responded {
            AuditKind::HermesApprovalResolved {
                intent_id: stamped,
                run_id,
                choice,
                resolved,
            } => {
                assert_eq!(stamped, intent_id);
                assert_eq!(run_id, "run-4");
                assert_eq!(choice, "allow");
                assert_eq!(
                    resolved, 2,
                    "HermesApprovalResolved must pass resolved through verbatim — covenant_runtime::RuntimeTrace::HermesApprovalResponded::resolved documents 'kept as u64 so an upstream change to the counter width never silently truncates an audit row'; a refactor that coerced to a different width here would defeat the upstream pin",
                );
            }
            other => panic!(
                "HermesApprovalResponded trace must map to AuditKind::HermesApprovalResolved (note the rename: Responded → Resolved), got {other:?}. A refactor that 'aligned' the variant names by renaming AuditKind::HermesApprovalResolved back to HermesApprovalResponded would break every operator dashboard joining on the documented Resolved name",
            ),
        }

        // HermesFileWritten → AuditKind::HermesFileWritten passes path
        // and bytes through verbatim — workspace writes are structural
        // and the path is not redacted (unlike preview, which carries
        // user input). A refactor that hashed `path` under a 'mirror the
        // preview redaction' rationale would strand the operator file
        // tree from its audit-row identity; this pin documents that
        // path stays plain.
        let file_written = runtime_trace_to_audit_kind(
            intent_id,
            RuntimeTrace::HermesFileWritten {
                run_id: "run-5".into(),
                path: "src/main.rs".into(),
                bytes: 1_024,
            },
        );
        match file_written {
            AuditKind::HermesFileWritten {
                intent_id: stamped,
                run_id,
                path,
                bytes,
            } => {
                assert_eq!(stamped, intent_id);
                assert_eq!(run_id, "run-5");
                assert_eq!(
                    path, "src/main.rs",
                    "HermesFileWritten path must pass through verbatim — operator file-tree views key on it, and a redaction would break the join from audit row to the rendered file path",
                );
                assert_eq!(
                    bytes, 1_024,
                    "HermesFileWritten bytes must pass through verbatim as u64 — covenant_runtime::RuntimeTrace::HermesFileWritten::bytes documents the u64 width invariant; a refactor that narrowed to u32 here would silently truncate any write above 4 GiB",
                );
            }
            other => panic!(
                "HermesFileWritten trace must map to AuditKind::HermesFileWritten, got {other:?}",
            ),
        }
    }

    #[tokio::test]
    async fn spawn_runtime_event_drainer_publishes_each_trace_to_broadcast_subscribers() {
        // The /intents/:id/events SSE handler subscribes to the broadcast
        // sender owned by HttpState; the drainer publishes every trace it
        // writes to audit, so a live subscriber sees the same events as the
        // audit chain. A refactor that dropped the broadcast.send call would
        // silently leave the SSE endpoint emitting nothing while audit kept
        // working — this pin catches that regression.
        use covenant_runtime::{RuntimeTrace, StreamedTrace};

        let server = server_with_audit(Arc::new(covenant_audit::InMemoryAuditLog::new()));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<StreamedTrace>();
        let (broadcast_tx, _) = tokio::sync::broadcast::channel::<StreamedTrace>(16);
        let mut subscriber = broadcast_tx.subscribe();
        let _drainer = spawn_runtime_event_drainer(server, rx, broadcast_tx);

        let trace = StreamedTrace {
            intent_id: Uuid::new_v4(),
            issuer: AgentId::new("agent@local", [0u8; 32]),
            trace: RuntimeTrace::HermesFileWritten {
                run_id: "run-bcast".into(),
                path: "src/lib.rs".into(),
                bytes: 4_096,
            },
        };
        tx.send(trace.clone()).expect("send to drainer");

        let received = tokio::time::timeout(std::time::Duration::from_secs(2), subscriber.recv())
            .await
            .expect("broadcast subscriber must receive a trace within 2s")
            .expect("broadcast subscriber must not see a closed channel");
        assert_eq!(received.intent_id, trace.intent_id);
        assert_eq!(received.issuer, trace.issuer);
        match received.trace {
            RuntimeTrace::HermesFileWritten {
                run_id,
                path,
                bytes,
            } => {
                assert_eq!(run_id, "run-bcast");
                assert_eq!(path, "src/lib.rs");
                assert_eq!(bytes, 4_096);
            }
            other => panic!("expected HermesFileWritten on the broadcast, got {other:?}"),
        }
    }

    #[test]
    fn parse_env_bool_accepts_documented_spellings_and_rejects_unknown() {
        for v in ["1", "true", "yes", "on"] {
            assert!(
                parse_env_bool(v).unwrap(),
                "{v:?} is a documented true spelling for COVENANT_* boolean env vars",
            );
        }
        for v in ["0", "false", "no", "off"] {
            assert!(
                !parse_env_bool(v).unwrap(),
                "{v:?} is a documented false spelling for COVENANT_* boolean env vars",
            );
        }

        assert!(
            parse_env_bool(" TRUE ").unwrap(),
            "case-insensitive parsing with surrounding whitespace must keep env files portable across shells that strip or preserve quoting",
        );
        assert!(
            parse_env_bool("Yes").unwrap(),
            "mixed-case Yes must parse as true so operators editing env files do not have to memorise lowercase-only spellings",
        );
        assert!(
            !parse_env_bool(" OFF ").unwrap(),
            "case-insensitive parsing with surrounding whitespace must work for the false branch too, otherwise the trim/lowercase pair silently regresses on one side only",
        );

        let err = parse_env_bool("truee").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("truee"),
            "rejection must echo the offending value so an operator typo in COVENANT_* env vars is debuggable from logs alone: {err:?}",
        );
    }

    fn server_with_ignore(cards: Vec<AgentCard>, runner_text: &str, ignore: IgnoreSet) -> Server {
        Server::new(
            Arc::new(Router::from_cards(cards)),
            Arc::new(MockRunner::new(runner_text)),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            Arc::new(covenant_audit::InMemoryAuditLog::new()),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(ignore),
            Arc::new(ToolRegistry::from_tools(vec![
                Arc::new(covenant_mcp::native::EchoTool),
                Arc::new(covenant_mcp::native::ClockTool),
            ])),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        )
    }

    fn server_with_audit(audit: Arc<covenant_audit::InMemoryAuditLog>) -> Server {
        Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit,
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::from_tools(vec![
                Arc::new(covenant_mcp::native::EchoTool),
                Arc::new(covenant_mcp::native::ClockTool),
            ])),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        )
    }

    async fn grant_action(s: &Server, action: &str) {
        let resp = s
            .op_respond(Request::GrantCapability {
                action: action.into(),
                scope: None,
                expires_at: None,
            })
            .await;
        assert!(
            matches!(resp, Response::CapabilityGranted { .. }),
            "grant {action} failed: {resp:?}"
        );
    }

    async fn grant_scoped_action(s: &Server, action: &str, scope: serde_json::Value) {
        let resp = s
            .op_respond(Request::GrantCapability {
                action: action.into(),
                scope: Some(scope),
                expires_at: None,
            })
            .await;
        assert!(
            matches!(resp, Response::CapabilityGranted { .. }),
            "grant {action} failed: {resp:?}"
        );
    }

    async fn grant_scoped_action_to(
        s: &Server,
        peer: &AgentId,
        action: &str,
        scope: serde_json::Value,
    ) {
        let resp = s
            .respond(
                Request::GrantCapability {
                    action: action.into(),
                    scope: Some(scope),
                    expires_at: None,
                },
                peer,
            )
            .await;
        assert!(
            matches!(resp, Response::CapabilityGranted { .. }),
            "grant {action} to {} failed: {resp:?}",
            peer.display
        );
    }

    #[test]
    fn runtime_runner_config_defaults_to_trusted_local() {
        let config =
            runtime_runner_config_from_values(Path::new("covenant-home"), None, None, None, None)
                .unwrap();
        assert_eq!(config, RuntimeRunnerConfig::TrustedLocal);
        assert_eq!(config.backend_name(), "trusted-local");
    }

    #[test]
    fn runtime_runner_config_rejects_unknown_backend() {
        let err = runtime_runner_config_from_values(
            Path::new("covenant-home"),
            Some("firecracker"),
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported COVENANT_RUNTIME_BACKEND"),
            "{err}"
        );
    }

    #[test]
    fn runtime_runner_config_requires_gvisor_rootfs() {
        let err = runtime_runner_config_from_values(
            Path::new("covenant-home"),
            Some("linux-gvisor"),
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("COVENANT_GVISOR_ROOTFS"), "{err}");
    }

    #[test]
    fn runtime_runner_config_parses_gvisor_paths() {
        let config = runtime_runner_config_from_values(
            Path::new("covenant-home"),
            Some("linux-gvisor"),
            Some("rootfs"),
            Some("bin/runsc"),
            Some("scratch"),
        )
        .unwrap();

        assert_eq!(
            config,
            RuntimeRunnerConfig::LinuxGvisor {
                runsc_path: PathBuf::from("bin/runsc"),
                rootfs: PathBuf::from("rootfs"),
                scratch_root: PathBuf::from("scratch"),
            }
        );
        assert_eq!(config.backend_name(), "linux-gvisor");
    }

    #[test]
    fn runtime_runner_config_defaults_gvisor_tool_and_scratch() {
        let config = runtime_runner_config_from_values(
            Path::new("covenant-home"),
            Some("linux-gvisor"),
            Some("rootfs"),
            None,
            None,
        )
        .unwrap();

        assert_eq!(
            config,
            RuntimeRunnerConfig::LinuxGvisor {
                runsc_path: PathBuf::from("runsc"),
                rootfs: PathBuf::from("rootfs"),
                scratch_root: PathBuf::from("covenant-home")
                    .join("runtime")
                    .join("gvisor"),
            }
        );
    }

    #[test]
    fn a2a_auto_retry_scheduler_config_defaults_disabled() {
        let config =
            a2a_auto_retry_scheduler_config_from_values(None, None, None, None, None, None)
                .unwrap();

        assert!(!config.enabled);
        assert_eq!(config.interval_ms, 60_000);
        assert!(!config.policy.enabled);
        assert_eq!(config.policy.min_lease_age_ms, 300_000);
        assert_eq!(config.policy.max_attempts, 3);
        assert_eq!(config.policy.max_requeues, 1);
        assert_eq!(config.policy.scan_limit, 100);
    }

    #[test]
    fn a2a_auto_retry_scheduler_config_parses_opt_in_policy() {
        let config = a2a_auto_retry_scheduler_config_from_values(
            Some("true"),
            Some("5000"),
            Some("1000"),
            Some("5"),
            Some("2"),
            Some("50"),
        )
        .unwrap();

        assert!(config.enabled);
        assert_eq!(config.interval_ms, 5_000);
        assert!(config.policy.enabled);
        assert_eq!(config.policy.min_lease_age_ms, 1_000);
        assert_eq!(config.policy.max_attempts, 5);
        assert_eq!(config.policy.max_requeues, 2);
        assert_eq!(config.policy.scan_limit, 50);
    }

    #[test]
    fn a2a_auto_retry_scheduler_config_rejects_zero_interval() {
        let err = a2a_auto_retry_scheduler_config_from_values(
            Some("1"),
            Some("0"),
            None,
            None,
            None,
            None,
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("COVENANT_A2A_AUTO_RETRY_INTERVAL_MS"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn ping_returns_pong() {
        let s = server_with(vec![], "");
        assert_eq!(s.op_respond(Request::Ping).await, Response::Pong);
    }

    #[tokio::test]
    async fn submit_intent_writes_memory_and_correlated_settlement() {
        let s = server_with(
            vec![stub_card("research", vec!["tool.web_search"])],
            "mocked summary",
        );
        // Hard enforcement: grant the required cap up-front.
        grant_action(&s, "tool.web_search").await;
        grant_action(&s, "memory.write").await;
        let resp = s
            .op_respond(Request::SubmitIntent {
                text: "find recent papers on agent memory".into(),
                prefer_stream: None,
            })
            .await;
        match resp {
            Response::IntentResult {
                intent_id, text, ..
            } => {
                assert_eq!(text, "mocked summary");
                let receipts = s.settlement.recent(10).await.unwrap();
                assert_eq!(receipts.len(), 1);
                assert_eq!(receipts[0].memory_record_id, Some(intent_id));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn hermes_intent_dispatches_async_and_records_outcome() {
        let s = server_with(
            vec![hermes_stub_card("coder", vec!["tool.code"])],
            "built fizzbuzz.py and ran it",
        );
        grant_action(&s, "tool.code").await;
        grant_action(&s, "memory.write").await;

        let resp = s
            .op_respond(Request::SubmitIntent {
                text: "create fizzbuzz.py that prints 1 to 100".into(),
                prefer_stream: None,
            })
            .await;

        // A hermes (coding) dispatch returns immediately with status
        // "running" and an empty body — the build runs in a spawned task.
        let intent_id = match resp {
            Response::IntentResult {
                intent_id,
                status,
                text,
                ..
            } => {
                assert_eq!(
                    status, "running",
                    "hermes dispatch must be async; body was {text:?}"
                );
                assert!(text.is_empty(), "a running result carries no body yet");
                intent_id
            }
            other => panic!("expected running IntentResult, got {other:?}"),
        };

        // The spawned run finishes and records its outcome; poll for it.
        let mut done = None;
        for _ in 0..300 {
            match s.intent_outcome(&intent_id) {
                Some(v) if v["status"] != "running" => {
                    done = Some(v);
                    break;
                }
                _ => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            }
        }
        let outcome = done.expect("async outcome never left running");
        assert_eq!(outcome["status"], "ok");
        assert_eq!(outcome["text"], "built fizzbuzz.py and ran it");
        assert_eq!(outcome["matched_agent"], "coder");

        // The same intent_id lands in the audit chain, so the task page can
        // correlate the step trail back to the submitted intent.
        let events = s.audit.recent(50).await.unwrap();
        assert!(
            events.iter().any(|e| matches!(
                &e.kind,
                AuditKind::IntentDispatched { intent_id: i, .. } if *i == intent_id
            )),
            "async run must still write an IntentDispatched row for the intent",
        );
    }

    #[tokio::test]
    async fn verify_reports_no_drift_after_successful_dispatch() {
        let s = server_with(
            vec![stub_card("research", vec!["tool.web_search"])],
            "mocked summary",
        );
        grant_action(&s, "tool.web_search").await;
        grant_action(&s, "memory.write").await;
        s.op_respond(Request::SubmitIntent {
            text: "find recent papers on agent memory".into(),
            prefer_stream: None,
        })
        .await;

        let resp = s.op_respond(Request::Verify { window: 100 }).await;
        match resp {
            Response::VerifyReport {
                checks,
                drift,
                orphans_total,
                ..
            } => {
                assert!(checks.iter().all(|check| check.passed), "{checks:?}");
                assert!(checks.iter().any(|check| check.name == "memory ↔ receipts"
                    && check.message.contains("exact drift = 0")));
                assert!(drift.is_empty(), "{drift:?}");
                assert_eq!(orphans_total, 0);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_accepts_legacy_receipt_count_fallback() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let memory_id = Uuid::new_v4();
        s.memory
            .put(MemoryRecord {
                id: memory_id,
                tier: MemoryTier::Working,
                owner: me.clone(),
                text: "legacy receipt".into(),
                embedding: vec![],
                metadata: serde_json::json!({}),
                created_at: epoch_ms(),
                parent: None,
            })
            .await
            .unwrap();
        s.audit
            .record(AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: me.clone(),
                kind: AuditKind::IntentDispatched {
                    intent_id: memory_id,
                    intent_text: "legacy receipt".into(),
                    matched_agent: None,
                    result_hash_hex: hash_hex(b"legacy receipt"),
                    status: "ok".into(),
                },
            })
            .await
            .unwrap();
        s.settlement
            .record(SettlementReceipt {
                id: Uuid::new_v4(),
                payer: me,
                resource: ResourceKind::Memory,
                memory_record_id: None,
                credits_consumed: 1,
                settled_at: epoch_ms(),
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
            .unwrap();

        let resp = s.op_respond(Request::Verify { window: 100 }).await;
        match resp {
            Response::VerifyReport {
                checks,
                drift,
                orphans_total,
                ..
            } => {
                assert!(checks.iter().all(|check| check.passed), "{checks:?}");
                assert!(checks.iter().any(|check| check.name == "memory ↔ receipts"
                    && check.message.contains("legacy fallback = 1")));
                assert!(drift.is_empty(), "{drift:?}");
                assert_eq!(orphans_total, 0);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_reports_exact_receipt_correlation_drift() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let memory_id = Uuid::new_v4();
        s.memory
            .put(MemoryRecord {
                id: memory_id,
                tier: MemoryTier::Working,
                owner: me.clone(),
                text: "correlated memory".into(),
                embedding: vec![],
                metadata: serde_json::json!({}),
                created_at: epoch_ms(),
                parent: None,
            })
            .await
            .unwrap();
        s.audit
            .record(AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: me.clone(),
                kind: AuditKind::IntentDispatched {
                    intent_id: memory_id,
                    intent_text: "correlated memory".into(),
                    matched_agent: None,
                    result_hash_hex: hash_hex(b"correlated memory"),
                    status: "ok".into(),
                },
            })
            .await
            .unwrap();

        for memory_record_id in [memory_id, memory_id, Uuid::new_v4()] {
            s.settlement
                .record(SettlementReceipt {
                    id: Uuid::new_v4(),
                    payer: me.clone(),
                    resource: ResourceKind::Memory,
                    memory_record_id: Some(memory_record_id),
                    credits_consumed: 1,
                    settled_at: epoch_ms(),
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
                .unwrap();
        }

        let resp = s.op_respond(Request::Verify { window: 100 }).await;
        match resp {
            Response::VerifyReport {
                checks,
                drift,
                orphans_total,
                ..
            } => {
                assert!(orphans_total >= 2);
                assert!(checks.iter().any(|check| {
                    !check.passed
                        && check.name == "memory ↔ receipts"
                        && check.message.contains("exact drift =")
                }));
                assert!(drift
                    .iter()
                    .any(|item| item.kind == "memory_receipt_duplicate"
                        && item.id.as_deref() == Some(&memory_id.to_string())));
                assert!(drift
                    .iter()
                    .any(|item| item.kind == "receipt_without_memory_record"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn memory_repair_rejects_without_capability() {
        let s = server_with(vec![], "");
        let id = Uuid::new_v4();
        let parent = Uuid::new_v4();
        s.memory
            .put(MemoryRecord {
                id,
                tier: MemoryTier::Working,
                owner: s.identity.agent_id(),
                text: "orphaned memory".into(),
                embedding: vec![1.0],
                metadata: serde_json::json!({}),
                created_at: epoch_ms(),
                parent: Some(parent),
            })
            .await
            .unwrap();

        let resp = s
            .op_respond(Request::RepairMemory {
                request: covenant_memory::MemoryRepairRequest {
                    mode: MemoryRepairMode::DryRun,
                    command: MemoryRepairCommand::DetachParent {
                        id,
                        expected_parent: Some(parent),
                    },
                    reason: "verified stale parent".into(),
                },
            })
            .await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("memory.repair.dry_run"));
                assert!(message.contains("requires capability"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn memory_purge_accepts_matching_scope() {
        let s = server_with(vec![], "");
        let id = Uuid::new_v4();
        s.memory
            .put(MemoryRecord {
                id,
                tier: MemoryTier::Working,
                owner: s.identity.agent_id(),
                text: "expired working memory".into(),
                embedding: vec![1.0],
                metadata: serde_json::json!({}),
                created_at: 10,
                parent: None,
            })
            .await
            .unwrap();
        s.op_respond(Request::GrantCapability {
            action: "memory.purge".into(),
            scope: Some(serde_json::json!({
                "version": 1,
                "tiers": ["working"],
                "before_ms": 100
            })),
            expires_at: None,
        })
        .await;

        let resp = s
            .op_respond(Request::PurgeMemory {
                tier: Some(MemoryTier::Working),
                before_ms: 99,
            })
            .await;
        match resp {
            Response::MemoryPurged { purged } => assert_eq!(purged, 1),
            other => panic!("unexpected: {other:?}"),
        }
        assert!(s.memory.get(id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn memory_purge_rejects_scope_cutoff_exceeded_and_audits() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "memory.purge".into(),
            scope: Some(serde_json::json!({
                "version": 1,
                "tiers": ["working"],
                "before_ms": 100
            })),
            expires_at: None,
        })
        .await;

        let resp = s
            .op_respond(Request::PurgeMemory {
                tier: Some(MemoryTier::Working),
                before_ms: 101,
            })
            .await;
        match resp {
            Response::Error { message } => assert!(message.contains("capability scope")),
            other => panic!("unexpected: {other:?}"),
        }

        let events = s.audit.recent(10).await.unwrap();
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AuditKind::CapabilityScopeRejected { action, .. } if action == "memory.purge"
        )));
    }

    #[tokio::test]
    async fn memory_repair_rejects_scope_record_mismatch_and_audits() {
        let s = server_with(vec![], "");
        let id = Uuid::new_v4();
        let parent = Uuid::new_v4();
        s.memory
            .put(MemoryRecord {
                id,
                tier: MemoryTier::Working,
                owner: s.identity.agent_id(),
                text: "stale parent".into(),
                embedding: vec![1.0],
                metadata: serde_json::json!({}),
                created_at: 10,
                parent: Some(parent),
            })
            .await
            .unwrap();
        s.op_respond(Request::GrantCapability {
            action: "memory.repair.dry_run".into(),
            scope: Some(serde_json::json!({
                "version": 1,
                "record_id": Uuid::new_v4().to_string(),
                "tiers": ["working"],
                "apply": false
            })),
            expires_at: None,
        })
        .await;

        let resp = s
            .op_respond(Request::RepairMemory {
                request: covenant_memory::MemoryRepairRequest {
                    mode: MemoryRepairMode::DryRun,
                    command: MemoryRepairCommand::DetachParent {
                        id,
                        expected_parent: Some(parent),
                    },
                    reason: "verified stale parent".into(),
                },
            })
            .await;
        match resp {
            Response::Error { message } => assert!(message.contains("capability scope")),
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(
            s.memory.get(id).await.unwrap().unwrap().parent,
            Some(parent)
        );

        let events = s.audit.recent(10).await.unwrap();
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AuditKind::CapabilityScopeRejected { action, .. } if action == "memory.repair.dry_run"
        )));
    }

    #[tokio::test]
    async fn memory_repair_dry_run_returns_before_after_without_mutating_and_audits() {
        let s = server_with(vec![], "");
        let id = Uuid::new_v4();
        let parent = Uuid::new_v4();
        s.memory
            .put(MemoryRecord {
                id,
                tier: MemoryTier::Working,
                owner: s.identity.agent_id(),
                text: "stale parent".into(),
                embedding: vec![1.0],
                metadata: serde_json::json!({}),
                created_at: epoch_ms(),
                parent: Some(parent),
            })
            .await
            .unwrap();
        s.op_respond(Request::GrantCapability {
            action: "memory.repair.dry_run".into(),
            scope: None,
            expires_at: None,
        })
        .await;

        let reason = "verified stale parent";
        let resp = s
            .op_respond(Request::RepairMemory {
                request: covenant_memory::MemoryRepairRequest {
                    mode: MemoryRepairMode::DryRun,
                    command: MemoryRepairCommand::DetachParent {
                        id,
                        expected_parent: Some(parent),
                    },
                    reason: reason.into(),
                },
            })
            .await;
        match resp {
            Response::MemoryRepaired { outcome } => {
                assert_eq!(outcome.id, id);
                assert_eq!(outcome.mode, MemoryRepairMode::DryRun);
                assert!(outcome.would_change);
                assert!(!outcome.changed);
                assert_eq!(outcome.before.as_ref().and_then(|r| r.parent), Some(parent));
                assert_eq!(outcome.after.as_ref().and_then(|r| r.parent), None);
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(
            s.memory.get(id).await.unwrap().unwrap().parent,
            Some(parent)
        );

        let events = s.audit.recent(20).await.unwrap();
        let repair = events
            .iter()
            .find(|event| matches!(event.kind, AuditKind::MemoryRepairApplied { .. }))
            .expect("memory repair audit row");
        match &repair.kind {
            AuditKind::MemoryRepairApplied {
                memory_id,
                action,
                mode,
                changed,
                reason: logged_reason,
            } => {
                assert_eq!(*memory_id, id);
                assert_eq!(action, "detach_parent");
                assert_eq!(mode, "dry_run");
                assert!(!changed);
                assert_eq!(logged_reason, reason);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn memory_repair_apply_detaches_parent_with_guard_and_audits() {
        let s = server_with(vec![], "");
        let id = Uuid::new_v4();
        let parent = Uuid::new_v4();
        s.memory
            .put(MemoryRecord {
                id,
                tier: MemoryTier::Working,
                owner: s.identity.agent_id(),
                text: "stale parent".into(),
                embedding: vec![1.0],
                metadata: serde_json::json!({}),
                created_at: epoch_ms(),
                parent: Some(parent),
            })
            .await
            .unwrap();
        s.op_respond(Request::GrantCapability {
            action: "memory.repair.apply".into(),
            scope: None,
            expires_at: None,
        })
        .await;

        let resp = s
            .op_respond(Request::RepairMemory {
                request: covenant_memory::MemoryRepairRequest {
                    mode: MemoryRepairMode::Apply,
                    command: MemoryRepairCommand::DetachParent {
                        id,
                        expected_parent: Some(parent),
                    },
                    reason: "verified stale parent".into(),
                },
            })
            .await;
        match resp {
            Response::MemoryRepaired { outcome } => {
                assert_eq!(
                    outcome.action,
                    covenant_memory::MemoryRepairAction::DetachParent
                );
                assert!(outcome.changed);
                assert_eq!(outcome.after.as_ref().and_then(|r| r.parent), None);
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(s.memory.get(id).await.unwrap().unwrap().parent, None);

        let events = s.audit.recent(20).await.unwrap();
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AuditKind::MemoryRepairApplied {
                memory_id,
                mode,
                changed: true,
                ..
            } if *memory_id == id && mode == "apply"
        )));
    }

    #[tokio::test]
    async fn memory_repair_apply_rejects_parent_mismatch() {
        let s = server_with(vec![], "");
        let id = Uuid::new_v4();
        let parent = Uuid::new_v4();
        s.memory
            .put(MemoryRecord {
                id,
                tier: MemoryTier::Working,
                owner: s.identity.agent_id(),
                text: "fresh parent".into(),
                embedding: vec![1.0],
                metadata: serde_json::json!({}),
                created_at: epoch_ms(),
                parent: Some(parent),
            })
            .await
            .unwrap();
        s.op_respond(Request::GrantCapability {
            action: "memory.repair.apply".into(),
            scope: None,
            expires_at: None,
        })
        .await;

        let resp = s
            .op_respond(Request::RepairMemory {
                request: covenant_memory::MemoryRepairRequest {
                    mode: MemoryRepairMode::Apply,
                    command: MemoryRepairCommand::DetachParent {
                        id,
                        expected_parent: Some(Uuid::new_v4()),
                    },
                    reason: "verified stale parent".into(),
                },
            })
            .await;
        match resp {
            Response::Error { message } => assert!(message.contains("parent mismatch")),
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(
            s.memory.get(id).await.unwrap().unwrap().parent,
            Some(parent)
        );
    }

    #[tokio::test]
    async fn memory_compaction_rejects_without_capability() {
        let s = server_with(vec![], "");
        let resp = s
            .op_respond(Request::CompactMemory {
                request: MemoryCompactionRequest {
                    mode: MemoryRepairMode::DryRun,
                    policy: covenant_types::MemoryCompactionPolicy {
                        detach_stale_parents: true,
                        ..covenant_types::MemoryCompactionPolicy::default()
                    },
                    reason: "routine compaction".into(),
                },
            })
            .await;

        match resp {
            Response::Error { message } => {
                assert!(message.contains("memory.compact.dry_run"));
                assert!(message.contains("requires capability"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Seed `<home>/receipts/working.jsonl` with a single legacy-wire
    /// receipt row that omits the default-bearing chain fields. A backfill
    /// re-serializes those fields back as `null`, so the row counts as one
    /// changed row — letting the daemon-level tests assert the apply and
    /// dry-run outcomes without reaching into the settlement crate's
    /// internals.
    fn seed_legacy_receipt_row(home: &Path) -> String {
        let receipt = SettlementReceipt {
            id: Uuid::from_u128(0x5e7),
            payer: AgentId::new("user@local", [0u8; 32]),
            resource: ResourceKind::Memory,
            memory_record_id: None,
            credits_consumed: 7,
            settled_at: 7,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        };
        let mut value = serde_json::to_value(&receipt).unwrap();
        let obj = value.as_object_mut().unwrap();
        for key in [
            "chain",
            "cluster",
            "batch_id",
            "merkle_root",
            "tx_sig",
            "slot",
            "confirmed_at",
            "onchain_sig",
        ] {
            obj.remove(key);
        }
        let line = format!("{}\n", serde_json::to_string(&value).unwrap());
        let receipts_dir = home.join("receipts");
        std::fs::create_dir_all(&receipts_dir).unwrap();
        std::fs::write(receipts_dir.join("working.jsonl"), &line).unwrap();
        line
    }

    fn rollback_checkpoint_files(home: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(home.join("receipts"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains(".backfill-rollback-")
            })
            .collect()
    }

    #[tokio::test]
    async fn settlement_backfill_apply_repairs_rows_with_capability() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = server_with(vec![], "").with_home(dir.path().to_path_buf());
        let original = seed_legacy_receipt_row(dir.path());
        let store_path = dir.path().join("receipts").join("working.jsonl");
        s.op_respond(Request::GrantCapability {
            action: "settlement.backfill.apply".into(),
            scope: None,
            expires_at: None,
        })
        .await;

        let resp = s
            .op_respond(Request::BackfillSettlementReceipts {
                dry_run: false,
                scope_pubkey: None,
            })
            .await;

        match resp {
            Response::SettlementReceiptsBackfilled {
                row_count,
                rollback_path,
                dry_run,
            } => {
                assert_eq!(row_count, 1);
                assert!(!dry_run);
                assert!(
                    rollback_path.is_some(),
                    "apply must write a rollback checkpoint"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert_ne!(
            std::fs::read_to_string(&store_path).unwrap(),
            original,
            "apply must rewrite the store"
        );
        assert_eq!(rollback_checkpoint_files(dir.path()).len(), 1);
    }

    #[tokio::test]
    async fn settlement_backfill_dry_run_reports_without_writes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = server_with(vec![], "").with_home(dir.path().to_path_buf());
        let original = seed_legacy_receipt_row(dir.path());
        let store_path = dir.path().join("receipts").join("working.jsonl");
        s.op_respond(Request::GrantCapability {
            action: "settlement.backfill.dry_run".into(),
            scope: None,
            expires_at: None,
        })
        .await;

        let resp = s
            .op_respond(Request::BackfillSettlementReceipts {
                dry_run: true,
                scope_pubkey: None,
            })
            .await;

        match resp {
            Response::SettlementReceiptsBackfilled {
                row_count,
                rollback_path,
                dry_run,
            } => {
                assert_eq!(row_count, 1);
                assert!(dry_run);
                assert_eq!(rollback_path, None);
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(&store_path).unwrap(),
            original,
            "dry run must not touch the store"
        );
        assert!(rollback_checkpoint_files(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn settlement_backfill_apply_emits_audit_row_on_operator_feed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = server_with(vec![], "").with_home(dir.path().to_path_buf());
        seed_legacy_receipt_row(dir.path());
        s.op_respond(Request::GrantCapability {
            action: "settlement.backfill.apply".into(),
            scope: None,
            expires_at: None,
        })
        .await;

        let resp = s
            .op_respond(Request::BackfillSettlementReceipts {
                dry_run: false,
                scope_pubkey: None,
            })
            .await;
        let response_rollback = match resp {
            Response::SettlementReceiptsBackfilled { rollback_path, .. } => {
                rollback_path.expect("apply returns a rollback path")
            }
            other => panic!("unexpected: {other:?}"),
        };

        // Read through the operator feed (issuer == peer filter), not the
        // raw log, so the test pins the audience too: the row must be
        // visible to the operator who ran the backfill.
        let feed = s
            .op_respond(Request::RecentAudit {
                limit: 20,
                since_ms: None,
                prefer_stream: None,
            })
            .await;
        let events = match feed {
            Response::AuditEvents { events } => events,
            other => panic!("unexpected: {other:?}"),
        };
        let row = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::SettlementReceiptBackfillApplied { .. }))
            .expect("backfill audit row on operator feed");
        match &row.kind {
            AuditKind::SettlementReceiptBackfillApplied {
                row_count,
                rollback_path,
                dry_run,
            } => {
                assert_eq!(*row_count, 1);
                assert!(!dry_run);
                assert_eq!(
                    rollback_path.as_deref(),
                    Some(response_rollback.as_str()),
                    "audit row must reference the same rollback checkpoint the operator received",
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn settlement_backfill_dry_run_emits_audit_row_without_rollback_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = server_with(vec![], "").with_home(dir.path().to_path_buf());
        seed_legacy_receipt_row(dir.path());
        s.op_respond(Request::GrantCapability {
            action: "settlement.backfill.dry_run".into(),
            scope: None,
            expires_at: None,
        })
        .await;

        s.op_respond(Request::BackfillSettlementReceipts {
            dry_run: true,
            scope_pubkey: None,
        })
        .await;

        let feed = s
            .op_respond(Request::RecentAudit {
                limit: 20,
                since_ms: None,
                prefer_stream: None,
            })
            .await;
        let events = match feed {
            Response::AuditEvents { events } => events,
            other => panic!("unexpected: {other:?}"),
        };
        let row = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::SettlementReceiptBackfillApplied { .. }))
            .expect("dry-run backfill audit row on operator feed");
        match &row.kind {
            AuditKind::SettlementReceiptBackfillApplied {
                row_count,
                rollback_path,
                dry_run,
            } => {
                assert_eq!(*row_count, 1);
                assert!(dry_run);
                assert_eq!(
                    *rollback_path, None,
                    "dry run records no rollback checkpoint"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn settlement_backfill_rejects_without_capability() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = server_with(vec![], "").with_home(dir.path().to_path_buf());
        let original = seed_legacy_receipt_row(dir.path());
        let store_path = dir.path().join("receipts").join("working.jsonl");
        let guest = AgentId::new("guest@local", [9u8; 32]);
        s.respond(
            Request::GrantCapability {
                action: "chain.flush".into(),
                scope: None,
                expires_at: None,
            },
            &guest,
        )
        .await;

        let resp = s
            .respond(
                Request::BackfillSettlementReceipts {
                    dry_run: false,
                    scope_pubkey: None,
                },
                &guest,
            )
            .await;

        match resp {
            Response::Error { message } => {
                assert!(message.contains("settlement.backfill.apply"));
                assert!(message.contains("requires capability"));
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(&store_path).unwrap(),
            original,
            "a denied backfill must not touch the store"
        );
        assert!(rollback_checkpoint_files(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn settlement_backfill_rejects_scope_pubkey_before_auth() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = server_with(vec![], "").with_home(dir.path().to_path_buf());
        let original = seed_legacy_receipt_row(dir.path());
        let store_path = dir.path().join("receipts").join("working.jsonl");
        s.op_respond(Request::GrantCapability {
            action: "settlement.backfill.apply".into(),
            scope: None,
            expires_at: None,
        })
        .await;

        let resp = s
            .op_respond(Request::BackfillSettlementReceipts {
                dry_run: false,
                scope_pubkey: Some("othersubjectpubkeyb58".into()),
            })
            .await;

        match resp {
            Response::Error { message } => {
                assert!(
                    message.contains("--scope-pubkey is not yet supported"),
                    "scope_pubkey must be rejected on its own guard, not auth: {message}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(&store_path).unwrap(),
            original,
            "a scope_pubkey-rejected backfill must not touch the store"
        );
        assert!(rollback_checkpoint_files(dir.path()).is_empty());
    }

    /// Seed one operator-owned memory record (no `metadata.receipt_id`) and
    /// one operator-paid legacy receipt (no `memory_record_id`) so the
    /// planner pairs them on owner==payer pubkey. Returns the memory id
    /// so tests can re-read the record after backfill.
    async fn seed_legacy_memory_and_receipt(s: &Server) -> Uuid {
        let op = s.identity.agent_id();
        let memory_id = Uuid::new_v4();
        s.memory
            .put(MemoryRecord {
                id: memory_id,
                tier: MemoryTier::Working,
                owner: op.clone(),
                text: "legacy memory awaiting receipt".into(),
                embedding: Vec::new(),
                metadata: serde_json::json!({"note": "preserved on merge"}),
                created_at: 1,
                parent: None,
            })
            .await
            .unwrap();
        let receipt = SettlementReceipt {
            id: Uuid::new_v4(),
            payer: op,
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
        s.settlement.record(receipt).await.unwrap();
        memory_id
    }

    #[tokio::test]
    async fn memory_backfill_apply_repairs_rows_with_capability() {
        let s = server_with(vec![], "");
        let memory_id = seed_legacy_memory_and_receipt(&s).await;
        s.op_respond(Request::GrantCapability {
            action: "memory.backfill.apply".into(),
            scope: None,
            expires_at: None,
        })
        .await;

        let resp = s
            .op_respond(Request::BackfillMemoryRecords {
                dry_run: false,
                scope_pubkey: None,
            })
            .await;

        match resp {
            Response::MemoryRecordsBackfilled {
                row_count,
                savepoint_name,
                dry_run,
            } => {
                assert_eq!(row_count, 1);
                assert!(!dry_run);
                assert_eq!(savepoint_name, MEMORY_BACKFILL_SAVEPOINT_NAME);
            }
            other => panic!("unexpected: {other:?}"),
        }
        let record = s.memory.get(memory_id).await.unwrap().expect("record");
        assert!(
            record
                .metadata
                .get("receipt_id")
                .and_then(|v| v.as_str())
                .is_some(),
            "apply must merge receipt_id into the record metadata: {:?}",
            record.metadata
        );
        assert_eq!(
            record.metadata.get("note").and_then(|v| v.as_str()),
            Some("preserved on merge"),
            "apply must preserve pre-existing metadata keys: {:?}",
            record.metadata
        );
    }

    #[tokio::test]
    async fn memory_backfill_dry_run_reports_without_writes() {
        let s = server_with(vec![], "");
        let memory_id = seed_legacy_memory_and_receipt(&s).await;
        s.op_respond(Request::GrantCapability {
            action: "memory.backfill.dry_run".into(),
            scope: None,
            expires_at: None,
        })
        .await;

        let resp = s
            .op_respond(Request::BackfillMemoryRecords {
                dry_run: true,
                scope_pubkey: None,
            })
            .await;

        match resp {
            Response::MemoryRecordsBackfilled {
                row_count,
                savepoint_name,
                dry_run,
            } => {
                assert_eq!(row_count, 1);
                assert!(dry_run);
                assert_eq!(savepoint_name, MEMORY_BACKFILL_SAVEPOINT_NAME);
            }
            other => panic!("unexpected: {other:?}"),
        }
        let record = s.memory.get(memory_id).await.unwrap().expect("record");
        assert!(
            record.metadata.get("receipt_id").is_none(),
            "dry run must not write metadata.receipt_id: {:?}",
            record.metadata
        );
    }

    #[tokio::test]
    async fn memory_backfill_apply_emits_audit_row_on_operator_feed() {
        let s = server_with(vec![], "");
        seed_legacy_memory_and_receipt(&s).await;
        s.op_respond(Request::GrantCapability {
            action: "memory.backfill.apply".into(),
            scope: None,
            expires_at: None,
        })
        .await;

        let resp = s
            .op_respond(Request::BackfillMemoryRecords {
                dry_run: false,
                scope_pubkey: None,
            })
            .await;
        let response_savepoint = match resp {
            Response::MemoryRecordsBackfilled { savepoint_name, .. } => savepoint_name,
            other => panic!("unexpected: {other:?}"),
        };

        let feed = s
            .op_respond(Request::RecentAudit {
                limit: 20,
                since_ms: None,
                prefer_stream: None,
            })
            .await;
        let events = match feed {
            Response::AuditEvents { events } => events,
            other => panic!("unexpected: {other:?}"),
        };
        let row = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::MemoryRecordBackfillApplied { .. }))
            .expect("backfill audit row on operator feed");
        match &row.kind {
            AuditKind::MemoryRecordBackfillApplied {
                row_count,
                savepoint_name,
                dry_run,
            } => {
                assert_eq!(*row_count, 1);
                assert!(!dry_run);
                assert_eq!(
                    savepoint_name.as_deref(),
                    Some(response_savepoint.as_str()),
                    "audit row must reference the same SAVEPOINT name the operator received",
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn memory_backfill_rejects_without_capability() {
        let s = server_with(vec![], "");
        let memory_id = seed_legacy_memory_and_receipt(&s).await;
        let guest = AgentId::new("guest@local", [9u8; 32]);
        s.respond(
            Request::GrantCapability {
                action: "memory.read".into(),
                scope: None,
                expires_at: None,
            },
            &guest,
        )
        .await;

        let resp = s
            .respond(
                Request::BackfillMemoryRecords {
                    dry_run: false,
                    scope_pubkey: None,
                },
                &guest,
            )
            .await;

        match resp {
            Response::Error { message } => {
                assert!(message.contains("memory.backfill.apply"));
                assert!(message.contains("requires capability"));
            }
            other => panic!("unexpected: {other:?}"),
        }
        let record = s.memory.get(memory_id).await.unwrap().expect("record");
        assert!(
            record.metadata.get("receipt_id").is_none(),
            "a denied backfill must not touch the store: {:?}",
            record.metadata
        );
    }

    #[tokio::test]
    async fn memory_backfill_rejects_non_operator_even_with_capability() {
        // Guest holding memory.backfill.apply must still be rejected on the
        // operator-identity gate. Mirrors the settlement-backfill
        // operator-identity check; the cap alone is not enough.
        let s = server_with(vec![], "");
        let memory_id = seed_legacy_memory_and_receipt(&s).await;
        let guest = AgentId::new("guest@local", [9u8; 32]);
        s.respond(
            Request::GrantCapability {
                action: "memory.backfill.apply".into(),
                scope: None,
                expires_at: None,
            },
            &guest,
        )
        .await;

        let resp = s
            .respond(
                Request::BackfillMemoryRecords {
                    dry_run: false,
                    scope_pubkey: None,
                },
                &guest,
            )
            .await;

        match resp {
            Response::Error { message } => {
                assert!(
                    message.contains("operator identity"),
                    "memory backfill must reject non-operator peers even when they hold the cap: {message}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
        let record = s.memory.get(memory_id).await.unwrap().expect("record");
        assert!(
            record.metadata.get("receipt_id").is_none(),
            "non-operator backfill must not touch the store: {:?}",
            record.metadata
        );
    }

    #[tokio::test]
    async fn memory_backfill_rejects_scope_pubkey_before_auth() {
        let s = server_with(vec![], "");
        let memory_id = seed_legacy_memory_and_receipt(&s).await;
        s.op_respond(Request::GrantCapability {
            action: "memory.backfill.apply".into(),
            scope: None,
            expires_at: None,
        })
        .await;

        let resp = s
            .op_respond(Request::BackfillMemoryRecords {
                dry_run: false,
                scope_pubkey: Some("othersubjectpubkeyb58".into()),
            })
            .await;

        match resp {
            Response::Error { message } => {
                assert!(
                    message.contains("--scope-pubkey is not yet supported"),
                    "scope_pubkey must be rejected on its own guard, not auth: {message}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
        let record = s.memory.get(memory_id).await.unwrap().expect("record");
        assert!(
            record.metadata.get("receipt_id").is_none(),
            "scope_pubkey-rejected backfill must not touch the store: {:?}",
            record.metadata
        );
    }

    #[tokio::test]
    async fn memory_compaction_rejects_non_operator_even_with_capability() {
        let s = server_with(vec![], "");
        let guest = AgentId::new("guest@local", [9u8; 32]);
        s.respond(
            Request::GrantCapability {
                action: "memory.compact.dry_run".into(),
                scope: None,
                expires_at: None,
            },
            &guest,
        )
        .await;

        let resp = s
            .respond(
                Request::CompactMemory {
                    request: MemoryCompactionRequest {
                        mode: MemoryRepairMode::DryRun,
                        policy: covenant_types::MemoryCompactionPolicy {
                            detach_stale_parents: true,
                            ..covenant_types::MemoryCompactionPolicy::default()
                        },
                        reason: "routine compaction".into(),
                    },
                },
                &guest,
            )
            .await;

        match resp {
            Response::Error { message } => {
                assert!(message.contains("operator identity"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn memory_compaction_rejects_scope_cutoff_exceeded_and_audits() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "memory.compact.apply".into(),
            scope: Some(serde_json::json!({
                "version": 1,
                "tiers": ["working"],
                "before_ms": 20,
                "apply": true
            })),
            expires_at: None,
        })
        .await;

        let resp = s
            .op_respond(Request::CompactMemory {
                request: MemoryCompactionRequest {
                    mode: MemoryRepairMode::Apply,
                    policy: covenant_types::MemoryCompactionPolicy {
                        delete_working_before_ms: Some(21),
                        ..covenant_types::MemoryCompactionPolicy::default()
                    },
                    reason: "age-based compaction".into(),
                },
            })
            .await;

        match resp {
            Response::Error { message } => assert!(message.contains("capability scope")),
            other => panic!("unexpected: {other:?}"),
        }

        let events = s.audit.recent(10).await.unwrap();
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AuditKind::CapabilityScopeRejected { action, .. } if action == "memory.compact.apply"
        )));
    }

    #[tokio::test]
    async fn memory_compaction_apply_deletes_marks_detaches_and_audits() {
        let s = server_with(vec![], "");
        let old_working = Uuid::new_v4();
        let old_episodic = Uuid::new_v4();
        let child = Uuid::new_v4();
        let longterm = Uuid::new_v4();
        s.memory
            .put(MemoryRecord {
                id: old_working,
                tier: MemoryTier::Working,
                owner: s.identity.agent_id(),
                text: "old working".into(),
                embedding: vec![1.0],
                metadata: serde_json::json!({}),
                created_at: 10,
                parent: None,
            })
            .await
            .unwrap();
        s.memory
            .put(MemoryRecord {
                id: old_episodic,
                tier: MemoryTier::Episodic,
                owner: s.identity.agent_id(),
                text: "old episodic".into(),
                embedding: vec![1.0],
                metadata: serde_json::json!({}),
                created_at: 10,
                parent: None,
            })
            .await
            .unwrap();
        s.memory
            .put(MemoryRecord {
                id: child,
                tier: MemoryTier::Episodic,
                owner: s.identity.agent_id(),
                text: "child".into(),
                embedding: vec![1.0],
                metadata: serde_json::json!({}),
                created_at: 50,
                parent: Some(old_working),
            })
            .await
            .unwrap();
        s.memory
            .put(MemoryRecord {
                id: longterm,
                tier: MemoryTier::LongTerm,
                owner: s.identity.agent_id(),
                text: "durable context".into(),
                embedding: vec![1.0],
                metadata: serde_json::json!({}),
                created_at: 10,
                parent: None,
            })
            .await
            .unwrap();
        s.op_respond(Request::GrantCapability {
            action: "memory.compact.apply".into(),
            scope: None,
            expires_at: None,
        })
        .await;

        let reason = "age-based compaction";
        let resp = s
            .op_respond(Request::CompactMemory {
                request: MemoryCompactionRequest {
                    mode: MemoryRepairMode::Apply,
                    policy: covenant_types::MemoryCompactionPolicy {
                        delete_working_before_ms: Some(20),
                        delete_episodic_before_ms: Some(20),
                        mark_longterm_stale_before_ms: Some(20),
                        detach_stale_parents: true,
                        marked_at_ms: Some(99),
                    },
                    reason: reason.into(),
                },
            })
            .await;

        match resp {
            Response::MemoryCompacted { outcome } => {
                let mut expected_deleted = vec![old_working, old_episodic];
                expected_deleted.sort();
                assert!(outcome.changed);
                assert_eq!(outcome.deleted, expected_deleted);
                assert_eq!(outcome.parents_detached, vec![child]);
                assert_eq!(outcome.stale_marked, vec![longterm]);
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(s.memory.get(old_working).await.unwrap().is_none());
        assert!(s.memory.get(old_episodic).await.unwrap().is_none());
        assert_eq!(s.memory.get(child).await.unwrap().unwrap().parent, None);
        let durable = s.memory.get(longterm).await.unwrap().unwrap();
        assert_eq!(durable.metadata["stale_context"]["marked_at_ms"], 99);

        let events = s.audit.recent(20).await.unwrap();
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AuditKind::MemoryCompactionApplied {
                mode,
                changed: true,
                reason: logged_reason,
                stale_marked,
                parents_detached,
                ..
            } if mode == "apply"
                && logged_reason == reason
                && stale_marked == &vec![longterm]
                && parents_detached == &vec![child]
        )));
    }

    #[tokio::test]
    async fn verify_reports_actionable_memory_drift_items() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let memory_id = Uuid::new_v4();
        let missing_parent = Uuid::new_v4();
        s.memory
            .put(MemoryRecord {
                id: memory_id,
                tier: MemoryTier::Working,
                owner: me.clone(),
                text: "orphaned memory".into(),
                embedding: vec![],
                metadata: serde_json::json!({}),
                created_at: epoch_ms(),
                parent: Some(missing_parent),
            })
            .await
            .unwrap();

        let audit_only_id = Uuid::new_v4();
        s.audit
            .record(AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: me,
                kind: AuditKind::IntentDispatched {
                    intent_id: audit_only_id,
                    intent_text: "audit without memory".into(),
                    matched_agent: None,
                    result_hash_hex: hash_hex(b""),
                    status: "ok".into(),
                },
            })
            .await
            .unwrap();

        let resp = s.op_respond(Request::Verify { window: 100 }).await;
        match resp {
            Response::VerifyReport {
                drift,
                orphans_total,
                ..
            } => {
                assert!(
                    orphans_total >= 4,
                    "expected multiple drift rows: {drift:?}"
                );
                assert!(drift.iter().any(|item| {
                    item.kind == "memory_without_audit"
                        && item.id.as_deref() == Some(&memory_id.to_string())
                }));
                assert!(drift.iter().any(|item| {
                    item.kind == "audit_without_memory"
                        && item.id.as_deref() == Some(&audit_only_id.to_string())
                }));
                assert!(drift.iter().any(|item| {
                    item.kind == "memory_stale_parent"
                        && item.id.as_deref() == Some(&memory_id.to_string())
                        && item.message.contains(&missing_parent.to_string())
                }));
                assert!(drift
                    .iter()
                    .any(|item| item.kind == "memory_without_receipt"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_reports_memory_self_parent_drift() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let memory_id = Uuid::new_v4();
        s.memory
            .put(MemoryRecord {
                id: memory_id,
                tier: MemoryTier::Working,
                owner: me.clone(),
                text: "self-referencing memory".into(),
                embedding: vec![],
                metadata: serde_json::json!({}),
                created_at: epoch_ms(),
                parent: Some(memory_id),
            })
            .await
            .unwrap();

        let resp = s.op_respond(Request::Verify { window: 100 }).await;
        match resp {
            Response::VerifyReport {
                drift,
                orphans_total,
                checks,
                ..
            } => {
                let self_parent = drift
                    .iter()
                    .find(|item| {
                        item.kind == "memory_self_parent"
                            && item.id.as_deref() == Some(&memory_id.to_string())
                    })
                    .unwrap_or_else(|| panic!("expected memory_self_parent: {drift:?}"));
                assert!(
                    self_parent.repair.contains("detach_parent"),
                    "repair hint should name detach_parent: {}",
                    self_parent.repair
                );
                assert!(
                    !drift.iter().any(|item| {
                        item.kind == "memory_stale_parent"
                            && item.id.as_deref() == Some(&memory_id.to_string())
                    }),
                    "self-parent must not double-report as memory_stale_parent: {drift:?}"
                );
                let parent_check = checks
                    .iter()
                    .find(|c| c.name == "memory parent references")
                    .unwrap_or_else(|| panic!("expected parent references check: {checks:?}"));
                assert!(!parent_check.passed);
                assert!(
                    parent_check.message.contains("1 self-parent reference"),
                    "check message should count self-parent refs: {}",
                    parent_check.message
                );
                assert!(orphans_total >= 1);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_reports_memory_empty_text_drift() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let memory_id = Uuid::new_v4();
        s.memory
            .put(MemoryRecord {
                id: memory_id,
                tier: MemoryTier::Working,
                owner: me.clone(),
                text: String::new(),
                embedding: vec![],
                metadata: serde_json::json!({}),
                created_at: epoch_ms(),
                parent: None,
            })
            .await
            .unwrap();

        let resp = s.op_respond(Request::Verify { window: 100 }).await;
        match resp {
            Response::VerifyReport {
                drift,
                orphans_total,
                checks,
                ..
            } => {
                let empty = drift
                    .iter()
                    .find(|item| {
                        item.kind == "memory_empty_text"
                            && item.id.as_deref() == Some(&memory_id.to_string())
                    })
                    .unwrap_or_else(|| panic!("expected memory_empty_text: {drift:?}"));
                assert!(
                    empty.repair.contains("delete_record"),
                    "repair hint should name delete_record: {}",
                    empty.repair
                );
                let integrity = checks
                    .iter()
                    .find(|c| c.name == "memory record integrity")
                    .unwrap_or_else(|| panic!("expected integrity check: {checks:?}"));
                assert!(!integrity.passed);
                assert!(
                    integrity.message.contains("1 empty-text record"),
                    "check message should count empty-text records: {}",
                    integrity.message
                );
                assert!(orphans_total >= 1);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_reports_memory_nan_embedding_drift() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let memory_id = Uuid::new_v4();
        s.memory
            .put(MemoryRecord {
                id: memory_id,
                tier: MemoryTier::Working,
                owner: me.clone(),
                text: "nan-embedding fixture".into(),
                embedding: vec![1.0, f32::NAN, 0.5],
                metadata: serde_json::json!({}),
                created_at: epoch_ms(),
                parent: None,
            })
            .await
            .unwrap();

        let resp = s.op_respond(Request::Verify { window: 100 }).await;
        match resp {
            Response::VerifyReport {
                drift,
                orphans_total,
                checks,
                ..
            } => {
                let nan = drift
                    .iter()
                    .find(|item| {
                        item.kind == "memory_nan_embedding"
                            && item.id.as_deref() == Some(&memory_id.to_string())
                    })
                    .unwrap_or_else(|| panic!("expected memory_nan_embedding: {drift:?}"));
                assert!(
                    nan.repair.contains("delete_record"),
                    "repair hint should name delete_record: {}",
                    nan.repair
                );
                assert!(
                    !drift.iter().any(|item| item.kind == "memory_empty_text"
                        && item.id.as_deref() == Some(&memory_id.to_string())),
                    "non-empty text must not double-report as memory_empty_text: {drift:?}"
                );
                let integrity = checks
                    .iter()
                    .find(|c| c.name == "memory record integrity")
                    .unwrap_or_else(|| panic!("expected integrity check: {checks:?}"));
                assert!(!integrity.passed);
                assert!(
                    integrity.message.contains("1 NaN-embedding record"),
                    "check message should count NaN-embedding records: {}",
                    integrity.message
                );
                assert!(orphans_total >= 1);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_reports_receipt_confirmed_without_chain_drift() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let receipt_id = Uuid::new_v4();
        s.settlement
            .record(SettlementReceipt {
                id: receipt_id,
                payer: me.clone(),
                resource: ResourceKind::Compute,
                memory_record_id: None,
                credits_consumed: 1,
                settled_at: 1_000,
                chain: None,
                cluster: None,
                batch_id: None,
                merkle_root: None,
                tx_sig: None,
                slot: None,
                confirmed_at: Some(2_000),
                onchain_sig: None,
            })
            .await
            .unwrap();

        let resp = s.op_respond(Request::Verify { window: 100 }).await;
        match resp {
            Response::VerifyReport {
                drift,
                orphans_total,
                checks,
                ..
            } => {
                let row = drift
                    .iter()
                    .find(|item| {
                        item.kind == "receipt_confirmed_without_chain"
                            && item.id.as_deref() == Some(&receipt_id.to_string())
                    })
                    .unwrap_or_else(|| {
                        panic!("expected receipt_confirmed_without_chain: {drift:?}")
                    });
                assert!(
                    row.repair.contains("annotate_receipt"),
                    "repair hint should name annotate_receipt: {}",
                    row.repair
                );
                let integrity = checks
                    .iter()
                    .find(|c| c.name == "settlement receipt integrity")
                    .unwrap_or_else(|| panic!("expected receipt integrity check: {checks:?}"));
                assert!(!integrity.passed);
                assert!(
                    integrity
                        .message
                        .contains("1 confirmed-without-chain receipt"),
                    "check message should count confirmed-without-chain receipts: {}",
                    integrity.message
                );
                assert!(orphans_total >= 1);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_reports_receipt_chain_partial_drift() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let receipt_id = Uuid::new_v4();
        s.settlement
            .record(SettlementReceipt {
                id: receipt_id,
                payer: me.clone(),
                resource: ResourceKind::Compute,
                memory_record_id: None,
                credits_consumed: 1,
                settled_at: 1_000,
                chain: Some("solana".into()),
                cluster: Some("devnet".into()),
                batch_id: None,
                merkle_root: None,
                tx_sig: None,
                slot: None,
                confirmed_at: None,
                onchain_sig: None,
            })
            .await
            .unwrap();

        let resp = s.op_respond(Request::Verify { window: 100 }).await;
        match resp {
            Response::VerifyReport {
                drift,
                orphans_total,
                checks,
                ..
            } => {
                let row = drift
                    .iter()
                    .find(|item| {
                        item.kind == "receipt_chain_partial"
                            && item.id.as_deref() == Some(&receipt_id.to_string())
                    })
                    .unwrap_or_else(|| panic!("expected receipt_chain_partial: {drift:?}"));
                assert!(
                    row.message.contains("chain=true")
                        && row.message.contains("cluster=true")
                        && row.message.contains("batch_id=false")
                        && row.message.contains("merkle_root=false"),
                    "drift message should record every bundle field's set state: {}",
                    row.message
                );
                assert!(
                    row.repair.contains("single bundle"),
                    "repair hint should name the single-bundle invariant: {}",
                    row.repair
                );
                assert!(
                    !drift.iter().any(|item| {
                        item.kind == "receipt_confirmed_without_chain"
                            && item.id.as_deref() == Some(&receipt_id.to_string())
                    }),
                    "chain=Some must not double-report under confirmed_without_chain: {drift:?}"
                );
                let integrity = checks
                    .iter()
                    .find(|c| c.name == "settlement receipt integrity")
                    .unwrap_or_else(|| panic!("expected receipt integrity check: {checks:?}"));
                assert!(!integrity.passed);
                assert!(
                    integrity.message.contains("1 partial-chain-bundle receipt"),
                    "check message should count partial-bundle receipts: {}",
                    integrity.message
                );
                assert!(orphans_total >= 1);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_reports_receipt_tx_sig_onchain_sig_diverged_drift() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let receipt_id = Uuid::new_v4();
        s.settlement
            .record(SettlementReceipt {
                id: receipt_id,
                payer: me.clone(),
                resource: ResourceKind::Compute,
                memory_record_id: None,
                credits_consumed: 1,
                settled_at: 1_000,
                chain: None,
                cluster: None,
                batch_id: None,
                merkle_root: None,
                tx_sig: Some("sig-from-annotate".into()),
                slot: None,
                confirmed_at: None,
                onchain_sig: Some("sig-rewritten-out-of-band".into()),
            })
            .await
            .unwrap();

        let resp = s.op_respond(Request::Verify { window: 100 }).await;
        match resp {
            Response::VerifyReport {
                drift,
                orphans_total,
                checks,
                ..
            } => {
                let row = drift
                    .iter()
                    .find(|item| {
                        item.kind == "receipt_tx_sig_onchain_sig_diverged"
                            && item.id.as_deref() == Some(&receipt_id.to_string())
                    })
                    .unwrap_or_else(|| {
                        panic!("expected receipt_tx_sig_onchain_sig_diverged: {drift:?}")
                    });
                assert!(
                    row.message.contains("tx_sig=sig-from-annotate")
                        && row
                            .message
                            .contains("onchain_sig=sig-rewritten-out-of-band"),
                    "drift message should record both signature values: {}",
                    row.message
                );
                assert!(
                    row.repair.contains("annotate_receipt"),
                    "repair hint should name annotate_receipt: {}",
                    row.repair
                );
                assert!(
                    !drift.iter().any(|item| {
                        item.kind == "receipt_confirmed_without_chain"
                            && item.id.as_deref() == Some(&receipt_id.to_string())
                    }),
                    "confirmed_at=None must not co-fire confirmed_without_chain: {drift:?}"
                );
                assert!(
                    !drift.iter().any(|item| {
                        item.kind == "receipt_chain_partial"
                            && item.id.as_deref() == Some(&receipt_id.to_string())
                    }),
                    "chain bundle fully unset must not co-fire chain_partial: {drift:?}"
                );
                let integrity = checks
                    .iter()
                    .find(|c| c.name == "settlement receipt integrity")
                    .unwrap_or_else(|| panic!("expected receipt integrity check: {checks:?}"));
                assert!(!integrity.passed);
                assert!(
                    integrity
                        .message
                        .contains("1 tx-sig/onchain-sig-diverged receipt"),
                    "check message should count diverged receipts: {}",
                    integrity.message
                );
                assert!(orphans_total >= 1);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_tolerates_tx_sig_only_and_onchain_sig_only_legacy_state() {
        // covenant-settlement/src/lib.rs:164 treats tx_sig.is_some() ||
        // onchain_sig.is_some() as "onchain_settled" for legacy/forward
        // compatibility. The diverged check must not fire when only one
        // field is populated; only a two-Some disagreement is out-of-band.
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let tx_only_id = Uuid::new_v4();
        let onchain_only_id = Uuid::new_v4();
        s.settlement
            .record(SettlementReceipt {
                id: tx_only_id,
                payer: me.clone(),
                resource: ResourceKind::Compute,
                memory_record_id: None,
                credits_consumed: 1,
                settled_at: 1_000,
                chain: None,
                cluster: None,
                batch_id: None,
                merkle_root: None,
                tx_sig: Some("tx-only".into()),
                slot: None,
                confirmed_at: None,
                onchain_sig: None,
            })
            .await
            .unwrap();
        s.settlement
            .record(SettlementReceipt {
                id: onchain_only_id,
                payer: me.clone(),
                resource: ResourceKind::Compute,
                memory_record_id: None,
                credits_consumed: 1,
                settled_at: 1_000,
                chain: None,
                cluster: None,
                batch_id: None,
                merkle_root: None,
                tx_sig: None,
                slot: None,
                confirmed_at: None,
                onchain_sig: Some("onchain-only".into()),
            })
            .await
            .unwrap();

        let resp = s.op_respond(Request::Verify { window: 100 }).await;
        match resp {
            Response::VerifyReport { drift, checks, .. } => {
                assert!(
                    !drift.iter().any(|item| {
                        item.kind == "receipt_tx_sig_onchain_sig_diverged"
                            && (item.id.as_deref() == Some(&tx_only_id.to_string())
                                || item.id.as_deref() == Some(&onchain_only_id.to_string()))
                    }),
                    "Some+None in either direction must not fire diverged: {drift:?}"
                );
                let integrity = checks
                    .iter()
                    .find(|c| c.name == "settlement receipt integrity")
                    .unwrap_or_else(|| panic!("expected receipt integrity check: {checks:?}"));
                assert!(
                    integrity
                        .message
                        .contains("0 tx-sig/onchain-sig-diverged receipt"),
                    "diverged counter must remain zero for legacy-compat singletons: {}",
                    integrity.message
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_reports_memory_receipt_settled_before_created_drift() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let memory_id = Uuid::new_v4();
        let receipt_id = Uuid::new_v4();
        s.memory
            .put(MemoryRecord {
                id: memory_id,
                tier: MemoryTier::Working,
                owner: me.clone(),
                text: "settled-before-created fixture".into(),
                embedding: vec![],
                metadata: serde_json::json!({}),
                created_at: 2_000,
                parent: None,
            })
            .await
            .unwrap();
        s.audit
            .record(AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: me.clone(),
                kind: AuditKind::IntentDispatched {
                    intent_id: memory_id,
                    intent_text: "settled-before-created fixture".into(),
                    matched_agent: None,
                    result_hash_hex: hash_hex(b"settled-before-created fixture"),
                    status: "ok".into(),
                },
            })
            .await
            .unwrap();
        s.settlement
            .record(SettlementReceipt {
                id: receipt_id,
                payer: me.clone(),
                resource: ResourceKind::Memory,
                memory_record_id: Some(memory_id),
                credits_consumed: 1,
                settled_at: 1_000,
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
            .unwrap();

        let resp = s.op_respond(Request::Verify { window: 100 }).await;
        match resp {
            Response::VerifyReport {
                drift,
                orphans_total,
                checks,
                ..
            } => {
                let row = drift
                    .iter()
                    .find(|item| {
                        item.kind == "memory_receipt_settled_before_created"
                            && item.id.as_deref() == Some(&receipt_id.to_string())
                    })
                    .unwrap_or_else(|| {
                        panic!("expected memory_receipt_settled_before_created: {drift:?}")
                    });
                assert!(
                    row.message.contains("settled_at=1000")
                        && row.message.contains("created_at=2000"),
                    "message should record both timestamps: {}",
                    row.message
                );
                assert!(
                    !drift.iter().any(|item| {
                        item.kind == "memory_receipt_owner_mismatch"
                            && item.id.as_deref() == Some(&receipt_id.to_string())
                    }),
                    "matching payer must not double-report as owner mismatch: {drift:?}"
                );
                let receipt_check = checks
                    .iter()
                    .find(|c| c.name == "memory ↔ receipts")
                    .unwrap_or_else(|| panic!("expected receipts check: {checks:?}"));
                assert!(!receipt_check.passed);
                assert!(orphans_total >= 1);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_reports_intent_dispatched_duplicate_drift() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let intent_id = Uuid::new_v4();
        s.memory
            .put(MemoryRecord {
                id: intent_id,
                tier: MemoryTier::Working,
                owner: me.clone(),
                text: "duplicate-dispatch intent".into(),
                embedding: vec![],
                metadata: serde_json::json!({}),
                created_at: epoch_ms(),
                parent: None,
            })
            .await
            .unwrap();
        for _ in 0..2 {
            s.audit
                .record(AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: epoch_ms(),
                    issuer: me.clone(),
                    kind: AuditKind::IntentDispatched {
                        intent_id,
                        intent_text: "duplicate-dispatch intent".into(),
                        matched_agent: None,
                        result_hash_hex: hash_hex(b"duplicate-dispatch intent"),
                        status: "ok".into(),
                    },
                })
                .await
                .unwrap();
        }

        let resp = s.op_respond(Request::Verify { window: 100 }).await;
        match resp {
            Response::VerifyReport {
                drift,
                orphans_total,
                checks,
                ..
            } => {
                let duplicates: Vec<_> = drift
                    .iter()
                    .filter(|item| {
                        item.kind == "intent_dispatched_duplicate"
                            && item.id.as_deref() == Some(&intent_id.to_string())
                    })
                    .collect();
                assert_eq!(
                    duplicates.len(),
                    1,
                    "exactly one intent_dispatched_duplicate row per intent_id: {drift:?}"
                );
                assert!(
                    duplicates[0].message.contains("2 IntentDispatched"),
                    "message should record the observed count: {}",
                    duplicates[0].message
                );
                assert!(
                    !drift.iter().any(|item| item.kind == "memory_without_audit"
                        || item.kind == "audit_without_memory"),
                    "matched intent must not also be reported as an orphan: {drift:?}"
                );
                let audit_check = checks
                    .iter()
                    .find(|c| c.name == "memory ↔ audit")
                    .unwrap_or_else(|| panic!("expected memory ↔ audit check: {checks:?}"));
                assert!(!audit_check.passed);
                assert!(
                    audit_check.message.contains("1 duplicate intent"),
                    "check message should count duplicates: {}",
                    audit_check.message
                );
                assert!(orphans_total >= 1);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_reports_memory_receipt_resource_mismatch_drift() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let compute_receipt_id = Uuid::new_v4();
        let bogus_memory_id = Uuid::new_v4();
        s.settlement
            .record(SettlementReceipt {
                id: compute_receipt_id,
                payer: me.clone(),
                resource: ResourceKind::Compute,
                memory_record_id: Some(bogus_memory_id),
                credits_consumed: 1,
                settled_at: epoch_ms(),
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
            .unwrap();

        let resp = s.op_respond(Request::Verify { window: 100 }).await;
        match resp {
            Response::VerifyReport {
                drift,
                orphans_total,
                checks,
                ..
            } => {
                let mismatch = drift
                    .iter()
                    .find(|item| {
                        item.kind == "memory_receipt_resource_mismatch"
                            && item.id.as_deref() == Some(&compute_receipt_id.to_string())
                    })
                    .unwrap_or_else(|| {
                        panic!("expected memory_receipt_resource_mismatch: {drift:?}")
                    });
                assert!(
                    mismatch.message.contains("Compute"),
                    "message should record observed resource: {}",
                    mismatch.message
                );
                assert!(
                    !drift.iter().any(|item| {
                        item.kind == "receipt_without_memory_record"
                            && item.id.as_deref() == Some(&compute_receipt_id.to_string())
                    }),
                    "cross-resource receipt must not double-report under receipt_without_memory_record: {drift:?}"
                );
                let receipt_check = checks
                    .iter()
                    .find(|c| c.name == "memory ↔ receipts")
                    .unwrap_or_else(|| panic!("expected receipts check: {checks:?}"));
                assert!(!receipt_check.passed);
                assert!(
                    receipt_check.message.contains("resource mismatch = 1"),
                    "check message should count resource mismatches: {}",
                    receipt_check.message
                );
                assert!(orphans_total >= 1);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_reports_memory_parent_cycle_drift() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let a_id = Uuid::new_v4();
        let b_id = Uuid::new_v4();
        s.memory
            .put(MemoryRecord {
                id: a_id,
                tier: MemoryTier::Working,
                owner: me.clone(),
                text: "node a".into(),
                embedding: vec![],
                metadata: serde_json::json!({}),
                created_at: epoch_ms(),
                parent: Some(b_id),
            })
            .await
            .unwrap();
        s.memory
            .put(MemoryRecord {
                id: b_id,
                tier: MemoryTier::Working,
                owner: me.clone(),
                text: "node b".into(),
                embedding: vec![],
                metadata: serde_json::json!({}),
                created_at: epoch_ms(),
                parent: Some(a_id),
            })
            .await
            .unwrap();

        let resp = s.op_respond(Request::Verify { window: 100 }).await;
        match resp {
            Response::VerifyReport {
                drift,
                orphans_total,
                checks,
                ..
            } => {
                for id in [a_id, b_id] {
                    let cycle = drift
                        .iter()
                        .find(|item| {
                            item.kind == "memory_parent_cycle"
                                && item.id.as_deref() == Some(&id.to_string())
                        })
                        .unwrap_or_else(|| {
                            panic!("expected memory_parent_cycle for {id}: {drift:?}")
                        });
                    assert!(
                        cycle.repair.contains("detach_parent"),
                        "repair hint should name detach_parent: {}",
                        cycle.repair
                    );
                }
                assert!(
                    !drift.iter().any(|item| item.kind == "memory_stale_parent"
                        || item.kind == "memory_self_parent"),
                    "two-hop cycle must not double-report as stale or self parent: {drift:?}"
                );
                let parent_check = checks
                    .iter()
                    .find(|c| c.name == "memory parent references")
                    .unwrap_or_else(|| panic!("expected parent references check: {checks:?}"));
                assert!(!parent_check.passed);
                assert!(
                    parent_check.message.contains("2 parent cycle"),
                    "check message should count cycles: {}",
                    parent_check.message
                );
                assert!(orphans_total >= 2);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn submit_intent_rejects_when_capabilities_missing() {
        let s = server_with(
            vec![stub_card("research", vec!["tool.web_search"])],
            "mocked summary",
        );
        grant_action(&s, "memory.write").await;
        let resp = s
            .op_respond(Request::SubmitIntent {
                text: "find recent papers on agent memory".into(),
                prefer_stream: None,
            })
            .await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("missing capabilities"));
                assert!(message.contains("tool.web_search"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn revoke_capability_takes_it_out_of_circulation() {
        let s = server_with(
            vec![stub_card("research", vec!["tool.web_search"])],
            "mocked summary",
        );
        grant_action(&s, "memory.write").await;
        let grant = s
            .op_respond(Request::GrantCapability {
                action: "tool.web_search".into(),
                scope: None,
                expires_at: None,
            })
            .await;
        let sig_b58 = match grant {
            Response::CapabilityGranted { signature_b58, .. } => signature_b58,
            other => panic!("unexpected: {other:?}"),
        };
        // Dispatch passes after grant.
        let r = s
            .op_respond(Request::SubmitIntent {
                text: "find papers".into(),
                prefer_stream: None,
            })
            .await;
        assert!(matches!(r, Response::IntentResult { .. }));

        // Revoke; dispatch now fails.
        let revoked = s
            .op_respond(Request::RevokeCapability {
                signature_b58: sig_b58,
            })
            .await;
        match revoked {
            Response::CapabilityRevoked { removed, .. } => assert!(removed),
            other => panic!("unexpected: {other:?}"),
        }
        let r2 = s
            .op_respond(Request::SubmitIntent {
                text: "find papers".into(),
                prefer_stream: None,
            })
            .await;
        assert!(matches!(r2, Response::Error { .. }));
    }

    #[tokio::test]
    async fn submit_intent_falls_back_to_echo_when_no_match() {
        let s = server_with(vec![stub_card("research", vec!["tool.web_search"])], "");
        grant_action(&s, "memory.write").await;
        let resp = s
            .op_respond(Request::SubmitIntent {
                text: "zzz no keywords".into(),
                prefer_stream: None,
            })
            .await;
        match resp {
            Response::IntentResult { text, .. } => assert!(text.contains("no agent matched")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn submit_intent_rejects_memory_write_scope_mismatch_and_audits() {
        let s = server_with(
            vec![stub_card("research", vec!["tool.web_search"])],
            "summary",
        );
        grant_action(&s, "tool.web_search").await;
        s.op_respond(Request::GrantCapability {
            action: "memory.write".into(),
            scope: Some(serde_json::json!({
                "version": 1,
                "tiers": ["episodic"],
                "apply": true
            })),
            expires_at: None,
        })
        .await;

        let resp = s
            .op_respond(Request::SubmitIntent {
                text: "find recent papers".into(),
                prefer_stream: None,
            })
            .await;
        match resp {
            Response::Error { message } => assert!(message.contains("memory write rejected")),
            other => panic!("unexpected: {other:?}"),
        }

        let events = s.audit.recent(10).await.unwrap();
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AuditKind::CapabilityScopeRejected { action, .. } if action == "memory.write"
        )));
    }

    #[tokio::test]
    async fn grant_capability_signs_and_persists() {
        let s = server_with(vec![], "");
        let resp = s
            .op_respond(Request::GrantCapability {
                action: "tool.web_search".into(),
                scope: None,
                expires_at: None,
            })
            .await;
        let sig_b58 = match resp {
            Response::CapabilityGranted { signature_b58, .. } => signature_b58,
            other => panic!("unexpected: {other:?}"),
        };
        // Recently-granted cap should round-trip through `recent_capabilities`.
        let recent = s
            .op_respond(Request::RecentCapabilities { limit: 10 })
            .await;
        match recent {
            Response::Capabilities { capabilities } => {
                assert_eq!(capabilities.len(), 1);
                let got = bs58::encode(capabilities[0].signature).into_string();
                assert_eq!(got, sig_b58);
                assert_eq!(capabilities[0].capability.action, "tool.web_search");
                assert!(verify_with_clock(&capabilities[0], epoch_ms()).is_ok());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn grant_capability_accepts_valid_scope() {
        let s = server_with(vec![], "");
        let scope = serde_json::json!({
            "version": 1,
            "tiers": ["working"],
            "record_id": null,
            "before_ms": null,
            "apply": false
        });
        let resp = s
            .op_respond(Request::GrantCapability {
                action: "memory.write".into(),
                scope: Some(scope.clone()),
                expires_at: None,
            })
            .await;
        match resp {
            Response::CapabilityGranted { action, .. } => assert_eq!(action, "memory.write"),
            other => panic!("unexpected: {other:?}"),
        }

        match s
            .op_respond(Request::RecentCapabilities { limit: 10 })
            .await
        {
            Response::Capabilities { capabilities } => {
                assert_eq!(capabilities.len(), 1);
                assert_eq!(capabilities[0].capability.scope, scope);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn grant_capability_rejects_invalid_scope() {
        let s = server_with(vec![], "");
        let resp = s
            .op_respond(Request::GrantCapability {
                action: "memory.write".into(),
                scope: Some(serde_json::json!({ "version": 2 })),
                expires_at: None,
            })
            .await;
        match resp {
            Response::Error { message } => assert!(message.contains("invalid capability scope")),
            other => panic!("unexpected: {other:?}"),
        }

        match s
            .op_respond(Request::RecentCapabilities { limit: 10 })
            .await
        {
            Response::Capabilities { capabilities } => assert!(capabilities.is_empty()),
            other => panic!("unexpected: {other:?}"),
        }

        match s
            .op_respond(Request::RecentAudit {
                limit: 10,
                since_ms: None,
                prefer_stream: None,
            })
            .await
        {
            Response::AuditEvents { events } => assert!(events.iter().any(|event| {
                matches!(
                    &event.kind,
                    AuditKind::CapabilityGrantRejected { action, .. } if action == "memory.write"
                )
            })),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_audits_capability_check_with_missing_actions() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![stub_card(
                "research",
                vec!["tool.web_search", "memory.write"],
            )])),
            Arc::new(MockRunner::new("ok")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        grant_action(&s, "memory.write").await;
        // Dispatch will be rejected, but the capability check event is still recorded.
        s.op_respond(Request::SubmitIntent {
            text: "find papers".into(),
            prefer_stream: None,
        })
        .await;
        let events = audit.recent(10).await.unwrap();
        let cap_check = events
            .iter()
            .find(|e| {
                matches!(
                    &e.kind,
                    AuditKind::CapabilityCheck { agent_id, .. } if agent_id == "research"
                )
            })
            .expect("capability check audit event present");
        match &cap_check.kind {
            AuditKind::CapabilityCheck {
                missing_actions,
                passed,
                required_actions,
                ..
            } => {
                assert_eq!(required_actions, &vec!["tool.web_search".to_string()]);
                assert_eq!(missing_actions, &vec!["tool.web_search".to_string()]);
                assert!(!passed);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_skips_when_intent_matches_ignore_rule() {
        let ignore = IgnoreSet::parse("id_rsa\n");
        let s = server_with_ignore(vec![], "echo", ignore);
        let resp = s
            .op_respond(Request::SubmitIntent {
                text: "summarise ~/.ssh/id_rsa".into(),
                prefer_stream: None,
            })
            .await;
        match resp {
            Response::IntentResult { status, text, .. } => {
                assert_eq!(status, "ignored");
                assert!(text.contains("id_rsa"));
            }
            other => panic!("unexpected: {other:?}"),
        }
        let recents = s.memory.recent(None, 10).await.unwrap();
        assert!(recents.is_empty(), "ignored intents must not write memory");
        let recpts = s.settlement.recent(10).await.unwrap();
        assert!(
            recpts.is_empty(),
            "ignored intents must not write a receipt"
        );
    }

    #[tokio::test]
    async fn list_tools_returns_registered_tool_specs() {
        let s = server_with(vec![], "");
        let resp = s.op_respond(Request::ListTools).await;
        match resp {
            Response::ToolList { tools } => {
                let names: Vec<String> = tools.into_iter().map(|t| t.name).collect();
                assert_eq!(names, vec!["clock".to_string(), "echo".to_string()]);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn call_tool_dispatches_through_registry() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "tool.call.echo".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s
            .op_respond(Request::CallTool {
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "hi" }),
            })
            .await;
        match resp {
            Response::ToolResult { content, is_error } => {
                assert!(!is_error);
                let txt = match &content[0] {
                    covenant_mcp::Content::Text { text } => text.clone(),
                    other => panic!("unexpected: {other:?}"),
                };
                assert_eq!(txt, "hi");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Full Hyre path: capability gate → executor → 402-then-pay loop
    /// (against the live Hyre challenge shape) → budget debit +
    /// settlement receipt + audit event. The signer is a shell script
    /// standing in for the funding-key sidecar, so no real USDC moves.
    #[cfg(unix)]
    #[tokio::test]
    async fn hyre_tool_call_pays_and_records_end_to_end() {
        use std::os::unix::fs::PermissionsExt;
        use wiremock::matchers::{header_exists, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        const LIVE_402: &str = r#"{
            "error":"X-PAYMENT header is required",
            "accepts":[{"scheme":"exact","network":"solana","maxAmountRequired":"10000",
                "payTo":"7G73PLhKvAPBGTzG5ESAE4coE7QrVeTTKfhTxQZbyGgC",
                "asset":"EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                "maxTimeoutSeconds":60,"extra":{"feePayer":"2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4"}}],
            "x402Version":1
        }"#;

        let upstream = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/defi/tvl"))
            .respond_with(ResponseTemplate::new(402).set_body_string(LIVE_402))
            .up_to_n_times(1)
            .mount(&upstream)
            .await;
        Mock::given(method("GET"))
            .and(path("/defi/tvl"))
            .and(header_exists("x-payment"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "tvl": 1 }, "signal": "low_yield", "confidence": 0.9,
                "sources": ["DeFiLlama"], "latency_ms": 7, "timestamp": "2026-05-26T00:00:00Z"
            })))
            .mount(&upstream)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let signer = dir.path().join("signer.sh");
        std::fs::write(
            &signer,
            "#!/bin/sh\ncat >/dev/null\nprintf 'x402-mock-header'\n",
        )
        .unwrap();
        std::fs::set_permissions(&signer, std::fs::Permissions::from_mode(0o755)).unwrap();

        let settlement = Arc::new(InMemorySettlement::new());
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let budget = Arc::new(covenant_budget::InMemoryLedger::new());
        let identity = Arc::new(LocalIdentity::generate("user@local"));

        let cfg = covenant_hyre::HyreConfig {
            enabled: true,
            base_url: upstream.uri(),
            ..Default::default()
        };
        let catalog = covenant_hyre::HyreCatalog::from_vendored(&cfg).unwrap();

        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("")),
            Arc::new(InMemoryStore::new()),
            settlement.clone(),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            identity.clone(),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            budget.clone(),
        )
        .with_x402_dispatch(x402::X402Config {
            enabled: true,
            signer_binary: signer,
            signer_env: vec![],
        })
        .with_hyre(hyre::HyreState::new(catalog, cfg));

        let peer = identity.agent_id();
        budget.set_capacity(&peer, 1000).await.unwrap();
        s.op_respond(Request::GrantCapability {
            action: "tool.call.hyre.defi.tvl".into(),
            scope: None,
            expires_at: None,
        })
        .await;

        let resp = s
            .op_respond(Request::CallTool {
                name: "hyre.defi.tvl".into(),
                arguments: serde_json::json!({}),
            })
            .await;

        match resp {
            Response::ToolResult { content, is_error } => {
                assert!(!is_error, "expected success, got {content:?}");
                let data = content
                    .iter()
                    .find_map(|c| match c {
                        covenant_mcp::Content::Json { value } => Some(value.clone()),
                        _ => None,
                    })
                    .expect("json content");
                assert_eq!(data["data"]["tvl"], 1);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }

        let receipts = settlement.recent(10).await.unwrap();
        assert_eq!(receipts.len(), 1, "one settlement receipt");
        assert_eq!(receipts[0].resource, covenant_types::ResourceKind::Tool);
        assert_eq!(receipts[0].credits_consumed, 1, "$0.01 → 1 credit");

        let events = audit.recent(20).await.unwrap();
        let settled = events
            .iter()
            .find_map(|e| match &e.kind {
                AuditKind::ExternalPaymentSettled {
                    provider, amount, ..
                } => Some((provider.clone(), amount.clone())),
                _ => None,
            })
            .expect("ExternalPaymentSettled audit event");
        assert_eq!(settled.0, "hyre");
        assert_eq!(settled.1, "10000", "records the live atomic amount");

        assert_eq!(
            budget.tokens_remaining(&peer).await.unwrap(),
            999,
            "1 credit debited from the caller"
        );
    }

    #[tokio::test]
    async fn call_tool_accepts_matching_scope_arguments() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "tool.call.echo".into(),
            scope: Some(serde_json::json!({
                "version": 1,
                "tool": "echo",
                "arguments": { "allow": { "text": "hi" } }
            })),
            expires_at: None,
        })
        .await;
        let resp = s
            .op_respond(Request::CallTool {
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "hi" }),
            })
            .await;
        match resp {
            Response::ToolResult { content, is_error } => {
                assert!(!is_error);
                let txt = match &content[0] {
                    covenant_mcp::Content::Text { text } => text.clone(),
                    other => panic!("unexpected: {other:?}"),
                };
                assert_eq!(txt, "hi");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn call_tool_rejects_scope_argument_mismatch() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "tool.call.echo".into(),
            scope: Some(serde_json::json!({
                "version": 1,
                "tool": "echo",
                "arguments": { "allow": { "text": "hi" } }
            })),
            expires_at: None,
        })
        .await;
        let resp = s
            .op_respond(Request::CallTool {
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "bye" }),
            })
            .await;
        match resp {
            Response::Error { message } => assert!(message.contains("capability scope")),
            other => panic!("unexpected: {other:?}"),
        }

        match s
            .op_respond(Request::RecentAudit {
                limit: 10,
                since_ms: None,
                prefer_stream: None,
            })
            .await
        {
            Response::AuditEvents { events } => assert!(events.iter().any(|event| {
                matches!(
                    &event.kind,
                    AuditKind::CapabilityScopeRejected { action, .. } if action == "tool.call.echo"
                )
            })),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn call_tool_returns_error_for_unknown_name() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "tool.call.missing".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s
            .op_respond(Request::CallTool {
                name: "missing".into(),
                arguments: serde_json::Value::Null,
            })
            .await;
        match resp {
            Response::Error { message } => assert!(message.contains("not found")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn call_tool_rejects_when_capability_missing() {
        let s = server_with(vec![], "");
        let resp = s
            .op_respond(Request::CallTool {
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "hi" }),
            })
            .await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("requires capability"));
                assert!(message.contains("tool.call.echo"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn call_tool_audits_capability_check() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::from_tools(vec![Arc::new(
                covenant_mcp::native::EchoTool,
            )])),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        s.op_respond(Request::CallTool {
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "hi" }),
        })
        .await;
        let events = audit.recent(10).await.unwrap();
        let cap = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::CapabilityCheck { .. }))
            .expect("capability check audit event present");
        match &cap.kind {
            AuditKind::CapabilityCheck {
                agent_id,
                required_actions,
                passed,
                ..
            } => {
                assert_eq!(agent_id, "tool:echo");
                assert_eq!(required_actions, &vec!["tool.call.echo".to_string()]);
                assert!(!passed);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Builds a task whose `sender` matches `s.identity.agent_id()` so it
    /// passes the send-time sender-spoof check. Tests that need a
    /// mismatched sender construct the task inline.
    fn dummy_a2a_task_for(s: &Server) -> covenant_a2a::A2ATask {
        // Recv gate: loopback recipient (operator's pubkey) skips the
        // recv-side admission gate. The display stays "research@local"
        // so existing assertions keying on the recipient display (e.g.
        // `a2a.send.research@local`) still hold.
        covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: s.identity.agent_id(),
            recipient: covenant_types::AgentId::new("research@local", s.identity.pubkey_bytes()),
            intent_text: "find recent papers".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        }
    }

    fn loopback_a2a_task_for(s: &Server) -> covenant_a2a::A2ATask {
        let peer = s.identity.agent_id();
        covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: peer.clone(),
            recipient: peer,
            intent_text: "loopback".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        }
    }

    #[tokio::test]
    async fn a2a_task_round_trips_through_server() {
        let s = server_with(vec![], "");
        // `try_recv` filters by recipient, so the round-trip test queues
        // a task addressed *to* the operator peer and drains it from the
        // same peer's perspective.
        let peer = s.identity.agent_id();
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: peer.clone(),
            recipient: peer.clone(),
            intent_text: "loopback".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", task.recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;

        let queued = s
            .op_respond(Request::SendA2ATask { task: task.clone() })
            .await;
        match queued {
            Response::A2ATaskQueued { task_id } => assert_eq!(task_id, task.id),
            other => panic!("unexpected: {other:?}"),
        }
        let recv = s.op_respond(Request::TryRecvA2ATask).await;
        match recv {
            Response::A2ATaskOpt { task: Some(t) } => assert_eq!(t.id, task.id),
            other => panic!("unexpected: {other:?}"),
        }
        let again = s.op_respond(Request::TryRecvA2ATask).await;
        match again {
            Response::A2ATaskOpt { task: None } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_send_rejects_when_capability_missing() {
        let s = server_with(vec![], "");
        let task = dummy_a2a_task_for(&s);
        let resp = s.op_respond(Request::SendA2ATask { task }).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("requires capability"));
                assert!(message.contains("a2a.send.research@local"));
            }
            other => panic!("unexpected: {other:?}"),
        }
        let drained = s.op_respond(Request::TryRecvA2ATask).await;
        assert!(
            matches!(drained, Response::A2ATaskOpt { task: None }),
            "rejected task must not enqueue: {drained:?}"
        );
    }

    #[tokio::test]
    async fn a2a_send_audits_capability_check() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        s.op_respond(Request::SendA2ATask {
            task: dummy_a2a_task_for(&s),
        })
        .await;

        let events = audit.recent(10).await.unwrap();
        let cap = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::CapabilityCheck { .. }))
            .expect("capability check audit event present");
        match &cap.kind {
            AuditKind::CapabilityCheck {
                agent_id,
                required_actions,
                passed,
                ..
            } => {
                assert_eq!(agent_id, "a2a-send:research@local");
                assert_eq!(
                    required_actions,
                    &vec!["a2a.send.research@local".to_string()]
                );
                assert!(!passed);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_result_round_trips_through_server() {
        let s = server_with(vec![], "");
        let task = dummy_a2a_task_for(&s);
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", task.recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::SendA2ATask { task: task.clone() })
            .await;

        s.op_respond(Request::GrantCapability {
            action: format!("a2a.respond.{}", task.sender.display),
            scope: None,
            expires_at: None,
        })
        .await;

        let result =
            covenant_a2a::A2ATaskResult::ok(task.id, vec![covenant_mcp::Content::text("done")]);
        let posted = s
            .op_respond(Request::PostA2AResult {
                result: result.clone(),
            })
            .await;
        match posted {
            Response::A2AResultPosted { task_id: id } => assert_eq!(id, task.id),
            other => panic!("unexpected: {other:?}"),
        }
        let recv = s.op_respond(Request::TryRecvA2AResult).await;
        match recv {
            Response::A2AResultOpt {
                result: Some(got), ..
            } => {
                assert_eq!(got.task_id, task.id);
                assert_eq!(got.status, covenant_a2a::A2ATaskStatus::Ok);
            }
            other => panic!("unexpected: {other:?}"),
        }
        let again = s.op_respond(Request::TryRecvA2AResult).await;
        match again {
            Response::A2AResultOpt { result: None } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_recent_returns_queued_tasks_without_consuming() {
        let s = server_with(vec![], "");
        // Per-peer recv requires the queued tasks to be addressed to
        // the peer doing the drain. Loopback fits the v0 single-peer
        // test surface.
        let peer = s.identity.agent_id();
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: peer.clone(),
            recipient: peer.clone(),
            intent_text: "loopback".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", task.recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::SendA2ATask { task: task.clone() })
            .await;
        s.op_respond(Request::SendA2ATask {
            task: covenant_a2a::A2ATask {
                id: Uuid::new_v4(),
                ..task.clone()
            },
        })
        .await;

        let recent = s.op_respond(Request::RecentA2ATasks { limit: 10 }).await;
        match recent {
            Response::A2ATasks { tasks } => {
                assert_eq!(tasks.len(), 2);
                assert_eq!(tasks[0].id, task.id, "oldest first");
            }
            other => panic!("unexpected: {other:?}"),
        }

        // recent must not consume — try_recv still finds tasks.
        let drained = s.op_respond(Request::TryRecvA2ATask).await;
        assert!(matches!(drained, Response::A2ATaskOpt { task: Some(_) }));
    }

    #[tokio::test]
    async fn a2a_queue_surfaces_in_flight_tasks() {
        let s = server_with(vec![], "");
        let peer = s.identity.agent_id();
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: peer.clone(),
            recipient: peer.clone(),
            intent_text: "loopback".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", task.recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::SendA2ATask { task: task.clone() })
            .await;

        let drained = s.op_respond(Request::TryRecvA2ATask).await;
        assert!(matches!(drained, Response::A2ATaskOpt { task: Some(_) }));

        let queue = s
            .op_respond(Request::A2AQueue {
                limit: 10,
                min_lease_age_ms: None,
                deadline_within_ms: None,
                state_filter: None,
            })
            .await;
        match queue {
            Response::A2AQueue { tasks, results } => {
                assert!(results.is_empty());
                assert_eq!(tasks.len(), 1);
                assert_eq!(tasks[0].state, covenant_a2a::A2ATaskQueueState::InFlight);
                assert_eq!(tasks[0].task.id, task.id);
                assert_eq!(tasks[0].leased_to.as_ref(), Some(&peer));
            }
            other => panic!("unexpected: {other:?}"),
        }

        let queue = s
            .op_respond(Request::A2AQueue {
                limit: 10,
                min_lease_age_ms: Some(0),
                deadline_within_ms: None,
                state_filter: None,
            })
            .await;
        match queue {
            Response::A2AQueue { tasks, results } => {
                assert!(results.is_empty());
                assert_eq!(tasks.len(), 1);
                assert_eq!(tasks[0].task.id, task.id);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_queue_min_lease_age_filters_only_in_flight_tasks() {
        let s = server_with(vec![], "");
        let in_flight = loopback_a2a_task_for(&s);
        let result_task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            ..in_flight.clone()
        };
        let queued = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            ..in_flight.clone()
        };
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", in_flight.recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.respond.{}", in_flight.sender.display),
            scope: None,
            expires_at: None,
        })
        .await;

        s.op_respond(Request::SendA2ATask {
            task: in_flight.clone(),
        })
        .await;
        let _ = s.op_respond(Request::TryRecvA2ATask).await;

        s.op_respond(Request::SendA2ATask {
            task: result_task.clone(),
        })
        .await;
        let _ = s.op_respond(Request::TryRecvA2ATask).await;
        let result = covenant_a2a::A2ATaskResult::ok(
            result_task.id,
            vec![covenant_mcp::Content::text("done")],
        );
        s.op_respond(Request::PostA2AResult { result }).await;

        s.op_respond(Request::SendA2ATask {
            task: queued.clone(),
        })
        .await;

        let queue = s
            .op_respond(Request::A2AQueue {
                limit: 10,
                min_lease_age_ms: Some(u64::MAX),
                deadline_within_ms: None,
                state_filter: None,
            })
            .await;
        match queue {
            Response::A2AQueue { tasks, results } => {
                assert_eq!(tasks.len(), 1);
                assert_eq!(tasks[0].state, covenant_a2a::A2ATaskQueueState::Queued);
                assert_eq!(tasks[0].task.id, queued.id);
                assert_eq!(results.len(), 1);
                assert_eq!(results[0].task_id, result_task.id);
            }
            other => panic!("unexpected: {other:?}"),
        }

        let queued_only = s
            .op_respond(Request::A2AQueue {
                limit: 10,
                min_lease_age_ms: None,
                deadline_within_ms: None,
                state_filter: Some(covenant_a2a::A2ATaskQueueState::Queued),
            })
            .await;
        match queued_only {
            Response::A2AQueue { tasks, .. } => {
                assert_eq!(tasks.len(), 1);
                assert_eq!(tasks[0].state, covenant_a2a::A2ATaskQueueState::Queued);
                assert_eq!(tasks[0].task.id, queued.id);
            }
            other => panic!("unexpected: {other:?}"),
        }

        let in_flight_only = s
            .op_respond(Request::A2AQueue {
                limit: 10,
                min_lease_age_ms: None,
                deadline_within_ms: None,
                state_filter: Some(covenant_a2a::A2ATaskQueueState::InFlight),
            })
            .await;
        match in_flight_only {
            Response::A2AQueue { tasks, .. } => {
                assert_eq!(tasks.len(), 1);
                assert_eq!(tasks[0].state, covenant_a2a::A2ATaskQueueState::InFlight);
                assert_eq!(tasks[0].task.id, in_flight.id);
            }
            other => panic!("unexpected: {other:?}"),
        }

        let narrow_limit = s
            .op_respond(Request::A2AQueue {
                limit: 1,
                min_lease_age_ms: None,
                deadline_within_ms: None,
                state_filter: Some(covenant_a2a::A2ATaskQueueState::InFlight),
            })
            .await;
        match narrow_limit {
            Response::A2AQueue { tasks, .. } => {
                assert_eq!(
                    tasks.len(),
                    1,
                    "state_filter must be applied before --limit so a queued cluster cannot push in_flight rows out of the result window",
                );
                assert_eq!(tasks[0].state, covenant_a2a::A2ATaskQueueState::InFlight);
                assert_eq!(tasks[0].task.id, in_flight.id);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_queue_scrubs_unrelated_in_flight_tasks() {
        let s = server_with(vec![], "");
        let alien_sender = AgentId::new("alice@local", [9u8; 32]);
        let alien_recipient = AgentId::new("bob@local", [8u8; 32]);
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: alien_sender,
            recipient: alien_recipient.clone(),
            intent_text: "alien".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        s.mailbox.send_task(task).await.unwrap();
        assert!(s
            .mailbox
            .try_recv_task_for(&alien_recipient)
            .await
            .unwrap()
            .is_some());

        let queue = s
            .op_respond(Request::A2AQueue {
                limit: 10,
                min_lease_age_ms: None,
                deadline_within_ms: None,
                state_filter: None,
            })
            .await;
        match queue {
            Response::A2AQueue { tasks, results } => {
                assert!(tasks.is_empty());
                assert!(results.is_empty());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_repair_rejects_without_capability() {
        let s = server_with(vec![], "");
        let task = loopback_a2a_task_for(&s);
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", task.recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::SendA2ATask { task: task.clone() })
            .await;
        let _ = s.op_respond(Request::TryRecvA2ATask).await;

        let resp = s
            .op_respond(Request::RepairA2ATask {
                request: covenant_a2a::A2ARepairRequest {
                    task_id: task.id,
                    command: covenant_a2a::A2ARepairCommand::Requeue {
                        lease_id: None,
                        duplicate_risk: covenant_a2a::A2ADuplicateRisk::Idempotent,
                    },
                    reason: "worker crashed".into(),
                },
            })
            .await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("a2a.repair.requeue"));
                assert!(message.contains("requires capability"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_repair_rejects_scope_lease_mismatch_and_audits() {
        let s = server_with(vec![], "");
        let task = loopback_a2a_task_for(&s);
        grant_action(&s, &format!("a2a.send.{}", task.recipient.display)).await;
        s.op_respond(Request::SendA2ATask { task: task.clone() })
            .await;
        let _ = s.op_respond(Request::TryRecvA2ATask).await;

        let lease_id = match s
            .op_respond(Request::A2AQueue {
                limit: 10,
                min_lease_age_ms: None,
                deadline_within_ms: None,
                state_filter: None,
            })
            .await
        {
            Response::A2AQueue { tasks, .. } => tasks[0].lease_id,
            other => panic!("unexpected: {other:?}"),
        };
        let wrong_lease = Uuid::new_v4();
        assert_ne!(lease_id, Some(wrong_lease));
        let action = "a2a.repair.requeue";
        grant_scoped_action(
            &s,
            action,
            serde_json::json!({
                "version": 1,
                "peer_pubkey_b58": task.recipient.pubkey_base58(),
                "task_id": task.id.to_string(),
                "lease_id": wrong_lease.to_string(),
                "duplicate_risk": "idempotent"
            }),
        )
        .await;

        let resp = s
            .op_respond(Request::RepairA2ATask {
                request: covenant_a2a::A2ARepairRequest {
                    task_id: task.id,
                    command: covenant_a2a::A2ARepairCommand::Requeue {
                        lease_id,
                        duplicate_risk: covenant_a2a::A2ADuplicateRisk::Idempotent,
                    },
                    reason: "worker crashed".into(),
                },
            })
            .await;
        match resp {
            Response::Error { message } => assert!(message.contains("capability scope")),
            other => panic!("expected Error, got {other:?}"),
        }

        match s
            .op_respond(Request::RecentAudit {
                limit: 30,
                since_ms: None,
                prefer_stream: None,
            })
            .await
        {
            Response::AuditEvents { events } => assert!(events.iter().any(|event| {
                matches!(
                    &event.kind,
                    AuditKind::CapabilityScopeRejected { action: got, .. } if got == action
                )
            })),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_repair_rejects_peer_mismatched_delegated_scope() {
        let s = server_with(vec![], "");
        let operator = s.identity.agent_id();
        let delegate = AgentId::new("delegate@local", [7u8; 32]);
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: operator.clone(),
            recipient: delegate.clone(),
            intent_text: "delegated repair visibility".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };

        grant_action(&s, &format!("a2a.send.{}", delegate.display)).await;
        grant_scoped_action_to(
            &s,
            &delegate,
            &format!("a2a.recv.{}", operator.display),
            serde_json::json!({}),
        )
        .await;

        match s
            .op_respond(Request::SendA2ATask { task: task.clone() })
            .await
        {
            Response::A2ATaskQueued { task_id } => assert_eq!(task_id, task.id),
            other => panic!("send failed: {other:?}"),
        }
        match s.respond(Request::TryRecvA2ATask, &delegate).await {
            Response::A2ATaskOpt { task: Some(got) } => assert_eq!(got.id, task.id),
            other => panic!("delegate lease failed: {other:?}"),
        }
        let lease_id = match s
            .respond(
                Request::A2AQueue {
                    limit: 10,
                    min_lease_age_ms: None,
                    deadline_within_ms: None,
                    state_filter: None,
                },
                &delegate,
            )
            .await
        {
            Response::A2AQueue { tasks, .. } => tasks[0].lease_id,
            other => panic!("unexpected: {other:?}"),
        };

        let action = "a2a.repair.requeue";
        grant_scoped_action_to(
            &s,
            &delegate,
            action,
            serde_json::json!({
                "version": 1,
                "peer_pubkey_b58": delegate.pubkey_base58(),
                "task_id": task.id.to_string(),
                "lease_id": lease_id.expect("leased task").to_string(),
                "duplicate_risk": "idempotent"
            }),
        )
        .await;

        let resp = s
            .respond(
                Request::RepairA2ATask {
                    request: covenant_a2a::A2ARepairRequest {
                        task_id: task.id,
                        command: covenant_a2a::A2ARepairCommand::Requeue {
                            lease_id,
                            duplicate_risk: covenant_a2a::A2ADuplicateRisk::Idempotent,
                        },
                        reason: "delegate retry probe".into(),
                    },
                },
                &delegate,
            )
            .await;
        match resp {
            Response::Error { message } => assert!(message.contains("capability scope")),
            other => panic!("expected Error, got {other:?}"),
        }

        match s
            .respond(
                Request::A2AQueue {
                    limit: 10,
                    min_lease_age_ms: None,
                    deadline_within_ms: None,
                    state_filter: None,
                },
                &delegate,
            )
            .await
        {
            Response::A2AQueue { tasks, .. } => {
                assert_eq!(tasks[0].task.id, task.id);
                assert_eq!(tasks[0].state, covenant_a2a::A2ATaskQueueState::InFlight);
            }
            other => panic!("unexpected: {other:?}"),
        }

        match s
            .respond(
                Request::RecentAudit {
                    limit: 30,
                    since_ms: None,
                    prefer_stream: None,
                },
                &delegate,
            )
            .await
        {
            Response::AuditEvents { events } => assert!(events.iter().any(|event| {
                matches!(
                    &event.kind,
                    AuditKind::CapabilityScopeRejected { action: got, .. } if got == action
                )
            })),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_repair_requeues_in_flight_task_and_audits() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        let task = loopback_a2a_task_for(&s);
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", task.recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::GrantCapability {
            action: "a2a.repair.requeue".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::SendA2ATask { task: task.clone() })
            .await;
        let _ = s.op_respond(Request::TryRecvA2ATask).await;
        let lease_id = match s
            .op_respond(Request::A2AQueue {
                limit: 10,
                min_lease_age_ms: None,
                deadline_within_ms: None,
                state_filter: None,
            })
            .await
        {
            Response::A2AQueue { tasks, .. } => tasks[0].lease_id,
            other => panic!("unexpected: {other:?}"),
        };

        let reason = "worker heartbeat expired";
        let repaired = s
            .op_respond(Request::RepairA2ATask {
                request: covenant_a2a::A2ARepairRequest {
                    task_id: task.id,
                    command: covenant_a2a::A2ARepairCommand::Requeue {
                        lease_id,
                        duplicate_risk: covenant_a2a::A2ADuplicateRisk::Idempotent,
                    },
                    reason: reason.into(),
                },
            })
            .await;
        match repaired {
            Response::A2ARepaired { outcome } => {
                assert_eq!(outcome.action, covenant_a2a::A2ARepairAction::Requeued);
                assert_eq!(outcome.attempt, 1);
            }
            other => panic!("unexpected: {other:?}"),
        }
        match s
            .op_respond(Request::A2AQueue {
                limit: 10,
                min_lease_age_ms: None,
                deadline_within_ms: None,
                state_filter: None,
            })
            .await
        {
            Response::A2AQueue { tasks, .. } => {
                assert_eq!(tasks[0].state, covenant_a2a::A2ATaskQueueState::Queued);
                assert_eq!(tasks[0].attempt, 1);
            }
            other => panic!("unexpected: {other:?}"),
        }

        let events = audit.recent(20).await.unwrap();
        let repair = events
            .iter()
            .find(|event| matches!(event.kind, AuditKind::A2ARepairApplied { .. }))
            .expect("repair audit row");
        match &repair.kind {
            AuditKind::A2ARepairApplied {
                task_id,
                action,
                reason: logged_reason,
                lease_id: logged_lease,
                duplicate_risk,
                attempt,
            } => {
                assert_eq!(*task_id, task.id);
                assert_eq!(action, "requeue");
                assert_eq!(logged_reason, reason);
                assert_eq!(*logged_lease, lease_id);
                assert_eq!(duplicate_risk.as_deref(), Some("idempotent"));
                assert_eq!(*attempt, 1);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_auto_retry_defaults_to_disabled_without_mutating() {
        let s = server_with(vec![], "");
        let mut task = loopback_a2a_task_for(&s);
        task.idempotency = Some(covenant_a2a::A2AIdempotency::new(
            covenant_a2a::A2ADuplicateSafety::Idempotent,
            "safe-task",
        ));
        grant_action(&s, &format!("a2a.send.{}", task.recipient.display)).await;
        s.op_respond(Request::SendA2ATask { task: task.clone() })
            .await;
        let _ = s.op_respond(Request::TryRecvA2ATask).await;

        match s
            .op_respond(Request::RetryA2AStale {
                policy: covenant_a2a::A2AAutoRetryPolicy::default(),
            })
            .await
        {
            Response::A2AAutoRetried { report } => {
                assert!(!report.policy.enabled);
                assert_eq!(report.considered, 1);
                assert!(report.requeued.is_empty());
                assert_eq!(
                    report.skipped[0].reason,
                    covenant_a2a::A2AAutoRetrySkipReason::Disabled
                );
            }
            other => panic!("unexpected: {other:?}"),
        }

        match s
            .op_respond(Request::A2AQueue {
                limit: 10,
                min_lease_age_ms: None,
                deadline_within_ms: None,
                state_filter: None,
            })
            .await
        {
            Response::A2AQueue { tasks, .. } => {
                assert_eq!(tasks[0].state, covenant_a2a::A2ATaskQueueState::InFlight);
                assert_eq!(tasks[0].task.id, task.id);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_auto_retry_requeues_only_safe_idempotent_tasks_and_audits() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = server_with_audit(audit.clone());
        let mut safe = loopback_a2a_task_for(&s);
        safe.idempotency = Some(covenant_a2a::A2AIdempotency::new(
            covenant_a2a::A2ADuplicateSafety::Idempotent,
            "safe-task",
        ));
        let mut unsafe_task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            ..safe.clone()
        };
        unsafe_task.idempotency = Some(covenant_a2a::A2AIdempotency::new(
            covenant_a2a::A2ADuplicateSafety::Unsafe,
            "unsafe-task",
        ));

        grant_action(&s, &format!("a2a.send.{}", safe.recipient.display)).await;
        grant_action(&s, "a2a.repair.requeue").await;
        s.op_respond(Request::SendA2ATask { task: safe.clone() })
            .await;
        s.op_respond(Request::SendA2ATask {
            task: unsafe_task.clone(),
        })
        .await;
        let _ = s.op_respond(Request::TryRecvA2ATask).await;
        let _ = s.op_respond(Request::TryRecvA2ATask).await;

        let report = match s
            .op_respond(Request::RetryA2AStale {
                policy: covenant_a2a::A2AAutoRetryPolicy {
                    enabled: true,
                    min_lease_age_ms: 0,
                    max_attempts: 3,
                    max_requeues: 10,
                    scan_limit: 10,
                },
            })
            .await
        {
            Response::A2AAutoRetried { report } => report,
            other => panic!("unexpected: {other:?}"),
        };
        assert_eq!(report.considered, 2);
        assert_eq!(report.requeued.len(), 1);
        assert_eq!(report.requeued[0].task_id, safe.id);
        assert_eq!(report.requeued[0].idempotency_key, "safe-task");
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].task_id, unsafe_task.id);
        assert_eq!(
            report.skipped[0].reason,
            covenant_a2a::A2AAutoRetrySkipReason::UnsafeDuplicateSafety
        );

        match s
            .op_respond(Request::A2AQueue {
                limit: 10,
                min_lease_age_ms: None,
                deadline_within_ms: None,
                state_filter: None,
            })
            .await
        {
            Response::A2AQueue { tasks, .. } => {
                let safe_entry = tasks
                    .iter()
                    .find(|entry| entry.task.id == safe.id)
                    .expect("safe task entry");
                assert_eq!(safe_entry.state, covenant_a2a::A2ATaskQueueState::Queued);
                let unsafe_entry = tasks
                    .iter()
                    .find(|entry| entry.task.id == unsafe_task.id)
                    .expect("unsafe task entry");
                assert_eq!(
                    unsafe_entry.state,
                    covenant_a2a::A2ATaskQueueState::InFlight
                );
            }
            other => panic!("unexpected: {other:?}"),
        }

        let events = audit.recent(20).await.unwrap();
        let repair = events
            .iter()
            .find(|event| {
                matches!(
                    &event.kind,
                    AuditKind::A2ARepairApplied { action, .. } if action == "auto_requeue"
                )
            })
            .expect("auto retry audit row");
        match &repair.kind {
            AuditKind::A2ARepairApplied {
                task_id,
                duplicate_risk,
                ..
            } => {
                assert_eq!(*task_id, safe.id);
                assert_eq!(duplicate_risk.as_deref(), Some("idempotent"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_auto_retry_scheduler_once_reuses_gate_and_audits_summary() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = server_with_audit(audit.clone());
        let mut task = loopback_a2a_task_for(&s);
        task.idempotency = Some(covenant_a2a::A2AIdempotency::new(
            covenant_a2a::A2ADuplicateSafety::Idempotent,
            "scheduler-safe-task",
        ));

        grant_action(&s, &format!("a2a.send.{}", task.recipient.display)).await;
        grant_action(&s, "a2a.repair.requeue").await;
        s.op_respond(Request::SendA2ATask { task: task.clone() })
            .await;
        let _ = s.op_respond(Request::TryRecvA2ATask).await;

        let response = s
            .run_a2a_auto_retry_scheduler_once(covenant_a2a::A2AAutoRetryPolicy {
                enabled: true,
                min_lease_age_ms: 0,
                max_attempts: 3,
                max_requeues: 1,
                scan_limit: 10,
            })
            .await;

        match response {
            Response::A2AAutoRetried { report } => {
                assert_eq!(report.considered, 1);
                assert_eq!(report.requeued.len(), 1);
                assert!(report.skipped.is_empty());
                assert_eq!(report.requeued[0].task_id, task.id);
            }
            other => panic!("unexpected: {other:?}"),
        }

        let events = audit.recent(20).await.unwrap();
        assert!(events.iter().any(|event| {
            matches!(
                &event.kind,
                AuditKind::A2ARepairApplied { action, .. } if action == "auto_requeue"
            )
        }));
        let scan = events
            .iter()
            .find(|event| matches!(event.kind, AuditKind::A2AAutoRetrySchedulerScan { .. }))
            .expect("scheduler scan audit row");
        match &scan.kind {
            AuditKind::A2AAutoRetrySchedulerScan {
                enabled,
                considered,
                requeued,
                skipped,
                skipped_by_reason,
                error,
                ..
            } => {
                assert!(*enabled);
                assert_eq!(*considered, 1);
                assert_eq!(*requeued, 1);
                assert_eq!(*skipped, 0);
                assert!(skipped_by_reason.is_empty());
                assert_eq!(error, &None);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_auto_retry_scheduler_once_audits_missing_capability_error() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = server_with_audit(audit.clone());
        let mut task = loopback_a2a_task_for(&s);
        task.idempotency = Some(covenant_a2a::A2AIdempotency::new(
            covenant_a2a::A2ADuplicateSafety::Idempotent,
            "scheduler-safe-task",
        ));

        grant_action(&s, &format!("a2a.send.{}", task.recipient.display)).await;
        s.op_respond(Request::SendA2ATask { task: task.clone() })
            .await;
        let _ = s.op_respond(Request::TryRecvA2ATask).await;

        let response = s
            .run_a2a_auto_retry_scheduler_once(covenant_a2a::A2AAutoRetryPolicy {
                enabled: true,
                min_lease_age_ms: 0,
                max_attempts: 3,
                max_requeues: 1,
                scan_limit: 10,
            })
            .await;

        match response {
            Response::Error { message } => {
                assert!(message.contains("a2a.repair.requeue"), "{message}");
            }
            other => panic!("unexpected: {other:?}"),
        }

        let events = audit.recent(20).await.unwrap();
        let scan = events
            .iter()
            .find(|event| matches!(event.kind, AuditKind::A2AAutoRetrySchedulerScan { .. }))
            .expect("scheduler scan audit row");
        match &scan.kind {
            AuditKind::A2AAutoRetrySchedulerScan {
                enabled,
                considered,
                requeued,
                skipped,
                error,
                ..
            } => {
                assert!(*enabled);
                assert_eq!(*considered, 0);
                assert_eq!(*requeued, 0);
                assert_eq!(*skipped, 0);
                assert!(
                    error
                        .as_deref()
                        .is_some_and(|message| message.contains("a2a.repair.requeue")),
                    "{error:?}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }

        match s
            .op_respond(Request::A2AQueue {
                limit: 10,
                min_lease_age_ms: None,
                deadline_within_ms: None,
                state_filter: None,
            })
            .await
        {
            Response::A2AQueue { tasks, .. } => {
                assert_eq!(tasks[0].state, covenant_a2a::A2ATaskQueueState::InFlight);
                assert_eq!(tasks[0].task.id, task.id);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_repair_rejects_stale_lease_guard() {
        let s = server_with(vec![], "");
        let task = loopback_a2a_task_for(&s);
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", task.recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::GrantCapability {
            action: "a2a.repair.requeue".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::SendA2ATask { task: task.clone() })
            .await;
        let _ = s.op_respond(Request::TryRecvA2ATask).await;

        let resp = s
            .op_respond(Request::RepairA2ATask {
                request: covenant_a2a::A2ARepairRequest {
                    task_id: task.id,
                    command: covenant_a2a::A2ARepairCommand::Requeue {
                        lease_id: Some(Uuid::new_v4()),
                        duplicate_risk: covenant_a2a::A2ADuplicateRisk::OperatorAccepted,
                    },
                    reason: "operator accepted duplicate risk".into(),
                },
            })
            .await;
        match resp {
            Response::Error { message } => assert!(message.contains("lease mismatch")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_repair_force_error_posts_sender_result() {
        let s = server_with(vec![], "");
        let task = loopback_a2a_task_for(&s);
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", task.recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::GrantCapability {
            action: "a2a.repair.force_error".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::SendA2ATask { task: task.clone() })
            .await;
        let _ = s.op_respond(Request::TryRecvA2ATask).await;
        let lease_id = match s
            .op_respond(Request::A2AQueue {
                limit: 10,
                min_lease_age_ms: None,
                deadline_within_ms: None,
                state_filter: None,
            })
            .await
        {
            Response::A2AQueue { tasks, .. } => tasks[0].lease_id,
            other => panic!("unexpected: {other:?}"),
        };

        let repaired = s
            .op_respond(Request::RepairA2ATask {
                request: covenant_a2a::A2ARepairRequest {
                    task_id: task.id,
                    command: covenant_a2a::A2ARepairCommand::ForceError {
                        lease_id,
                        message: "operator forced stale lease failure".into(),
                    },
                    reason: "recipient process exited".into(),
                },
            })
            .await;
        match repaired {
            Response::A2ARepaired { outcome } => {
                assert_eq!(outcome.action, covenant_a2a::A2ARepairAction::ForcedError);
                assert_eq!(outcome.state, covenant_a2a::A2ARepairState::ResultPending);
                assert!(outcome.result.is_some());
            }
            other => panic!("unexpected: {other:?}"),
        }

        let recv = s.op_respond(Request::TryRecvA2AResult).await;
        match recv {
            Response::A2AResultOpt {
                result: Some(result),
            } => {
                assert_eq!(result.task_id, task.id);
                assert_eq!(result.status, covenant_a2a::A2ATaskStatus::Error);
                assert_eq!(
                    result.error_message.as_deref(),
                    Some("operator forced stale lease failure")
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_respond_rejects_when_task_id_unknown() {
        let s = server_with(vec![], "");
        // No task has been sent; any task_id is unknown to the mailbox.
        let unknown_id = Uuid::new_v4();
        let result =
            covenant_a2a::A2ATaskResult::ok(unknown_id, vec![covenant_mcp::Content::text("done")]);
        let resp = s.op_respond(Request::PostA2AResult { result }).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("never dispatched"));
            }
            other => panic!("unexpected: {other:?}"),
        }
        let drained = s.op_respond(Request::TryRecvA2AResult).await;
        assert!(
            matches!(drained, Response::A2AResultOpt { result: None }),
            "rejected result must not enqueue: {drained:?}"
        );

        // Defender-visible: the rejection lands in the audit log even
        // though no capability check happened upstream of the lookup.
        match s
            .op_respond(Request::RecentAudit {
                limit: 10,
                since_ms: None,
                prefer_stream: None,
            })
            .await
        {
            Response::AuditEvents { events } => {
                let logged = events.iter().find_map(|e| match &e.kind {
                    AuditKind::A2AResultRejected { task_id, reason } => {
                        Some((*task_id, reason.clone()))
                    }
                    _ => None,
                });
                let (task_id, reason) = logged.expect("expected an A2AResultRejected audit event");
                assert_eq!(task_id, unknown_id);
                assert_eq!(reason, "unknown_task");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_respond_rejects_when_sender_capability_missing() {
        let s = server_with(vec![], "");
        let task = dummy_a2a_task_for(&s);
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", task.recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::SendA2ATask { task: task.clone() })
            .await;

        // Task is now known; respond cap is still missing.
        let result =
            covenant_a2a::A2ATaskResult::ok(task.id, vec![covenant_mcp::Content::text("done")]);
        let resp = s.op_respond(Request::PostA2AResult { result }).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("requires capability"));
                assert!(message.contains(&format!("a2a.respond.{}", task.sender.display)));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_send_rejects_when_sender_does_not_match_peer() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        // Authenticated peer is `user@local`, but the task claims to be from
        // `evil@local`. Even with the send cap granted, the spoof check fires
        // first and the task never reaches the mailbox.
        s.op_respond(Request::GrantCapability {
            action: "a2a.send.research@local".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: covenant_types::AgentId::new("evil@local", [9u8; 32]),
            recipient: covenant_types::AgentId::new("research@local", [0u8; 32]),
            intent_text: "stolen identity".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        let resp = s.op_respond(Request::SendA2ATask { task }).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("does not match"));
                assert!(message.contains("evil@local"));
                assert!(message.contains("user@local"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
        let drained = s.op_respond(Request::TryRecvA2ATask).await;
        assert!(
            matches!(drained, Response::A2ATaskOpt { task: None }),
            "spoofed task must not enqueue: {drained:?}"
        );
        let events = audit.recent(20).await.unwrap();
        let mismatch = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::A2ASenderMismatch { .. }))
            .expect("expected an A2ASenderMismatch audit event");
        match &mismatch.kind {
            AuditKind::A2ASenderMismatch {
                peer_display,
                claimed_sender_display,
            } => {
                assert_eq!(peer_display, "user@local");
                assert_eq!(claimed_sender_display, "evil@local");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    // Recipient admission gate tests.

    #[tokio::test]
    async fn recv_gate_skipped_when_peer_equals_recipient_loopback() {
        let s = server_with(vec![], "");
        let peer = s.identity.agent_id();
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: peer.clone(),
            recipient: peer.clone(),
            intent_text: "loopback".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", task.recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s
            .op_respond(Request::SendA2ATask { task: task.clone() })
            .await;
        match resp {
            Response::A2ATaskQueued { task_id } => assert_eq!(task_id, task.id),
            other => panic!("v0 loopback must skip recv gate, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn recv_gate_rejects_when_recipient_lacks_cap() {
        let s = server_with(vec![], "");
        let peer = s.identity.agent_id();
        let foreign_recipient = AgentId::new("victim@local", [7u8; 32]);
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: peer.clone(),
            recipient: foreign_recipient.clone(),
            intent_text: "spam".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", foreign_recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s.op_respond(Request::SendA2ATask { task }).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("recipient has not granted"));
                assert!(message.contains(&format!("a2a.recv.{}", peer.display)));
            }
            other => panic!("expected Error, got {other:?}"),
        }
        let drained = s.op_respond(Request::TryRecvA2ATask).await;
        assert!(
            matches!(drained, Response::A2ATaskOpt { task: None }),
            "rejected task must not enqueue: {drained:?}"
        );
    }

    #[tokio::test]
    async fn recv_gate_passes_when_recipient_has_cap() {
        let s = server_with(vec![], "");
        let peer = s.identity.agent_id();
        let foreign_recipient = AgentId::new("victim@local", [7u8; 32]);
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", foreign_recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        // Inject the recv cap directly with subject = recipient.pubkey.
        // v0 has no IPC verb to grant on a foreign subject; tests bypass
        // via the store API to exercise the gate's pass path. Sign with
        // the daemon's identity so the cap passes the trust-root check
        // every dispatch-time verify now performs.
        let recv_cap = covenant_types::Capability {
            subject: foreign_recipient.clone(),
            action: format!("a2a.recv.{}", peer.display),
            scope: serde_json::json!({}),
            granted_by: peer.clone(),
            expires_at: None,
        };
        let signed = sign_capability(recv_cap, s.identity.signing_key());
        s.capabilities.record(signed).await.unwrap();

        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: peer.clone(),
            recipient: foreign_recipient,
            intent_text: "authorised".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        let resp = s
            .op_respond(Request::SendA2ATask { task: task.clone() })
            .await;
        match resp {
            Response::A2ATaskQueued { task_id } => assert_eq!(task_id, task.id),
            other => panic!("recv gate must pass with cap granted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn recv_gate_audits_recipient_rejected_with_attribution() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        let peer = s.identity.agent_id();
        let foreign_recipient = AgentId::new("victim@local", [7u8; 32]);
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", foreign_recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: peer.clone(),
            recipient: foreign_recipient.clone(),
            intent_text: "spam".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        s.op_respond(Request::SendA2ATask { task }).await;

        let events = audit.recent(20).await.unwrap();
        let row = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::A2ARecipientRejected { .. }))
            .expect("expected an A2ARecipientRejected audit event");
        assert_eq!(
            row.issuer.pubkey, peer.pubkey,
            "issuer must be the sender peer (record_peer_event invariant)"
        );
        match &row.kind {
            AuditKind::A2ARecipientRejected {
                sender_display,
                recipient_display,
                action,
            } => {
                assert_eq!(sender_display, &peer.display);
                assert_eq!(recipient_display, &foreign_recipient.display);
                assert_eq!(action, &format!("a2a.recv.{}", peer.display));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recv_gate_does_not_short_circuit_send_cap_check() {
        let s = server_with(vec![], "");
        let peer = s.identity.agent_id();
        let foreign_recipient = AgentId::new("victim@local", [7u8; 32]);
        // No `a2a.send.<recipient>` granted. The send-cap check fires
        // first and short-circuits before the recv gate runs.
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: peer.clone(),
            recipient: foreign_recipient.clone(),
            intent_text: "no send cap".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        let resp = s.op_respond(Request::SendA2ATask { task }).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains(&format!("a2a.send.{}", foreign_recipient.display)));
                assert!(
                    !message.contains("recipient has not granted"),
                    "send-cap check must short-circuit before recv gate: {message}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn recv_gate_keys_on_pubkey_not_display() {
        let s = server_with(vec![], "");
        let peer = s.identity.agent_id();
        // Recipient with the operator's *display* but a different
        // pubkey: the gate must still trip because subject keying is on
        // the 32-byte pubkey.
        let spoofed_recipient = AgentId::new(peer.display.clone(), [9u8; 32]);
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", spoofed_recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: peer.clone(),
            recipient: spoofed_recipient,
            intent_text: "display spoof".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        let resp = s.op_respond(Request::SendA2ATask { task }).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("recipient has not granted"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // Accept-both-shapes: a2a.{send,recv,respond} caps are satisfied by
    // either the peer's display form or its pubkey-b58 form. Display
    // remains the v0 default; b58 is the unforgeable form that closes
    // the display-collision failure mode going into Phase-1 multi-peer.

    #[tokio::test]
    async fn a2a_send_accepts_pubkey_b58_grant() {
        let s = server_with(vec![], "");
        let task = dummy_a2a_task_for(&s);
        // Grant the b58 form against the recipient's pubkey rather than
        // the display "research@local". The check must accept it.
        let alternatives = task.recipient.scoped_action_alternatives("a2a.send");
        s.op_respond(Request::GrantCapability {
            action: alternatives[1].clone(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s
            .op_respond(Request::SendA2ATask { task: task.clone() })
            .await;
        match resp {
            Response::A2ATaskQueued { task_id } => assert_eq!(task_id, task.id),
            other => panic!("b58 grant must satisfy send-cap, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_send_audit_records_matched_b58_form() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        let task = dummy_a2a_task_for(&s);
        let alternatives = task.recipient.scoped_action_alternatives("a2a.send");
        let b58_form = alternatives[1].clone();
        s.op_respond(Request::GrantCapability {
            action: b58_form.clone(),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::SendA2ATask { task }).await;

        let events = audit.recent(20).await.unwrap();
        let cap = events
            .iter()
            .filter(|e| matches!(e.kind, AuditKind::CapabilityCheck { .. }))
            .find(|e| match &e.kind {
                AuditKind::CapabilityCheck { agent_id, .. } => agent_id.starts_with("a2a-send:"),
                _ => false,
            })
            .expect("expected a CapabilityCheck for the send call");
        match &cap.kind {
            AuditKind::CapabilityCheck {
                required_actions,
                missing_actions,
                passed,
                ..
            } => {
                assert_eq!(required_actions, &vec![b58_form.clone()]);
                assert!(missing_actions.is_empty());
                assert!(*passed);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_send_audit_records_display_on_miss() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        // No grant — both forms miss. C1 records the canonical (display)
        // form so operator-facing renderers stay display-form on failures.
        s.op_respond(Request::SendA2ATask {
            task: dummy_a2a_task_for(&s),
        })
        .await;

        let events = audit.recent(20).await.unwrap();
        let cap = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::CapabilityCheck { .. }))
            .expect("capability check audit event present");
        match &cap.kind {
            AuditKind::CapabilityCheck {
                required_actions,
                missing_actions,
                passed,
                ..
            } => {
                assert_eq!(
                    required_actions,
                    &vec!["a2a.send.research@local".to_string()]
                );
                assert_eq!(
                    missing_actions,
                    &vec!["a2a.send.research@local".to_string()]
                );
                assert!(!passed);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_send_rejects_other_peer_b58_grant() {
        let s = server_with(vec![], "");
        let task = dummy_a2a_task_for(&s);
        // Compose a b58 form against a *different* pubkey. The check
        // must NOT honour it — defeats the display-collision attack a
        // Phase-1 second peer would otherwise enable.
        let other = AgentId::new("research@local", [42u8; 32]);
        let other_alternatives = other.scoped_action_alternatives("a2a.send");
        assert_ne!(
            other_alternatives[1],
            task.recipient.scoped_action_alternatives("a2a.send")[1],
            "test premise: the b58 forms must differ between the two pubkeys"
        );
        s.op_respond(Request::GrantCapability {
            action: other_alternatives[1].clone(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s.op_respond(Request::SendA2ATask { task }).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("requires capability"));
                assert!(message.contains("a2a.send.research@local"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_send_rejects_scope_peer_mismatch_and_audits() {
        let s = server_with(vec![], "");
        let task = dummy_a2a_task_for(&s);
        let action = format!("a2a.send.{}", task.recipient.display);
        let other_peer = AgentId::new("other@local", [11u8; 32]);
        grant_scoped_action(
            &s,
            &action,
            serde_json::json!({
                "version": 1,
                "peer_pubkey_b58": other_peer.pubkey_base58(),
                "task_id": task.id.to_string()
            }),
        )
        .await;

        let resp = s
            .op_respond(Request::SendA2ATask { task: task.clone() })
            .await;
        match resp {
            Response::Error { message } => assert!(message.contains("capability scope")),
            other => panic!("expected Error, got {other:?}"),
        }

        match s
            .op_respond(Request::RecentAudit {
                limit: 20,
                since_ms: None,
                prefer_stream: None,
            })
            .await
        {
            Response::AuditEvents { events } => assert!(events.iter().any(|event| {
                matches!(
                    &event.kind,
                    AuditKind::CapabilityScopeRejected { action: got, .. } if got == &action
                )
            })),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_recv_gate_accepts_pubkey_b58_grant() {
        let s = server_with(vec![], "");
        let peer = s.identity.agent_id();
        let foreign_recipient = AgentId::new("victim@local", [7u8; 32]);
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", foreign_recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        // Recipient grants `a2a.recv.<sender_pubkey_b58>` instead of the
        // display form. The gate must accept it. Sign with the daemon
        // identity so the cap passes the trust-root check.
        let recv_alternatives = peer.scoped_action_alternatives("a2a.recv");
        let recv_cap = covenant_types::Capability {
            subject: foreign_recipient.clone(),
            action: recv_alternatives[1].clone(),
            scope: serde_json::json!({}),
            granted_by: peer.clone(),
            expires_at: None,
        };
        let signed = sign_capability(recv_cap, s.identity.signing_key());
        s.capabilities.record(signed).await.unwrap();

        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: peer.clone(),
            recipient: foreign_recipient,
            intent_text: "authorised via b58".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        let resp = s
            .op_respond(Request::SendA2ATask { task: task.clone() })
            .await;
        match resp {
            Response::A2ATaskQueued { task_id } => assert_eq!(task_id, task.id),
            other => panic!("recv gate must accept b58 form, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_recv_gate_rejects_other_peer_b58_grant() {
        let s = server_with(vec![], "");
        let peer = s.identity.agent_id();
        let foreign_recipient = AgentId::new("victim@local", [7u8; 32]);
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", foreign_recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        // Recipient grants the b58 form for a *different* sender pubkey.
        // The gate must reject this send — collision-attack defense.
        let other = AgentId::new(peer.display.clone(), [42u8; 32]);
        let other_recv = other.scoped_action_alternatives("a2a.recv");
        assert_ne!(
            other_recv[1],
            peer.scoped_action_alternatives("a2a.recv")[1]
        );
        let alien_grantor = LocalIdentity::generate("granter@local");
        let recv_cap = covenant_types::Capability {
            subject: foreign_recipient.clone(),
            action: other_recv[1].clone(),
            scope: serde_json::json!({}),
            granted_by: alien_grantor.agent_id(),
            expires_at: None,
        };
        let signed = sign_capability(recv_cap, alien_grantor.signing_key());
        s.capabilities.record(signed).await.unwrap();

        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: peer.clone(),
            recipient: foreign_recipient,
            intent_text: "spoofed b58".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        let resp = s.op_respond(Request::SendA2ATask { task }).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("recipient has not granted"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_respond_accepts_pubkey_b58_grant() {
        let s = server_with(vec![], "");
        let task = dummy_a2a_task_for(&s);
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", task.recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::SendA2ATask { task: task.clone() })
            .await;
        // Grant the b58 form for the respond cap; the respond check must
        // accept it.
        let respond_alternatives = task.sender.scoped_action_alternatives("a2a.respond");
        s.op_respond(Request::GrantCapability {
            action: respond_alternatives[1].clone(),
            scope: None,
            expires_at: None,
        })
        .await;
        let result =
            covenant_a2a::A2ATaskResult::ok(task.id, vec![covenant_mcp::Content::text("done")]);
        let resp = s.op_respond(Request::PostA2AResult { result }).await;
        match resp {
            Response::A2AResultPosted { task_id } => assert_eq!(task_id, task.id),
            other => panic!("b58 grant must satisfy respond-cap, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_respond_rejects_scope_task_mismatch_and_audits() {
        let s = server_with(vec![], "");
        let task = dummy_a2a_task_for(&s);
        grant_action(&s, &format!("a2a.send.{}", task.recipient.display)).await;
        s.op_respond(Request::SendA2ATask { task: task.clone() })
            .await;

        let action = format!("a2a.respond.{}", task.sender.display);
        grant_scoped_action(
            &s,
            &action,
            serde_json::json!({
                "version": 1,
                "peer_pubkey_b58": task.sender.pubkey_base58(),
                "task_id": Uuid::new_v4().to_string()
            }),
        )
        .await;

        let result =
            covenant_a2a::A2ATaskResult::ok(task.id, vec![covenant_mcp::Content::text("done")]);
        let resp = s.op_respond(Request::PostA2AResult { result }).await;
        match resp {
            Response::Error { message } => assert!(message.contains("capability scope")),
            other => panic!("expected Error, got {other:?}"),
        }

        match s
            .op_respond(Request::RecentAudit {
                limit: 30,
                since_ms: None,
                prefer_stream: None,
            })
            .await
        {
            Response::AuditEvents { events } => assert!(events.iter().any(|event| {
                matches!(
                    &event.kind,
                    AuditKind::CapabilityScopeRejected { action: got, .. } if got == &action
                )
            })),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_respond_audit_records_matched_b58_form() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        let task = dummy_a2a_task_for(&s);
        s.op_respond(Request::GrantCapability {
            action: format!("a2a.send.{}", task.recipient.display),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::SendA2ATask { task: task.clone() })
            .await;
        let respond_alternatives = task.sender.scoped_action_alternatives("a2a.respond");
        let b58_form = respond_alternatives[1].clone();
        s.op_respond(Request::GrantCapability {
            action: b58_form.clone(),
            scope: None,
            expires_at: None,
        })
        .await;
        let result =
            covenant_a2a::A2ATaskResult::ok(task.id, vec![covenant_mcp::Content::text("done")]);
        s.op_respond(Request::PostA2AResult { result }).await;

        let events = audit.recent(50).await.unwrap();
        let cap = events
            .iter()
            .filter(|e| matches!(e.kind, AuditKind::CapabilityCheck { .. }))
            .find(|e| match &e.kind {
                AuditKind::CapabilityCheck { agent_id, .. } => agent_id.starts_with("a2a-respond:"),
                _ => false,
            })
            .expect("expected a CapabilityCheck for the respond call");
        match &cap.kind {
            AuditKind::CapabilityCheck {
                required_actions,
                missing_actions,
                passed,
                ..
            } => {
                assert_eq!(required_actions, &vec![b58_form.clone()]);
                assert!(missing_actions.is_empty());
                assert!(*passed);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn purge_audit_rejects_without_capability() {
        let s = server_with(vec![], "");
        // No `audit.purge` cap granted — purge attempt is rejected even
        // though the peer is authenticated.
        let resp = s.op_respond(Request::PurgeAudit { before_ms: 1 }).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("audit.purge"));
                assert!(message.contains("requires capability"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn purge_audit_passes_after_grant() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "audit.purge".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s.op_respond(Request::PurgeAudit { before_ms: 0 }).await;
        match resp {
            Response::AuditPurged { .. } => {}
            other => panic!("expected AuditPurged, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn purge_audit_accepts_scope_cutoff() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "audit.purge".into(),
            scope: Some(serde_json::json!({
                "version": 1,
                "before_ms": 1_000
            })),
            expires_at: None,
        })
        .await;
        let resp = s.op_respond(Request::PurgeAudit { before_ms: 1_000 }).await;
        match resp {
            Response::AuditPurged { .. } => {}
            other => panic!("expected AuditPurged, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn purge_audit_rejects_scope_cutoff_exceeded() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "audit.purge".into(),
            scope: Some(serde_json::json!({
                "version": 1,
                "before_ms": 1_000
            })),
            expires_at: None,
        })
        .await;
        let resp = s.op_respond(Request::PurgeAudit { before_ms: 1_001 }).await;
        match resp {
            Response::Error { message } => assert!(message.contains("capability scope")),
            other => panic!("expected Error, got {other:?}"),
        }

        match s
            .op_respond(Request::RecentAudit {
                limit: 10,
                since_ms: None,
                prefer_stream: None,
            })
            .await
        {
            Response::AuditEvents { events } => assert!(events.iter().any(|event| {
                matches!(
                    &event.kind,
                    AuditKind::CapabilityScopeRejected { action, .. } if action == "audit.purge"
                )
            })),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn purge_capabilities_rejects_without_capability() {
        let s = server_with(vec![], "");
        let resp = s
            .op_respond(Request::PurgeCapabilities { before_ms: 1 })
            .await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("capabilities.purge"));
                assert!(message.contains("requires capability"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn purge_capabilities_passes_after_grant() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "capabilities.purge".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s
            .op_respond(Request::PurgeCapabilities { before_ms: 0 })
            .await;
        match resp {
            Response::CapabilitiesPurged { .. } => {}
            other => panic!("expected CapabilitiesPurged, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn compact_a2a_rejects_without_capability() {
        let s = server_with(vec![], "");
        let resp = s.op_respond(Request::CompactA2A).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("a2a.compact"));
                assert!(message.contains("requires capability"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn compact_a2a_passes_after_grant() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "a2a.compact".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s.op_respond(Request::CompactA2A).await;
        match resp {
            Response::A2ACompacted { dropped } => {
                // No tasks to compact in a fresh in-memory mailbox.
                assert_eq!(dropped, 0);
            }
            other => panic!("expected A2ACompacted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn purge_peers_rejects_without_capability() {
        let s = server_with(vec![], "");
        let resp = s.op_respond(Request::PurgePeers { before_ms: 1 }).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("peers.purge"));
                assert!(message.contains("requires capability"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn purge_peers_passes_after_grant() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "peers.purge".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let resp = s.op_respond(Request::PurgePeers { before_ms: 0 }).await;
        match resp {
            Response::PeersPurged { purged } => {
                assert_eq!(purged, 0, "no revocations to purge in a fresh registry");
            }
            other => panic!("expected PeersPurged, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn purge_peers_rejects_scope_cutoff_exceeded_and_audits() {
        let s = server_with(vec![], "");
        grant_scoped_action(
            &s,
            "peers.purge",
            serde_json::json!({
                "version": 1,
                "before_ms": 1_000
            }),
        )
        .await;
        let resp = s.op_respond(Request::PurgePeers { before_ms: 1_001 }).await;
        match resp {
            Response::Error { message } => assert!(message.contains("capability scope")),
            other => panic!("expected scope rejection, got {other:?}"),
        }
        assert!(s.audit.recent(50).await.unwrap().iter().any(|event| {
            matches!(
                &event.kind,
                AuditKind::CapabilityScopeRejected { action, .. } if action == "peers.purge"
            )
        }));
    }

    #[tokio::test]
    async fn revoke_rejects_when_peer_is_not_subject() {
        let s = server_with(vec![], "");
        // Operator (= s.identity) grants themselves a cap. The capability's
        // subject pubkey is the operator's. A different peer asking to
        // revoke that signature must be rejected even though the signature
        // is publicly visible via `/capabilities/recent`.
        let granted = s
            .op_respond(Request::GrantCapability {
                action: "tool.web_search".into(),
                scope: None,
                expires_at: None,
            })
            .await;
        let sig_b58 = match granted {
            Response::CapabilityGranted { signature_b58, .. } => signature_b58,
            other => panic!("unexpected: {other:?}"),
        };

        let stranger = AgentId::new("stranger@local", [7u8; 32]);
        let resp = s
            .respond(
                Request::RevokeCapability {
                    signature_b58: sig_b58.clone(),
                },
                &stranger,
            )
            .await;
        match resp {
            Response::Error { message } => assert!(message.contains("does not match")),
            other => panic!("expected Error, got {other:?}"),
        }

        // Cap is still live: operator can still revoke it.
        let owner = s
            .op_respond(Request::RevokeCapability {
                signature_b58: sig_b58,
            })
            .await;
        match owner {
            Response::CapabilityRevoked { removed, .. } => assert!(removed),
            other => panic!("expected CapabilityRevoked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn revoke_returns_false_for_already_revoked_signature() {
        let s = server_with(vec![], "");
        let granted = s
            .op_respond(Request::GrantCapability {
                action: "tool.web_search".into(),
                scope: None,
                expires_at: None,
            })
            .await;
        let sig_b58 = match granted {
            Response::CapabilityGranted { signature_b58, .. } => signature_b58,
            other => panic!("unexpected: {other:?}"),
        };

        let first = s
            .op_respond(Request::RevokeCapability {
                signature_b58: sig_b58.clone(),
            })
            .await;
        match first {
            Response::CapabilityRevoked { removed, .. } => assert!(removed),
            other => panic!("expected CapabilityRevoked, got {other:?}"),
        }

        let second = s
            .op_respond(Request::RevokeCapability {
                signature_b58: sig_b58,
            })
            .await;
        match second {
            Response::CapabilityRevoked { removed, .. } => assert!(!removed),
            other => panic!("expected idempotent CapabilityRevoked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn auth_failure_records_audit_event() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        s.record_auth_failure("ipc", "first frame must be Authenticate")
            .await
            .unwrap();
        s.record_auth_failure("http", "missing Authorization header")
            .await
            .unwrap();
        let events = audit.recent(10).await.unwrap();
        let mut transports: Vec<&str> = events
            .iter()
            .filter_map(|e| match &e.kind {
                AuditKind::AuthenticationFailed { transport, .. } => Some(transport.as_str()),
                _ => None,
            })
            .collect();
        transports.sort();
        assert_eq!(transports, vec!["http", "ipc"]);
    }

    #[tokio::test]
    async fn recent_audit_returns_events_in_order() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "tool.call.echo".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::CallTool {
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "hi" }),
        })
        .await;
        let resp = s
            .op_respond(Request::RecentAudit {
                limit: 10,
                since_ms: None,
                prefer_stream: None,
            })
            .await;
        match resp {
            Response::AuditEvents { events } => {
                assert!(
                    events
                        .iter()
                        .any(|e| matches!(e.kind, AuditKind::CapabilityGranted { .. })),
                    "expected a CapabilityGranted event"
                );
                assert!(
                    events
                        .iter()
                        .any(|e| matches!(e.kind, AuditKind::CapabilityCheck { .. })),
                    "expected a CapabilityCheck event from the tool dispatch"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_audit_since_ms_drops_older_events_before_limit() {
        let s = server_with(vec![], "");
        let mine = s.identity.agent_id();
        for ts in [1_000u64, 2_000, 3_000, 4_000, 5_000] {
            s.audit
                .record(AuditEvent {
                    id: Uuid::new_v4(),
                    timestamp_ms: ts,
                    issuer: mine.clone(),
                    kind: AuditKind::IntentDispatched {
                        intent_id: Uuid::new_v4(),
                        intent_text: format!("row@{ts}"),
                        matched_agent: None,
                        result_hash_hex: hash_hex(b""),
                        status: "ok".into(),
                    },
                })
                .await
                .unwrap();
        }

        let resp = s
            .op_respond(Request::RecentAudit {
                limit: 10,
                since_ms: Some(3_000),
                prefer_stream: None,
            })
            .await;
        match resp {
            Response::AuditEvents { events } => {
                let timestamps: Vec<u64> = events.iter().map(|e| e.timestamp_ms).collect();
                assert!(
                    timestamps.iter().all(|ts| *ts >= 3_000),
                    "since_ms must drop rows below the threshold: timestamps={timestamps:?}",
                );
                assert!(
                    timestamps.contains(&3_000),
                    "boundary is inclusive at >=, so the row at the threshold must survive: timestamps={timestamps:?}",
                );
            }
            other => panic!("unexpected: {other:?}"),
        }

        let narrow = s
            .op_respond(Request::RecentAudit {
                limit: 1,
                since_ms: Some(2_000),
                prefer_stream: None,
            })
            .await;
        match narrow {
            Response::AuditEvents { events } => {
                assert_eq!(
                    events.len(),
                    1,
                    "limit must still cap the response after the filter",
                );
                assert_eq!(
                    events[0].timestamp_ms, 5_000,
                    "since_ms applies before limit truncation so the newest in-window row survives a tight --limit, not the oldest: events={events:?}",
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_audit_scrubs_other_peers_rows() {
        let s = server_with(vec![], "");
        let alien = AgentId::new("alice@local", [9u8; 32]);
        s.audit
            .record(AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: alien.clone(),
                kind: AuditKind::BudgetExhausted {
                    agent_display: "research@agent".into(),
                    intent_id: Uuid::new_v4(),
                    intent_text: "alice's secret intent".into(),
                    requested: 1,
                    tokens_remaining: 0,
                    refill_eta_ms: u64::MAX,
                },
            })
            .await
            .unwrap();
        let mine = s.identity.agent_id();
        s.audit
            .record(AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: mine.clone(),
                kind: AuditKind::IntentDispatched {
                    intent_id: Uuid::new_v4(),
                    intent_text: "operator's own intent".into(),
                    matched_agent: None,
                    result_hash_hex: hash_hex(b""),
                    status: "ok".into(),
                },
            })
            .await
            .unwrap();
        let resp = s
            .op_respond(Request::RecentAudit {
                limit: 100,
                since_ms: None,
                prefer_stream: None,
            })
            .await;
        match resp {
            Response::AuditEvents { events } => {
                assert!(
                    events.iter().all(|e| e.issuer.pubkey == mine.pubkey),
                    "every returned row must belong to the requesting peer"
                );
                assert!(
                    !events.iter().any(|e| matches!(
                        &e.kind,
                        AuditKind::BudgetExhausted { intent_text, .. }
                            if intent_text == "alice's secret intent"
                    )),
                    "alien BudgetExhausted row leaked through filter"
                );
                assert!(
                    events.iter().any(|e| matches!(
                        &e.kind,
                        AuditKind::IntentDispatched { intent_text, .. }
                            if intent_text == "operator's own intent"
                    )),
                    "operator's own row should still be visible"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn resume_intent_rejects_other_peers_intent() {
        let s = server_with(vec![], "");
        let alien = AgentId::new("alice@local", [9u8; 32]);
        let alien_intent_id = Uuid::new_v4();
        s.audit
            .record(AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: alien,
                kind: AuditKind::BudgetExhausted {
                    agent_display: "research@agent".into(),
                    intent_id: alien_intent_id,
                    intent_text: "leaked".into(),
                    requested: 1,
                    tokens_remaining: 0,
                    refill_eta_ms: u64::MAX,
                },
            })
            .await
            .unwrap();
        let resp = s
            .op_respond(Request::ResumeIntent {
                intent_id: alien_intent_id,
            })
            .await;
        match resp {
            Response::Error { message } => {
                assert!(
                    message.contains("no BudgetExhausted audit row"),
                    "expected the not-found error, got: {message}"
                );
                assert!(
                    !message.contains("leaked"),
                    "intent_text must not appear in the error"
                );
            }
            other => panic!("expected Response::Error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_audit_v0_operator_sees_own_events() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "tool.call.echo".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        s.op_respond(Request::CallTool {
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "hi" }),
        })
        .await;
        let me = s.identity.agent_id();
        let resp = s
            .op_respond(Request::RecentAudit {
                limit: 10,
                since_ms: None,
                prefer_stream: None,
            })
            .await;
        match resp {
            Response::AuditEvents { events } => {
                assert!(!events.is_empty(), "operator should see their own rows");
                assert!(
                    events.iter().all(|e| e.issuer.pubkey == me.pubkey),
                    "filter must keep operator's rows"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn audit_integrity_returns_report_for_operator() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "tool.call.echo".into(),
            scope: None,
            expires_at: None,
        })
        .await;

        let resp = s.op_respond(Request::VerifyAuditIntegrity).await;
        match resp {
            Response::AuditIntegrity { report } => {
                assert!(report.valid, "{report:?}");
                assert!(report.events > 0);
                assert_eq!(report.events, report.anchors);
                assert_eq!(report.root_hash_hex.len(), 64);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn audit_integrity_rejects_non_operator() {
        let s = server_with(vec![], "");
        let foreign = AgentId::new("guest@local", [9u8; 32]);

        let resp = s.respond(Request::VerifyAuditIntegrity, &foreign).await;
        match resp {
            Response::Error { message } => {
                assert!(message.contains("operator identity"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    // -------- Per-peer filter on the other RecentX surfaces --------

    #[tokio::test]
    async fn recent_memory_scrubs_other_peers_records() {
        let s = server_with(vec![], "");
        grant_action(&s, "memory.read").await;
        let alien = AgentId::new("alice@local", [9u8; 32]);
        let alien_record = MemoryRecord {
            id: Uuid::new_v4(),
            tier: MemoryTier::Working,
            owner: alien,
            text: "alice's secret memory".into(),
            embedding: Vec::new(),
            metadata: serde_json::json!({}),
            created_at: epoch_ms(),
            parent: None,
        };
        s.memory.put(alien_record).await.unwrap();
        let me = s.identity.agent_id();
        let mine = MemoryRecord {
            id: Uuid::new_v4(),
            tier: MemoryTier::Working,
            owner: me.clone(),
            text: "operator's own memory".into(),
            embedding: Vec::new(),
            metadata: serde_json::json!({}),
            created_at: epoch_ms(),
            parent: None,
        };
        s.memory.put(mine).await.unwrap();
        let resp = s
            .op_respond(Request::RecentMemory {
                tier: None,
                limit: 100,
                prefer_stream: None,
            })
            .await;
        match resp {
            Response::Memories { records } => {
                assert!(
                    records.iter().all(|r| r.owner.pubkey == me.pubkey),
                    "every returned row must be owned by the requesting peer"
                );
                assert!(
                    !records.iter().any(|r| r.text == "alice's secret memory"),
                    "alien memory record leaked through filter"
                );
                assert!(
                    records.iter().any(|r| r.text == "operator's own memory"),
                    "operator's own row should still be visible"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_memory_rejects_without_read_capability() {
        let s = server_with(vec![], "");
        let resp = s
            .op_respond(Request::RecentMemory {
                tier: None,
                limit: 10,
                prefer_stream: None,
            })
            .await;
        match resp {
            Response::Error { message } => assert!(message.contains("memory read requires")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_recent_memory_happy_path_emits_begin_chunks_end_and_purges_tracker() {
        // Capability granted, two records in store. The orchestrator
        // routes through emit_memory_stream, the tracker holds one
        // entry mid-flight, and the entry is gone after the method
        // returns Ok.
        let s = server_with(vec![], "");
        grant_action(&s, "memory.read").await;
        let me = s.identity.agent_id();
        for i in 0..2u8 {
            s.memory
                .put(MemoryRecord {
                    id: Uuid::from_bytes([i + 1; 16]),
                    tier: MemoryTier::Working,
                    owner: me.clone(),
                    text: format!("memory {i}"),
                    embedding: Vec::new(),
                    metadata: serde_json::json!({}),
                    created_at: 100 + i as u64,
                    parent: None,
                })
                .await
                .unwrap();
        }

        let connection_id = Uuid::new_v4();
        let mut buf = Vec::new();
        s.stream_recent_memory(&mut buf, connection_id, None, 10, &me)
            .await
            .expect("stream_recent_memory must succeed on the happy path");

        let mut cursor = std::io::Cursor::new(buf.as_slice());
        let mut envelopes = Vec::new();
        while let Ok(env) =
            covenant_ipc::read_frame::<_, covenant_ipc::StreamEnvelope>(&mut cursor).await
        {
            envelopes.push(env);
        }
        assert_eq!(envelopes.len(), 4, "begin + 2 chunks + end = 4 frames");
        assert!(matches!(
            envelopes[0],
            covenant_ipc::StreamEnvelope::StreamBegin { .. }
        ));
        assert!(matches!(
            envelopes[3],
            covenant_ipc::StreamEnvelope::StreamEnd { summary: None, .. }
        ));
        assert!(
            s.stream_tracker.is_empty(),
            "tracker must be empty after a successful stream_recent_memory; register-without-unregister leaks entries"
        );
    }

    #[tokio::test]
    async fn stream_recent_memory_capability_failure_writes_v1_error_and_skips_tracker() {
        // No grant. Expected: writer received a v1-shape
        // Response::Error frame (NOT a StreamEnvelope), tracker
        // stays empty. ADR 0010's 'daemon decides per verb' clause
        // permits the v1 fallback frame.
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let connection_id = Uuid::new_v4();
        let mut buf = Vec::new();
        s.stream_recent_memory(&mut buf, connection_id, None, 10, &me)
            .await
            .expect("capability-failure path must still return Ok — error went out on the wire");

        // The capability-failure path writes a v1 Response, not a
        // StreamEnvelope. Decoding as Response succeeds; decoding as
        // StreamEnvelope must fail because the JSON does not match
        // any envelope variant.
        let mut cursor = std::io::Cursor::new(buf.as_slice());
        let resp: covenant_ipc::Response = covenant_ipc::read_frame(&mut cursor)
            .await
            .expect("first frame must decode as v1 Response");
        match resp {
            covenant_ipc::Response::Error { message } => {
                assert!(
                    message.contains("memory read requires"),
                    "expected capability-failure message, got {message:?}"
                );
            }
            other => panic!("expected v1 Response::Error, got {other:?}"),
        }
        // No more frames — the v1 fallback writes exactly one.
        assert!(
            cursor.position() == buf.len() as u64,
            "v1 fallback path must write exactly one frame; got extra bytes"
        );
        assert!(
            s.stream_tracker.is_empty(),
            "capability-failure path must NOT touch the tracker"
        );
    }

    /// Writer that rejects every write with a broken-pipe error, used to
    /// drive the streaming orchestrators' emit path to failure — it models
    /// a client that disconnects the moment the daemon starts streaming.
    struct FailingWriter;
    impl tokio::io::AsyncWrite for FailingWriter {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "injected write failure",
            )))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn stream_recent_memory_write_failure_unregisters_tracker() {
        // Capability granted and a record present, so the orchestrator
        // takes the streaming path and registers a tracker entry. The
        // writer then rejects the frame: emit returns Err, but the
        // unregister must still run so the entry does not leak for the
        // connection's lifetime. result.is_err() distinguishes this from
        // the v1 capability-fallback path, which returns Ok.
        let s = server_with(vec![], "");
        grant_action(&s, "memory.read").await;
        let me = s.identity.agent_id();
        s.memory
            .put(MemoryRecord {
                id: Uuid::from_bytes([1; 16]),
                tier: MemoryTier::Working,
                owner: me.clone(),
                text: "memory 0".into(),
                embedding: Vec::new(),
                metadata: serde_json::json!({}),
                created_at: 100,
                parent: None,
            })
            .await
            .unwrap();

        let connection_id = Uuid::new_v4();
        let mut writer = FailingWriter;
        let result = s
            .stream_recent_memory(&mut writer, connection_id, None, 10, &me)
            .await;
        assert!(
            result.is_err(),
            "a rejected write on the streaming path must propagate as Err"
        );
        assert!(
            s.stream_tracker.is_empty(),
            "the tracker entry must be unregistered even when emit fails; the error path must not leak entries"
        );
    }

    #[tokio::test]
    async fn stream_recent_audit_write_failure_unregisters_tracker() {
        // Audit has no capability gate; one recorded event puts the
        // orchestrator on the streaming path. The writer rejects the
        // frame, so emit fails — the tracker entry must still be cleared.
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        s.record_peer_event(
            &me,
            AuditEvent {
                id: Uuid::from_bytes([10; 16]),
                timestamp_ms: 1_700_000_000_000,
                issuer: me.clone(),
                kind: AuditKind::IntentDispatched {
                    intent_id: Uuid::from_bytes([20; 16]),
                    intent_text: "intent 0".into(),
                    matched_agent: Some("test-agent".into()),
                    result_hash_hex: format!("{:064x}", 0u64),
                    status: "ok".into(),
                },
            },
        )
        .await;

        let connection_id = Uuid::new_v4();
        let mut writer = FailingWriter;
        let result = s
            .stream_recent_audit(&mut writer, connection_id, 10, None, &me)
            .await;
        assert!(
            result.is_err(),
            "a rejected write on the audit streaming path must propagate as Err"
        );
        assert!(
            s.stream_tracker.is_empty(),
            "the audit tracker entry must be unregistered even when emit fails"
        );
    }

    #[tokio::test]
    async fn stream_submit_intent_write_failure_unregisters_tracker() {
        // Card matches and the required caps are granted, so dispatch_intent
        // returns an IntentResult and the orchestrator registers a tracker
        // entry before emitting. The writer rejects the frame: emit fails,
        // and the unregister must still clear the entry.
        let s = server_with(
            vec![stub_card("research", vec!["tool.web_search"])],
            "mocked summary",
        );
        grant_action(&s, "tool.web_search").await;
        grant_action(&s, "memory.write").await;
        let me = s.identity.agent_id();

        let connection_id = Uuid::new_v4();
        let mut writer = FailingWriter;
        let result = s
            .stream_submit_intent(
                &mut writer,
                connection_id,
                "find recent papers on agent memory".into(),
                &me,
            )
            .await;
        assert!(
            result.is_err(),
            "a rejected write on the intent streaming path must propagate as Err"
        );
        assert!(
            s.stream_tracker.is_empty(),
            "the intent tracker entry must be unregistered even when emit fails"
        );
    }

    #[tokio::test]
    async fn stream_recent_memory_two_calls_use_distinct_stream_ids() {
        // Two back-to-back calls on the same connection must emit
        // distinct stream_ids in their StreamBegin frames. A
        // refactor that took stream_id from a connection-level
        // field would surface here.
        let s = server_with(vec![], "");
        grant_action(&s, "memory.read").await;
        let me = s.identity.agent_id();
        let connection_id = Uuid::new_v4();

        let mut buf_a = Vec::new();
        s.stream_recent_memory(&mut buf_a, connection_id, None, 10, &me)
            .await
            .unwrap();
        let mut buf_b = Vec::new();
        s.stream_recent_memory(&mut buf_b, connection_id, None, 10, &me)
            .await
            .unwrap();

        async fn read_first_begin(buf: &[u8]) -> Uuid {
            let mut cursor = std::io::Cursor::new(buf);
            let env: covenant_ipc::StreamEnvelope =
                covenant_ipc::read_frame(&mut cursor).await.unwrap();
            match env {
                covenant_ipc::StreamEnvelope::StreamBegin { stream_id, .. } => stream_id,
                other => panic!("expected StreamBegin, got {other:?}"),
            }
        }
        let id_a = read_first_begin(&buf_a).await;
        let id_b = read_first_begin(&buf_b).await;
        assert_ne!(
            id_a, id_b,
            "consecutive streams must allocate fresh stream_ids"
        );
    }

    #[tokio::test]
    async fn recent_memory_envelopes_happy_path_returns_begin_chunks_end() {
        // Capability granted, three records in store. The collector
        // returns Ok([Begin, Chunk×3, End]); the StreamBegin's
        // response_kind matches the writer-based form; chunk
        // sequences are 0/1/2 in order; the StreamEnd carries no
        // summary; tracker is empty after the call.
        let s = server_with(vec![], "");
        grant_action(&s, "memory.read").await;
        let me = s.identity.agent_id();
        for i in 0..3u8 {
            s.memory
                .put(MemoryRecord {
                    id: Uuid::from_bytes([i + 1; 16]),
                    tier: MemoryTier::Working,
                    owner: me.clone(),
                    text: format!("memory {i}"),
                    embedding: Vec::new(),
                    metadata: serde_json::json!({}),
                    created_at: 100 + i as u64,
                    parent: None,
                })
                .await
                .unwrap();
        }

        let connection_id = Uuid::new_v4();
        let envelopes = s
            .recent_memory_envelopes(None, 10, &me, connection_id)
            .await
            .expect("happy path must return Ok");
        assert_eq!(envelopes.len(), 5, "begin + 3 chunks + end = 5 envelopes");
        match &envelopes[0] {
            StreamEnvelope::StreamBegin { response_kind, .. } => {
                assert_eq!(response_kind, stream_dispatch::MEMORY_RESPONSE_KIND);
            }
            other => panic!("expected StreamBegin, got {other:?}"),
        }
        for (i, env) in envelopes[1..=3].iter().enumerate() {
            match env {
                StreamEnvelope::StreamChunk { sequence, .. } => {
                    assert_eq!(*sequence, i as u32, "chunk {i} must have sequence {i}");
                }
                other => panic!("expected StreamChunk at index {}, got {other:?}", i + 1),
            }
        }
        assert!(matches!(
            envelopes[4],
            StreamEnvelope::StreamEnd { summary: None, .. }
        ));
        assert!(
            s.stream_tracker.is_empty(),
            "tracker must be empty after a successful recent_memory_envelopes"
        );
    }

    #[tokio::test]
    async fn recent_memory_envelopes_empty_records_returns_begin_end_only() {
        // Capability granted but no memory records. The collector
        // returns Ok([Begin, End]) — the begin+end pair is emitted
        // even on empty pages so a dead daemon is never confused with
        // an empty stream at the protocol layer.
        let s = server_with(vec![], "");
        grant_action(&s, "memory.read").await;
        let me = s.identity.agent_id();
        let connection_id = Uuid::new_v4();
        let envelopes = s
            .recent_memory_envelopes(None, 10, &me, connection_id)
            .await
            .expect("empty page must return Ok");
        assert_eq!(envelopes.len(), 2, "begin + end = 2 envelopes (no chunks)");
        assert!(matches!(envelopes[0], StreamEnvelope::StreamBegin { .. }));
        assert!(matches!(
            envelopes[1],
            StreamEnvelope::StreamEnd { summary: None, .. }
        ));
    }

    #[tokio::test]
    async fn recent_memory_envelopes_capability_fail_returns_err_response() {
        // No grant. Expected: Err(Response::Error) so the HTTP handler
        // can render a buffered JSON response instead of an empty SSE
        // stream. Tracker must stay empty — register+unregister never
        // runs when capability fails.
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let connection_id = Uuid::new_v4();
        let err = s
            .recent_memory_envelopes(None, 10, &me, connection_id)
            .await
            .expect_err("missing memory.read scope must produce Err(Response::Error)");
        match err {
            Response::Error { message } => {
                assert!(
                    message.contains("memory read requires"),
                    "capability-failure message must come through verbatim; got {message:?}"
                );
            }
            other => panic!("expected Response::Error, got {other:?}"),
        }
        assert!(
            s.stream_tracker.is_empty(),
            "capability-failure path must NOT touch the tracker"
        );
    }

    #[tokio::test]
    async fn stream_recent_audit_happy_path_emits_begin_chunks_end_and_purges_tracker() {
        // Pre-populate two audit events via record_peer_event so
        // recent_audit's peer-scoped filter returns both. Drive the
        // orchestrator through a Vec<u8>, decode 4 frames, assert
        // tracker is empty after.
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        for i in 0..2u8 {
            let event = AuditEvent {
                id: Uuid::from_bytes([i + 10; 16]),
                timestamp_ms: 1_700_000_000_000 + i as u64,
                issuer: me.clone(),
                kind: AuditKind::IntentDispatched {
                    intent_id: Uuid::from_bytes([i + 20; 16]),
                    intent_text: format!("intent {i}"),
                    matched_agent: Some("test-agent".into()),
                    result_hash_hex: format!("{:064x}", i as u64),
                    status: "ok".into(),
                },
            };
            s.record_peer_event(&me, event).await;
        }

        let connection_id = Uuid::new_v4();
        let mut buf = Vec::new();
        s.stream_recent_audit(&mut buf, connection_id, 10, None, &me)
            .await
            .expect("audit happy path must succeed");

        let mut cursor = std::io::Cursor::new(buf.as_slice());
        let mut envelopes = Vec::new();
        while let Ok(env) =
            covenant_ipc::read_frame::<_, covenant_ipc::StreamEnvelope>(&mut cursor).await
        {
            envelopes.push(env);
        }
        assert_eq!(envelopes.len(), 4, "begin + 2 chunks + end = 4 frames");
        assert!(matches!(
            envelopes[0],
            covenant_ipc::StreamEnvelope::StreamBegin { .. }
        ));
        assert!(matches!(
            envelopes[3],
            covenant_ipc::StreamEnvelope::StreamEnd { summary: None, .. }
        ));
        assert!(
            s.stream_tracker.is_empty(),
            "tracker must be empty after a successful stream_recent_audit"
        );
    }

    #[tokio::test]
    async fn stream_recent_audit_with_zero_events_emits_begin_then_end_and_purges_tracker() {
        // Audit has no capability gate; an empty audit log still
        // streams begin+end and leaves the tracker empty.
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let connection_id = Uuid::new_v4();
        let mut buf = Vec::new();
        s.stream_recent_audit(&mut buf, connection_id, 10, None, &me)
            .await
            .expect("empty audit must still emit begin+end");

        let mut cursor = std::io::Cursor::new(buf.as_slice());
        let mut envelopes = Vec::new();
        while let Ok(env) =
            covenant_ipc::read_frame::<_, covenant_ipc::StreamEnvelope>(&mut cursor).await
        {
            envelopes.push(env);
        }
        assert_eq!(envelopes.len(), 2, "empty audit emits exactly begin+end");
        assert!(s.stream_tracker.is_empty());
    }

    #[tokio::test]
    async fn stream_recent_audit_two_calls_use_distinct_stream_ids() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let connection_id = Uuid::new_v4();

        let mut buf_a = Vec::new();
        s.stream_recent_audit(&mut buf_a, connection_id, 10, None, &me)
            .await
            .unwrap();
        let mut buf_b = Vec::new();
        s.stream_recent_audit(&mut buf_b, connection_id, 10, None, &me)
            .await
            .unwrap();

        async fn read_first_begin(buf: &[u8]) -> Uuid {
            let mut cursor = std::io::Cursor::new(buf);
            let env: covenant_ipc::StreamEnvelope =
                covenant_ipc::read_frame(&mut cursor).await.unwrap();
            match env {
                covenant_ipc::StreamEnvelope::StreamBegin { stream_id, .. } => stream_id,
                other => panic!("expected StreamBegin, got {other:?}"),
            }
        }
        let id_a = read_first_begin(&buf_a).await;
        let id_b = read_first_begin(&buf_b).await;
        assert_ne!(
            id_a, id_b,
            "consecutive audit streams must allocate fresh stream_ids"
        );
    }

    #[tokio::test]
    async fn recent_audit_envelopes_happy_path_returns_begin_chunks_end() {
        // Pre-populate two audit events visible to `me` via the
        // peer-scoped recorder. The collector returns
        // Ok([Begin, Chunk×2, End]); response_kind matches the audit
        // const; chunk sequences are 0/1 in order; tracker is empty
        // after.
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        for i in 0..2u8 {
            let event = AuditEvent {
                id: Uuid::from_bytes([i + 10; 16]),
                timestamp_ms: 1_700_000_000_000 + i as u64,
                issuer: me.clone(),
                kind: AuditKind::IntentDispatched {
                    intent_id: Uuid::from_bytes([i + 20; 16]),
                    intent_text: format!("intent {i}"),
                    matched_agent: Some("test-agent".into()),
                    result_hash_hex: format!("{:064x}", i as u64),
                    status: "ok".into(),
                },
            };
            s.record_peer_event(&me, event).await;
        }

        let connection_id = Uuid::new_v4();
        let envelopes = s
            .recent_audit_envelopes(10, None, &me, connection_id)
            .await
            .expect("audit happy path must return Ok");
        assert_eq!(envelopes.len(), 4, "begin + 2 chunks + end = 4 envelopes");
        match &envelopes[0] {
            StreamEnvelope::StreamBegin { response_kind, .. } => {
                assert_eq!(response_kind, stream_dispatch::AUDIT_RESPONSE_KIND);
            }
            other => panic!("expected StreamBegin, got {other:?}"),
        }
        for (i, env) in envelopes[1..=2].iter().enumerate() {
            match env {
                StreamEnvelope::StreamChunk { sequence, .. } => {
                    assert_eq!(*sequence, i as u32);
                }
                other => panic!("expected StreamChunk at index {}, got {other:?}", i + 1),
            }
        }
        assert!(matches!(
            envelopes[3],
            StreamEnvelope::StreamEnd { summary: None, .. }
        ));
        assert!(
            s.stream_tracker.is_empty(),
            "tracker must be empty after a successful recent_audit_envelopes"
        );
    }

    #[tokio::test]
    async fn recent_audit_envelopes_empty_events_returns_begin_end_only() {
        // No audit events. The collector still emits begin+end so a
        // stream that never opens is never confused with a dead daemon
        // at the protocol layer.
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let connection_id = Uuid::new_v4();
        let envelopes = s
            .recent_audit_envelopes(10, None, &me, connection_id)
            .await
            .expect("empty audit page must return Ok");
        assert_eq!(envelopes.len(), 2, "begin + end = 2 envelopes (no chunks)");
        assert!(matches!(envelopes[0], StreamEnvelope::StreamBegin { .. }));
        assert!(matches!(
            envelopes[1],
            StreamEnvelope::StreamEnd { summary: None, .. }
        ));
    }

    #[tokio::test]
    async fn stream_submit_intent_happy_path_emits_begin_chunk_end_with_summary_and_purges_tracker()
    {
        // Agent card matches "find" + "papers", required caps granted.
        // dispatch_intent returns Response::IntentResult with a non-nil
        // intent_id, status="ok", and a paired settlement receipt. The
        // orchestrator must emit StreamBegin + 1 chunk (the AgentResult
        // text/sources, runtime_events emptied) + StreamEnd carrying a
        // summary Value that round-trips intent_id and status. Tracker
        // is empty after the method returns Ok.
        let s = server_with(
            vec![stub_card("research", vec!["tool.web_search"])],
            "mocked summary",
        );
        grant_action(&s, "tool.web_search").await;
        grant_action(&s, "memory.write").await;
        let me = s.identity.agent_id();
        let connection_id = Uuid::new_v4();

        let mut buf = Vec::new();
        s.stream_submit_intent(
            &mut buf,
            connection_id,
            "find recent papers on agent memory".into(),
            &me,
        )
        .await
        .expect("stream_submit_intent must succeed on the happy path");

        let mut cursor = std::io::Cursor::new(buf.as_slice());
        let mut envelopes = Vec::new();
        while let Ok(env) =
            covenant_ipc::read_frame::<_, covenant_ipc::StreamEnvelope>(&mut cursor).await
        {
            envelopes.push(env);
        }
        assert_eq!(envelopes.len(), 3, "begin + 1 chunk + end = 3 frames");
        match &envelopes[0] {
            covenant_ipc::StreamEnvelope::StreamBegin { response_kind, .. } => {
                assert_eq!(response_kind, "intent_result");
            }
            other => panic!("frame 0 must be StreamBegin, got {other:?}"),
        }
        match &envelopes[1] {
            covenant_ipc::StreamEnvelope::StreamChunk {
                sequence, chunk, ..
            } => {
                assert_eq!(*sequence, 0);
                assert_eq!(chunk["text"], "mocked summary");
                assert!(
                    chunk["runtime_events"].as_array().unwrap().is_empty(),
                    "runtime_events must be empty on the chunk — dispatch_intent already folded them into the audit chain, double-publishing would surface here"
                );
            }
            other => panic!("frame 1 must be StreamChunk, got {other:?}"),
        }
        let summary = match &envelopes[2] {
            covenant_ipc::StreamEnvelope::StreamEnd { summary, .. } => summary
                .as_ref()
                .expect("StreamEnd.summary must carry IntentResult bookkeeping"),
            other => panic!("frame 2 must be StreamEnd, got {other:?}"),
        };
        assert_eq!(summary["status"], "ok");
        let intent_id_str = summary["intent_id"]
            .as_str()
            .expect("intent_id must serialize as a string");
        Uuid::parse_str(intent_id_str).expect("intent_id must round-trip through Uuid::parse_str — a string-formatting drift would surface here");
        assert!(
            s.stream_tracker.is_empty(),
            "tracker must be empty after a successful stream_submit_intent"
        );
    }

    #[tokio::test]
    async fn stream_submit_intent_capability_failure_writes_v1_error_and_skips_tracker() {
        // No grants present. dispatch_intent's capability gate returns
        // Response::Error (not Response::IntentResult), so the
        // orchestrator's fallthrough writes a v1-shape terminal frame
        // and skips StreamTracker bookkeeping. ADR 0010 allows
        // daemon-decides-not-to-stream by falling back to v1 shape on
        // any non-IntentResult variant. A regression that wrapped the
        // capability failure in StreamBegin+StreamError would surface
        // here as a StreamEnvelope decode succeeding instead of
        // Response::Error.
        let s = server_with(
            vec![stub_card("research", vec!["tool.web_search"])],
            "mocked summary",
        );
        // Deliberately skip grant_action — no capability means
        // dispatch_intent returns Response::Error.
        let me = s.identity.agent_id();
        let connection_id = Uuid::new_v4();

        let mut buf = Vec::new();
        s.stream_submit_intent(
            &mut buf,
            connection_id,
            "find recent papers on agent memory".into(),
            &me,
        )
        .await
        .expect("capability-failure path must still return Ok — error went out on the wire");

        let mut cursor = std::io::Cursor::new(buf.as_slice());
        let resp: covenant_ipc::Response = covenant_ipc::read_frame(&mut cursor)
            .await
            .expect("first frame must decode as v1 Response");
        match resp {
            covenant_ipc::Response::Error { .. } => {}
            other => panic!("expected v1 Response::Error on capability failure, got {other:?}"),
        }
        assert!(
            s.stream_tracker.is_empty(),
            "tracker must be empty on the capability-failure path — the orchestrator skips register+unregister entirely"
        );
    }

    #[tokio::test]
    async fn stream_submit_intent_two_calls_use_distinct_stream_ids() {
        let s = server_with(
            vec![stub_card("research", vec!["tool.web_search"])],
            "mocked summary",
        );
        grant_action(&s, "tool.web_search").await;
        grant_action(&s, "memory.write").await;
        let me = s.identity.agent_id();
        let connection_id = Uuid::new_v4();

        let mut buf_a = Vec::new();
        s.stream_submit_intent(
            &mut buf_a,
            connection_id,
            "find recent papers on agent memory".into(),
            &me,
        )
        .await
        .unwrap();
        let mut buf_b = Vec::new();
        s.stream_submit_intent(
            &mut buf_b,
            connection_id,
            "find recent papers on agent memory".into(),
            &me,
        )
        .await
        .unwrap();

        async fn read_first_begin(buf: &[u8]) -> Uuid {
            let mut cursor = std::io::Cursor::new(buf);
            let env: covenant_ipc::StreamEnvelope =
                covenant_ipc::read_frame(&mut cursor).await.unwrap();
            match env {
                covenant_ipc::StreamEnvelope::StreamBegin { stream_id, .. } => stream_id,
                other => panic!("expected StreamBegin, got {other:?}"),
            }
        }
        let id_a = read_first_begin(&buf_a).await;
        let id_b = read_first_begin(&buf_b).await;
        assert_ne!(
            id_a, id_b,
            "consecutive intent streams must allocate fresh stream_ids"
        );
    }

    #[tokio::test]
    async fn submit_intent_envelopes_happy_path_returns_begin_chunk_end_with_summary() {
        // Symmetric setup with stream_submit_intent's happy-path test:
        // agent card matches "find" + "papers", required caps granted.
        // The collector returns Ok([Begin, Chunk, End]); the
        // AgentResult chunk has an empty runtime_events Vec
        // (dispatch_intent already folded events into the audit
        // chain); the StreamEnd's summary carries intent_id, status,
        // and settlement. Tracker is empty after.
        let s = server_with(
            vec![stub_card("research", vec!["tool.web_search"])],
            "mocked summary",
        );
        grant_action(&s, "tool.web_search").await;
        grant_action(&s, "memory.write").await;
        let me = s.identity.agent_id();
        let connection_id = Uuid::new_v4();
        let envelopes = s
            .submit_intent_envelopes(
                "find recent papers on agent memory".into(),
                &me,
                connection_id,
            )
            .await
            .expect("intent happy path must return Ok");
        assert_eq!(envelopes.len(), 3, "begin + 1 chunk + end = 3 envelopes");
        match &envelopes[0] {
            StreamEnvelope::StreamBegin { response_kind, .. } => {
                assert_eq!(response_kind, stream_dispatch::INTENT_RESPONSE_KIND);
            }
            other => panic!("frame 0 must be StreamBegin, got {other:?}"),
        }
        match &envelopes[1] {
            StreamEnvelope::StreamChunk {
                sequence, chunk, ..
            } => {
                assert_eq!(*sequence, 0);
                assert_eq!(chunk["text"], "mocked summary");
                assert!(
                    chunk["runtime_events"].as_array().unwrap().is_empty(),
                    "runtime_events must be empty on the chunk — double-publishing would surface here"
                );
            }
            other => panic!("frame 1 must be StreamChunk, got {other:?}"),
        }
        let summary = match &envelopes[2] {
            StreamEnvelope::StreamEnd { summary, .. } => summary
                .as_ref()
                .expect("StreamEnd.summary must carry IntentResult bookkeeping"),
            other => panic!("frame 2 must be StreamEnd, got {other:?}"),
        };
        assert!(
            summary.get("intent_id").is_some(),
            "summary must include intent_id key"
        );
        assert!(
            summary.get("status").is_some(),
            "summary must include status key"
        );
        assert!(
            summary.get("settlement").is_some(),
            "summary must include settlement key"
        );
        assert_eq!(summary["status"], "ok");
        let intent_id_str = summary["intent_id"]
            .as_str()
            .expect("intent_id must serialize as a string");
        Uuid::parse_str(intent_id_str).expect("intent_id must round-trip through Uuid::parse_str");
        assert!(
            s.stream_tracker.is_empty(),
            "tracker must be empty after a successful submit_intent_envelopes"
        );
    }

    #[tokio::test]
    async fn submit_intent_envelopes_capability_failure_returns_err_response() {
        // No grants — dispatch_intent's capability gate produces a
        // non-IntentResult Response. The collector returns Err(response)
        // so the HTTP handler can render a buffered JSON response with
        // the verbatim payload. Tracker stays empty — register+unregister
        // never runs on the non-IntentResult path.
        let s = server_with(
            vec![stub_card("research", vec!["tool.web_search"])],
            "mocked summary",
        );
        let me = s.identity.agent_id();
        let connection_id = Uuid::new_v4();
        let err = s
            .submit_intent_envelopes(
                "find recent papers on agent memory".into(),
                &me,
                connection_id,
            )
            .await
            .expect_err("missing capability must produce Err(Response)");
        match err {
            Response::IntentResult { .. } => {
                panic!(
                    "Err arm must NOT carry Response::IntentResult — that's the streamable variant"
                )
            }
            other => {
                let _ = other;
            }
        }
        assert!(
            s.stream_tracker.is_empty(),
            "capability-failure path must NOT touch the tracker"
        );
    }

    #[tokio::test]
    async fn recent_memory_filters_to_scoped_tier() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        s.memory
            .put(MemoryRecord {
                id: Uuid::new_v4(),
                tier: MemoryTier::Working,
                owner: me.clone(),
                text: "working memory".into(),
                embedding: Vec::new(),
                metadata: serde_json::json!({}),
                created_at: 10,
                parent: None,
            })
            .await
            .unwrap();
        s.memory
            .put(MemoryRecord {
                id: Uuid::new_v4(),
                tier: MemoryTier::Episodic,
                owner: me,
                text: "episodic memory".into(),
                embedding: Vec::new(),
                metadata: serde_json::json!({}),
                created_at: 10,
                parent: None,
            })
            .await
            .unwrap();
        s.op_respond(Request::GrantCapability {
            action: "memory.read".into(),
            scope: Some(serde_json::json!({
                "version": 1,
                "tiers": ["working"],
                "apply": false
            })),
            expires_at: None,
        })
        .await;

        let resp = s
            .op_respond(Request::RecentMemory {
                tier: None,
                limit: 10,
                prefer_stream: None,
            })
            .await;
        match resp {
            Response::Memories { records } => {
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].tier, MemoryTier::Working);
            }
            other => panic!("unexpected: {other:?}"),
        }

        let rejected = s
            .op_respond(Request::RecentMemory {
                tier: Some(MemoryTier::Episodic),
                limit: 10,
                prefer_stream: None,
            })
            .await;
        match rejected {
            Response::Error { message } => assert!(message.contains("capability scope")),
            other => panic!("unexpected: {other:?}"),
        }
        let events = s.audit.recent(10).await.unwrap();
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AuditKind::CapabilityScopeRejected { action, .. } if action == "memory.read"
        )));
    }

    #[tokio::test]
    async fn search_memory_filters_owner_and_scope() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let alien = AgentId::new("alice@local", [9u8; 32]);
        let embedding = s.embedder.embed("note").await.unwrap();
        s.memory
            .put(MemoryRecord {
                id: Uuid::new_v4(),
                tier: MemoryTier::Working,
                owner: me,
                text: "operator working note".into(),
                embedding: embedding.clone(),
                metadata: serde_json::json!({}),
                created_at: 10,
                parent: None,
            })
            .await
            .unwrap();
        s.memory
            .put(MemoryRecord {
                id: Uuid::new_v4(),
                tier: MemoryTier::Working,
                owner: alien,
                text: "alien working note".into(),
                embedding: embedding.clone(),
                metadata: serde_json::json!({}),
                created_at: 10,
                parent: None,
            })
            .await
            .unwrap();
        s.memory
            .put(MemoryRecord {
                id: Uuid::new_v4(),
                tier: MemoryTier::Episodic,
                owner: s.identity.agent_id(),
                text: "operator episodic note".into(),
                embedding,
                metadata: serde_json::json!({}),
                created_at: 10,
                parent: None,
            })
            .await
            .unwrap();
        s.op_respond(Request::GrantCapability {
            action: "memory.read".into(),
            scope: Some(serde_json::json!({
                "version": 1,
                "tiers": ["working"],
                "apply": false
            })),
            expires_at: None,
        })
        .await;

        let resp = s
            .op_respond(Request::SearchMemory {
                query: "note".into(),
                tier: None,
                limit: 10,
                min_relevance: None,
            })
            .await;
        match resp {
            Response::Memories { records } => {
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].text, "operator working note");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_memory_v0_operator_sees_own_records() {
        let s = server_with(
            vec![stub_card("research", vec!["tool.web_search"])],
            "summary",
        );
        grant_action(&s, "tool.web_search").await;
        grant_action(&s, "memory.write").await;
        grant_action(&s, "memory.read").await;
        s.op_respond(Request::SubmitIntent {
            text: "find recent papers on agent memory".into(),
            prefer_stream: None,
        })
        .await;
        let me = s.identity.agent_id();
        let resp = s
            .op_respond(Request::RecentMemory {
                tier: None,
                limit: 10,
                prefer_stream: None,
            })
            .await;
        match resp {
            Response::Memories { records } => {
                assert!(!records.is_empty(), "operator should see their own rows");
                assert!(
                    records.iter().all(|r| r.owner.pubkey == me.pubkey),
                    "filter must keep operator's rows"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_receipts_scrubs_other_peers_receipts() {
        let s = server_with(vec![], "");
        let alien = AgentId::new("alice@local", [9u8; 32]);
        s.settlement
            .record(SettlementReceipt {
                id: Uuid::new_v4(),
                payer: alien,
                resource: ResourceKind::Memory,
                memory_record_id: None,
                credits_consumed: 7,
                settled_at: epoch_ms(),
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
            .unwrap();
        let me = s.identity.agent_id();
        s.settlement
            .record(SettlementReceipt {
                id: Uuid::new_v4(),
                payer: me.clone(),
                resource: ResourceKind::Memory,
                memory_record_id: None,
                credits_consumed: 3,
                settled_at: epoch_ms(),
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
            .unwrap();
        grant_action(&s, "chain.receipts").await;
        let resp = s
            .op_respond(Request::RecentReceipts {
                limit: 100,
                since_ms: None,
            })
            .await;
        match resp {
            Response::Receipts { receipts } => {
                assert!(
                    receipts.iter().all(|r| r.payer.pubkey == me.pubkey),
                    "every returned receipt must have the requesting peer as payer"
                );
                assert!(
                    !receipts.iter().any(|r| r.credits_consumed == 7),
                    "alien receipt amount leaked through filter"
                );
                assert!(
                    receipts.iter().any(|r| r.credits_consumed == 3),
                    "operator's own receipt should still be visible"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_receipts_since_ms_drops_older_receipts_before_limit() {
        let s = server_with(vec![], "");
        let mine = s.identity.agent_id();
        for (ts, credits) in [
            (1_000u64, 1u64),
            (2_000, 2),
            (3_000, 3),
            (4_000, 4),
            (5_000, 5),
        ] {
            s.settlement
                .record(SettlementReceipt {
                    id: Uuid::new_v4(),
                    payer: mine.clone(),
                    resource: ResourceKind::Memory,
                    memory_record_id: None,
                    credits_consumed: credits,
                    settled_at: ts,
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
                .unwrap();
        }
        grant_action(&s, "chain.receipts").await;

        let resp = s
            .op_respond(Request::RecentReceipts {
                limit: 10,
                since_ms: Some(3_000),
            })
            .await;
        match resp {
            Response::Receipts { receipts } => {
                let timestamps: Vec<u64> = receipts.iter().map(|r| r.settled_at).collect();
                assert!(
                    timestamps.iter().all(|ts| *ts >= 3_000),
                    "since_ms must drop receipts below the threshold: timestamps={timestamps:?}",
                );
                assert!(
                    timestamps.contains(&3_000),
                    "boundary is inclusive at >=, so the receipt at the threshold must survive: timestamps={timestamps:?}",
                );
            }
            other => panic!("unexpected: {other:?}"),
        }

        let narrow = s
            .op_respond(Request::RecentReceipts {
                limit: 1,
                since_ms: Some(2_000),
            })
            .await;
        match narrow {
            Response::Receipts { receipts } => {
                let timestamps: Vec<u64> = receipts.iter().map(|r| r.settled_at).collect();
                assert_eq!(
                    timestamps,
                    vec![5_000],
                    "since_ms applies before limit truncation so the newest in-window receipt survives a tight --limit, not the oldest: receipts={receipts:?}",
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_receipts_v0_operator_sees_own() {
        let s = server_with(
            vec![stub_card("research", vec!["tool.web_search"])],
            "summary",
        );
        grant_action(&s, "tool.web_search").await;
        grant_action(&s, "memory.write").await;
        grant_action(&s, "chain.receipts").await;
        let resp = s
            .op_respond(Request::SubmitIntent {
                text: "find recent papers on agent memory".into(),
                prefer_stream: None,
            })
            .await;
        let intent_id = match resp {
            Response::IntentResult { intent_id, .. } => intent_id,
            other => panic!("unexpected: {other:?}"),
        };
        let me = s.identity.agent_id();
        let resp = s
            .op_respond(Request::RecentReceipts {
                limit: 10,
                since_ms: None,
            })
            .await;
        match resp {
            Response::Receipts { receipts } => {
                assert!(!receipts.is_empty(), "operator should see their own rows");
                assert!(
                    receipts.iter().all(|r| r.payer.pubkey == me.pubkey),
                    "filter must keep operator's receipts"
                );
                assert!(
                    receipts
                        .iter()
                        .filter(|r| r.resource == ResourceKind::Memory)
                        .all(|r| r.memory_record_id == Some(intent_id)),
                    "memory receipts must carry the originating memory record id"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_receipts_rejects_without_chain_capability() {
        let s = server_with(vec![], "");
        let resp = s
            .op_respond(Request::RecentReceipts {
                limit: 10,
                since_ms: None,
            })
            .await;
        match resp {
            Response::Error { message } => assert!(message.contains("chain.receipts")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_receipts_rejects_scope_limit_exceeded_and_audits() {
        let s = server_with(vec![], "");
        grant_scoped_action(
            &s,
            "chain.receipts",
            serde_json::json!({
                "version": 1,
                "limit": 1
            }),
        )
        .await;
        let resp = s
            .op_respond(Request::RecentReceipts {
                limit: 2,
                since_ms: None,
            })
            .await;
        match resp {
            Response::Error { message } => assert!(message.contains("capability scope")),
            other => panic!("expected scope rejection, got {other:?}"),
        }
        assert!(s.audit.recent(50).await.unwrap().iter().any(|event| {
            matches!(
                &event.kind,
                AuditKind::CapabilityScopeRejected { action, .. } if action == "chain.receipts"
            )
        }));
    }

    #[tokio::test]
    async fn receipt_batches_rejects_scope_limit_exceeded_and_audits() {
        let s = server_with(vec![], "");
        grant_scoped_action(
            &s,
            "chain.batches",
            serde_json::json!({
                "version": 1,
                "limit": 1
            }),
        )
        .await;
        let resp = s.op_respond(Request::ReceiptBatches { limit: 2 }).await;
        match resp {
            Response::Error { message } => assert!(message.contains("capability scope")),
            other => panic!("expected scope rejection, got {other:?}"),
        }
        assert!(s.audit.recent(50).await.unwrap().iter().any(|event| {
            matches!(
                &event.kind,
                AuditKind::CapabilityScopeRejected { action, .. } if action == "chain.batches"
            )
        }));
    }

    #[tokio::test]
    async fn flush_receipts_rejects_scope_limit_exceeded_and_audits() {
        let s = server_with(vec![], "");
        grant_scoped_action(
            &s,
            "chain.flush",
            serde_json::json!({
                "version": 1,
                "limit": 1
            }),
        )
        .await;
        let resp = s.op_respond(Request::FlushReceipts { limit: 2 }).await;
        match resp {
            Response::Error { message } => assert!(message.contains("capability scope")),
            other => panic!("expected scope rejection, got {other:?}"),
        }
        assert!(s.audit.recent(50).await.unwrap().iter().any(|event| {
            matches!(
                &event.kind,
                AuditKind::CapabilityScopeRejected { action, .. } if action == "chain.flush"
            )
        }));
    }

    // The COVNT mint is environment-level; receipts carry no mint field, so a
    // mint-bound chain.receipts/chain.batches scope can only be enforced at the
    // gather stage. With COVNT_MINT unset in tests the gathered mint is "", which
    // cannot satisfy a concrete mint scope — so the grant is rejected rather than
    // leaking receipts across mints (the previous unwrap_or(true) behavior).
    #[tokio::test]
    async fn recent_receipts_rejects_unmatched_mint_scope_and_audits() {
        let s = server_with(vec![], "");
        grant_scoped_action(
            &s,
            "chain.receipts",
            serde_json::json!({
                "version": 1,
                "mint": "Mint1111111111111111111111111111111111111111"
            }),
        )
        .await;
        let resp = s
            .op_respond(Request::RecentReceipts {
                limit: 5,
                since_ms: None,
            })
            .await;
        match resp {
            Response::Error { message } => assert!(message.contains("capability scope")),
            other => panic!("expected mint-scope rejection, got {other:?}"),
        }
        assert!(s.audit.recent(50).await.unwrap().iter().any(|event| {
            matches!(
                &event.kind,
                AuditKind::CapabilityScopeRejected { action, .. } if action == "chain.receipts"
            )
        }));
    }

    #[tokio::test]
    async fn receipt_batches_rejects_unmatched_mint_scope_and_audits() {
        let s = server_with(vec![], "");
        grant_scoped_action(
            &s,
            "chain.batches",
            serde_json::json!({
                "version": 1,
                "mint": "Mint1111111111111111111111111111111111111111"
            }),
        )
        .await;
        let resp = s.op_respond(Request::ReceiptBatches { limit: 5 }).await;
        match resp {
            Response::Error { message } => assert!(message.contains("capability scope")),
            other => panic!("expected mint-scope rejection, got {other:?}"),
        }
        assert!(s.audit.recent(50).await.unwrap().iter().any(|event| {
            matches!(
                &event.kind,
                AuditKind::CapabilityScopeRejected { action, .. } if action == "chain.batches"
            )
        }));
    }

    #[tokio::test]
    async fn recent_capabilities_scrubs_when_neither_subject_nor_granted_by() {
        let s = server_with(vec![], "");
        // Alien grants alien — peer is on neither side. Sign with a separate
        // identity so the wire-shape is realistic; the filter is on pubkeys,
        // not signature validity, so a hand-built capability is enough.
        let alien_grantor = LocalIdentity::generate("alice@local");
        let alien_subject = AgentId::new("bob@local", [8u8; 32]);
        let cap = covenant_types::Capability {
            subject: alien_subject,
            action: "tool.call.echo".into(),
            scope: serde_json::json!({}),
            granted_by: alien_grantor.agent_id(),
            expires_at: None,
        };
        let signed = sign_capability(cap, alien_grantor.signing_key());
        s.capabilities.record(signed).await.unwrap();

        let resp = s
            .op_respond(Request::RecentCapabilities { limit: 100 })
            .await;
        match resp {
            Response::Capabilities { capabilities } => {
                assert!(
                    capabilities.is_empty(),
                    "alien-to-alien capability leaked through filter: {capabilities:?}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_capabilities_visible_when_peer_is_subject() {
        let s = server_with(vec![], "");
        // Alien grants the operator: peer is the subject.
        let alien_grantor = LocalIdentity::generate("alice@local");
        let me = s.identity.agent_id();
        let cap = covenant_types::Capability {
            subject: me.clone(),
            action: "tool.call.echo".into(),
            scope: serde_json::json!({}),
            granted_by: alien_grantor.agent_id(),
            expires_at: None,
        };
        let signed = sign_capability(cap, alien_grantor.signing_key());
        s.capabilities.record(signed).await.unwrap();

        let resp = s
            .op_respond(Request::RecentCapabilities { limit: 100 })
            .await;
        match resp {
            Response::Capabilities { capabilities } => {
                assert_eq!(capabilities.len(), 1);
                assert_eq!(capabilities[0].capability.subject.pubkey, me.pubkey);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_capabilities_visible_when_peer_is_granted_by() {
        let s = server_with(vec![], "");
        // Operator grants an alien: peer is `granted_by`. Sign with the
        // operator's own key so the on-disk shape is the real grant path.
        let alien_subject = AgentId::new("bob@local", [8u8; 32]);
        let me = s.identity.agent_id();
        let cap = covenant_types::Capability {
            subject: alien_subject,
            action: "tool.call.echo".into(),
            scope: serde_json::json!({}),
            granted_by: me.clone(),
            expires_at: None,
        };
        let signed = sign_capability(cap, s.identity.signing_key());
        s.capabilities.record(signed).await.unwrap();

        let resp = s
            .op_respond(Request::RecentCapabilities { limit: 100 })
            .await;
        match resp {
            Response::Capabilities { capabilities } => {
                assert_eq!(capabilities.len(), 1);
                assert_eq!(capabilities[0].capability.granted_by.pubkey, me.pubkey);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_capabilities_v0_operator_sees_own_grants() {
        let s = server_with(vec![], "");
        s.op_respond(Request::GrantCapability {
            action: "tool.call.echo".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        let me = s.identity.agent_id();
        let resp = s
            .op_respond(Request::RecentCapabilities { limit: 10 })
            .await;
        match resp {
            Response::Capabilities { capabilities } => {
                assert_eq!(capabilities.len(), 1);
                let c = &capabilities[0].capability;
                assert_eq!(c.subject.pubkey, me.pubkey);
                assert_eq!(c.granted_by.pubkey, me.pubkey);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_a2a_tasks_scrubs_when_peer_is_neither_side() {
        let s = server_with(vec![], "");
        let alien_a = AgentId::new("alice@local", [9u8; 32]);
        let alien_b = AgentId::new("bob@local", [8u8; 32]);
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: alien_a,
            recipient: alien_b,
            intent_text: "alien-to-alien".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        s.mailbox.send_task(task).await.unwrap();
        let resp = s.op_respond(Request::RecentA2ATasks { limit: 100 }).await;
        match resp {
            Response::A2ATasks { tasks } => {
                assert!(
                    tasks.is_empty(),
                    "task with neither sender nor recipient match leaked: {tasks:?}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_a2a_tasks_visible_when_peer_is_sender() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let alien_recipient = AgentId::new("bob@local", [8u8; 32]);
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: me.clone(),
            recipient: alien_recipient,
            intent_text: "outbound".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        s.mailbox.send_task(task.clone()).await.unwrap();
        let resp = s.op_respond(Request::RecentA2ATasks { limit: 100 }).await;
        match resp {
            Response::A2ATasks { tasks } => {
                assert_eq!(tasks.len(), 1);
                assert_eq!(tasks[0].id, task.id);
                assert_eq!(tasks[0].sender.pubkey, me.pubkey);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_a2a_tasks_visible_when_peer_is_recipient() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let alien_sender = AgentId::new("alice@local", [9u8; 32]);
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: alien_sender,
            recipient: me.clone(),
            intent_text: "inbound".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        s.mailbox.send_task(task.clone()).await.unwrap();
        let resp = s.op_respond(Request::RecentA2ATasks { limit: 100 }).await;
        match resp {
            Response::A2ATasks { tasks } => {
                assert_eq!(tasks.len(), 1);
                assert_eq!(tasks[0].id, task.id);
                assert_eq!(tasks[0].recipient.pubkey, me.pubkey);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_a2a_results_scrubs_when_lookup_returns_other_peer() {
        let s = server_with(vec![], "");
        // Alien-sent task; result posted against it. Operator must not see
        // the result because `lookup_task_sender` returns the alien.
        let alien = AgentId::new("alice@local", [9u8; 32]);
        let alien_recipient = AgentId::new("bob@local", [8u8; 32]);
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: alien,
            recipient: alien_recipient,
            intent_text: "alien-task".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        s.mailbox.send_task(task.clone()).await.unwrap();
        let result = covenant_a2a::A2ATaskResult::ok(
            task.id,
            vec![covenant_mcp::Content::text("alien-result")],
        );
        s.mailbox.send_result(result).await.unwrap();

        let resp = s.op_respond(Request::RecentA2AResults { limit: 100 }).await;
        match resp {
            Response::A2AResults { results } => {
                assert!(
                    results.is_empty(),
                    "result for alien-sent task leaked: {results:?}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_a2a_results_visible_when_lookup_returns_peer() {
        let s = server_with(vec![], "");
        let me = s.identity.agent_id();
        let alien_recipient = AgentId::new("bob@local", [8u8; 32]);
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: me.clone(),
            recipient: alien_recipient,
            intent_text: "operator-task".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        s.mailbox.send_task(task.clone()).await.unwrap();
        let result = covenant_a2a::A2ATaskResult::ok(
            task.id,
            vec![covenant_mcp::Content::text("operator-result")],
        );
        s.mailbox.send_result(result.clone()).await.unwrap();

        let resp = s.op_respond(Request::RecentA2AResults { limit: 100 }).await;
        match resp {
            Response::A2AResults { results } => {
                assert_eq!(results.len(), 1);
                assert_eq!(results[0].task_id, task.id);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn ignore_check_returns_matched_pattern() {
        let ignore = IgnoreSet::parse("**/*.pem\n");
        let s = server_with_ignore(vec![], "echo", ignore);
        let resp = s
            .op_respond(Request::IgnoreCheck {
                text: "load /etc/ssl/server.pem please".into(),
            })
            .await;
        match resp {
            Response::IgnoreReport {
                ignored,
                matched_pattern,
                rules_loaded,
            } => {
                assert!(ignored);
                assert_eq!(matched_pattern.as_deref(), Some("**/*.pem"));
                assert_eq!(rules_loaded, 1);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    fn stub_card_with_budget(id: &str, capabilities: Vec<&str>, credits: u64) -> AgentCard {
        let toml = format!(
            r#"
[agent]
id = "{id}"
name = "{id}"
version = "0.0.1"
runtime = "rust-bin"
entry = "./fake"

[capabilities]
required = {caps:?}

[settlement]
budget_credits_per_hour = {credits}
"#,
            caps = capabilities
        );
        let m = Manifest::parse(&toml).unwrap();
        AgentCard::from_manifest_and_dir(m, PathBuf::from("/tmp/nope"))
    }

    /// Register-then-dispatch path: capacity = 10, one debit lands, token
    /// count drops to 9, dispatch returns the runner's mocked output.
    #[tokio::test]
    async fn dispatch_passes_when_budget_healthy() {
        let s = server_with(
            vec![stub_card_with_budget(
                "research",
                vec!["tool.web_search"],
                10,
            )],
            "mocked summary",
        );
        s.register_agent_budgets().await.unwrap();
        s.op_respond(Request::GrantCapability {
            action: "tool.web_search".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        grant_action(&s, "memory.write").await;
        let resp = s
            .op_respond(Request::SubmitIntent {
                text: "find recent papers on agent memory".into(),
                prefer_stream: None,
            })
            .await;
        match resp {
            Response::IntentResult { text, .. } => assert_eq!(text, "mocked summary"),
            other => panic!("unexpected: {other:?}"),
        }
        let synth = agent_id_for_card(&stub_card_with_budget(
            "research",
            vec!["tool.web_search"],
            10,
        ));
        assert_eq!(s.budget.tokens_remaining(&synth).await.unwrap(), 9);
    }

    /// Capacity = 1, two dispatches: the second is rejected with
    /// `Response::Error`, leaves a `BudgetExhausted` row in the audit log,
    /// and writes no memory record / receipt for the rejected attempt.
    #[tokio::test]
    async fn dispatch_rejects_and_audits_when_budget_exhausted() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let memory = Arc::new(InMemoryStore::new());
        let settlement = Arc::new(InMemorySettlement::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![stub_card_with_budget(
                "research",
                vec!["tool.web_search"],
                1,
            )])),
            Arc::new(MockRunner::new("mocked summary")),
            memory.clone(),
            settlement.clone(),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        s.register_agent_budgets().await.unwrap();
        s.op_respond(Request::GrantCapability {
            action: "tool.web_search".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        grant_action(&s, "memory.write").await;

        let first = s
            .op_respond(Request::SubmitIntent {
                text: "find recent papers".into(),
                prefer_stream: None,
            })
            .await;
        assert!(matches!(first, Response::IntentResult { .. }));
        let memory_after_first = memory.recent(None, 10).await.unwrap().len();
        let receipts_after_first = settlement.recent(10).await.unwrap().len();

        let second = s
            .op_respond(Request::SubmitIntent {
                text: "find more recent papers".into(),
                prefer_stream: None,
            })
            .await;
        match second {
            Response::Error { message } => {
                assert!(
                    message.contains("budget exhausted"),
                    "expected budget exhaustion message, got {message:?}"
                );
                assert!(message.contains("research"));
            }
            other => panic!("expected Error, got {other:?}"),
        }

        // Rejected dispatch must not have advanced memory or receipts.
        assert_eq!(
            memory.recent(None, 10).await.unwrap().len(),
            memory_after_first
        );
        assert_eq!(
            settlement.recent(10).await.unwrap().len(),
            receipts_after_first
        );

        let events = audit.recent(50).await.unwrap();
        let exhausted = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::BudgetExhausted { .. }))
            .expect("expected a BudgetExhausted audit event");
        // Pin the issuer attribution: the audit row's issuer is the
        // rejected caller (peer), not the synthesised agent. A future
        // refactor that flips it would silently re-key audit feeds.
        assert_eq!(exhausted.issuer.display, "user@local");
        match &exhausted.kind {
            AuditKind::BudgetExhausted {
                agent_display,
                intent_text,
                requested,
                tokens_remaining,
                ..
            } => {
                assert_eq!(agent_display, "research@agent");
                assert_eq!(*requested, 1);
                assert_eq!(*tokens_remaining, 0);
                // The audit row carries the rejected text so
                // `intents resume <id>` can re-dispatch from this row alone.
                assert_eq!(intent_text, "find more recent papers");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_budget_exhaustion_saves_checkpoint_and_resume_claims_once() {
        let dir = tempfile::tempdir().unwrap();
        let checkpoints = Arc::new(
            JsonlPauseCheckpointStore::open(dir.path().join("checkpoints.jsonl"))
                .await
                .unwrap(),
        );
        let card = stub_card_with_budget("research", vec!["tool.web_search"], 1);
        let agent = agent_id_for_card(&card);
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![card])),
            Arc::new(MockRunner::new("mocked summary")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        )
        .with_budget_checkpoints(checkpoints.clone());
        s.register_agent_budgets().await.unwrap();
        grant_action(&s, "tool.web_search").await;
        grant_action(&s, "memory.write").await;

        assert!(matches!(
            s.op_respond(Request::SubmitIntent {
                text: "find recent papers".into(),
                prefer_stream: None,
            })
            .await,
            Response::IntentResult { .. }
        ));
        let second_text = "find more recent papers";
        assert!(matches!(
            s.op_respond(Request::SubmitIntent {
                text: second_text.into(),
                prefer_stream: None,
            })
            .await,
            Response::Error { .. }
        ));

        let events = audit.recent(50).await.unwrap();
        let exhausted_intent = events
            .iter()
            .find_map(|event| match &event.kind {
                AuditKind::BudgetExhausted { intent_id, .. } => Some(*intent_id),
                _ => None,
            })
            .expect("expected budget exhaustion row");
        let saved = checkpoints
            .active_pause(exhausted_intent, &agent)
            .await
            .expect("budget exhaustion should save a pause checkpoint");
        assert_eq!(saved.reason, BudgetPauseReason::BudgetExhausted);
        assert_eq!(saved.resume_state["intent_text"], second_text);

        let first_resume = s
            .op_respond(Request::ResumeIntent {
                intent_id: exhausted_intent,
            })
            .await;
        assert!(
            matches!(first_resume, Response::Error { ref message } if message.contains("budget exhausted")),
            "resume should redispatch and hit the still-empty bucket, got {first_resume:?}"
        );
        assert!(
            checkpoints
                .active_pause(exhausted_intent, &agent)
                .await
                .is_none(),
            "resume claim should consume the original checkpoint"
        );

        let duplicate = s
            .op_respond(Request::ResumeIntent {
                intent_id: exhausted_intent,
            })
            .await;
        assert!(
            matches!(duplicate, Response::Error { ref message } if message.contains("already claimed")),
            "duplicate resume must not redispatch a claimed checkpoint, got {duplicate:?}"
        );
    }

    #[tokio::test]
    async fn shutdown_saves_active_budget_checkpoints_once() {
        let dir = tempfile::tempdir().unwrap();
        let checkpoints = Arc::new(
            JsonlPauseCheckpointStore::open(dir.path().join("checkpoints.jsonl"))
                .await
                .unwrap(),
        );
        let card = stub_card_with_budget("research", vec!["tool.web_search"], 10);
        let agent = agent_id_for_card(&card);
        let s = Server::new(
            Arc::new(Router::from_cards(vec![card])),
            Arc::new(MockRunner::new("mocked summary")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            Arc::new(covenant_audit::InMemoryAuditLog::new()),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        )
        .with_budget_checkpoints(checkpoints.clone());

        let intent_id = Uuid::new_v4();
        let checkpoint = budget_pause_checkpoint(
            intent_id,
            agent.clone(),
            BudgetPauseReason::Shutdown,
            1,
            9,
            epoch_ms(),
            epoch_ms(),
            budget_resume_state("find recent papers", "research", "active_dispatch"),
        );
        s.active_budget_pauses
            .lock()
            .await
            .insert(intent_id, checkpoint.clone());

        assert_eq!(s.save_shutdown_budget_checkpoints().await, 1);
        assert_eq!(
            checkpoints.active_pause(intent_id, &agent).await,
            Some(checkpoint)
        );
        assert_eq!(s.save_shutdown_budget_checkpoints().await, 0);
    }

    /// Phase-0 manifests have `budget_credits_per_hour = 0`. The daemon
    /// must keep dispatching them — register_agent_budgets seeds capacity
    /// 0, the bucket has no tokens, and try_debit returns Exhausted; the
    /// dispatch path treats credit-0 agents as "no enforcement requested"
    /// and skips the debit. (Equivalent test: card with budget = 0 plus
    /// no register_agent_budgets call, exercising the NoCapacity warn-
    /// and-pass branch.)
    /// When the manifest opts in to budget but `register_agent_budgets`
    /// was never called, dispatch falls into the NoCapacity arm. v0
    /// still passes the dispatch through but records a `BudgetUnseeded`
    /// audit row so the bypass is visible.
    #[tokio::test]
    async fn dispatch_audits_unseeded_when_manifest_opts_in_but_bucket_missing() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![stub_card_with_budget(
                "research",
                vec!["tool.web_search"],
                10,
            )])),
            Arc::new(MockRunner::new("mocked summary")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        // Skip register_agent_budgets — the bucket is absent.
        s.op_respond(Request::GrantCapability {
            action: "tool.web_search".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        grant_action(&s, "memory.write").await;
        let resp = s
            .op_respond(Request::SubmitIntent {
                text: "find recent papers".into(),
                prefer_stream: None,
            })
            .await;
        // Dispatch passes (v0 fail-open).
        assert!(matches!(resp, Response::IntentResult { .. }));
        let events = audit.recent(50).await.unwrap();
        let unseeded = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::BudgetUnseeded { .. }))
            .expect("expected a BudgetUnseeded audit event");
        match &unseeded.kind {
            AuditKind::BudgetUnseeded {
                agent_display,
                requested,
                ..
            } => {
                assert_eq!(agent_display, "research@agent");
                assert_eq!(*requested, 1);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn a2a_repair_action_pins_each_repair_command_variant() {
        let requeue = covenant_a2a::A2ARepairCommand::Requeue {
            lease_id: Some(Uuid::new_v4()),
            duplicate_risk: covenant_a2a::A2ADuplicateRisk::Idempotent,
        };
        let force_error = covenant_a2a::A2ARepairCommand::ForceError {
            lease_id: Some(Uuid::new_v4()),
            message: "stuck on dependency".into(),
        };
        assert_eq!(a2a_repair_action(&requeue), "requeue");
        assert_eq!(a2a_repair_action(&force_error), "force_error");
    }

    #[test]
    fn a2a_repair_lease_id_pins_each_arm() {
        let requeue_lease = Uuid::new_v4();
        let force_lease = Uuid::new_v4();
        let requeue_with = covenant_a2a::A2ARepairCommand::Requeue {
            lease_id: Some(requeue_lease),
            duplicate_risk: covenant_a2a::A2ADuplicateRisk::Idempotent,
        };
        let requeue_without = covenant_a2a::A2ARepairCommand::Requeue {
            lease_id: None,
            duplicate_risk: covenant_a2a::A2ADuplicateRisk::Idempotent,
        };
        let force_with = covenant_a2a::A2ARepairCommand::ForceError {
            lease_id: Some(force_lease),
            message: "stuck".into(),
        };
        let force_without = covenant_a2a::A2ARepairCommand::ForceError {
            lease_id: None,
            message: "stuck".into(),
        };
        assert_eq!(a2a_repair_lease_id(&requeue_with), Some(requeue_lease));
        assert_eq!(a2a_repair_lease_id(&requeue_without), None);
        assert_eq!(a2a_repair_lease_id(&force_with), Some(force_lease));
        assert_eq!(a2a_repair_lease_id(&force_without), None);
    }

    #[test]
    fn settlement_resource_name_pins_each_resource_kind_variant() {
        assert_eq!(settlement_resource_name(ResourceKind::Compute), "compute");
        assert_eq!(settlement_resource_name(ResourceKind::Memory), "memory");
        assert_eq!(settlement_resource_name(ResourceKind::Tool), "tool");
        assert_eq!(settlement_resource_name(ResourceKind::Message), "message");
        assert_eq!(
            settlement_resource_name(ResourceKind::Registration),
            "registration"
        );
    }

    #[test]
    fn memory_tier_name_pins_each_tier_variant() {
        assert_eq!(memory_tier_name(MemoryTier::Working), "working");
        assert_eq!(memory_tier_name(MemoryTier::Episodic), "episodic");
        assert_eq!(memory_tier_name(MemoryTier::LongTerm), "longterm");
    }

    #[test]
    fn audit_kind_requires_persistence_pins_each_must_persist_kind() {
        let must_persist = [
            AuditKind::AuthenticationFailed {
                transport: "ipc".into(),
                reason: "missing token".into(),
            },
            AuditKind::OperatorTokenRotationRejected {
                peer_display: "intruder@local".into(),
                peer_pubkey_b58: "111".into(),
            },
            AuditKind::OperatorPeersListRejected {
                peer_display: "intruder@local".into(),
                peer_pubkey_b58: "222".into(),
            },
            AuditKind::OperatorPeerRevokeRejected {
                peer_display: "intruder@local".into(),
                peer_pubkey_b58: "333".into(),
            },
            AuditKind::A2ASenderMismatch {
                peer_display: "claimed@local".into(),
                claimed_sender_display: "spoofed@local".into(),
            },
            AuditKind::A2ARecipientRejected {
                sender_display: "sender@local".into(),
                recipient_display: "recipient@local".into(),
                action: "a2a.recv".into(),
            },
            AuditKind::CapabilityRevokeRejected {
                signature_b58: "sig".into(),
                reason: "not owner".into(),
            },
            AuditKind::BudgetExhausted {
                agent_display: "research@agent".into(),
                intent_id: Uuid::new_v4(),
                intent_text: "find papers".into(),
                requested: 5,
                tokens_remaining: 0,
                refill_eta_ms: 1000,
            },
        ];
        for kind in &must_persist {
            assert!(
                audit_kind_requires_persistence(kind),
                "expected {:?} to be must-persist",
                kind,
            );
        }

        let best_effort = AuditKind::IntentDispatched {
            intent_id: Uuid::new_v4(),
            intent_text: "find papers".into(),
            matched_agent: Some("research@agent".into()),
            result_hash_hex: "deadbeef".into(),
            status: "ok".into(),
        };
        assert!(!audit_kind_requires_persistence(&best_effort));
    }

    #[test]
    fn a2a_entry_matches_state_pins_filter_matrix() {
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: AgentId::new("sender@local", [1u8; 32]),
            recipient: AgentId::new("recipient@local", [2u8; 32]),
            intent_text: "anything".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        let queued = covenant_a2a::A2ATaskQueueEntry {
            state: covenant_a2a::A2ATaskQueueState::Queued,
            task: task.clone(),
            lease_id: None,
            leased_to: None,
            leased_at_ms: None,
            attempt: 0,
        };
        let in_flight = covenant_a2a::A2ATaskQueueEntry {
            state: covenant_a2a::A2ATaskQueueState::InFlight,
            task,
            lease_id: Some(Uuid::new_v4()),
            leased_to: Some(AgentId::new("leasee@local", [3u8; 32])),
            leased_at_ms: Some(0),
            attempt: 1,
        };
        assert!(a2a_entry_matches_state(&queued, None));
        assert!(a2a_entry_matches_state(&in_flight, None));
        assert!(a2a_entry_matches_state(
            &queued,
            Some(covenant_a2a::A2ATaskQueueState::Queued)
        ));
        assert!(!a2a_entry_matches_state(
            &in_flight,
            Some(covenant_a2a::A2ATaskQueueState::Queued)
        ));
        assert!(!a2a_entry_matches_state(
            &queued,
            Some(covenant_a2a::A2ATaskQueueState::InFlight)
        ));
        assert!(a2a_entry_matches_state(
            &in_flight,
            Some(covenant_a2a::A2ATaskQueueState::InFlight)
        ));
    }

    #[test]
    fn memory_repair_mode_pins_each_mode_variant() {
        assert_eq!(memory_repair_mode(MemoryRepairMode::DryRun), "dry_run");
        assert_eq!(memory_repair_mode(MemoryRepairMode::Apply), "apply");
    }

    #[test]
    fn memory_repair_id_pins_each_repair_command_variant() {
        let detach_id = Uuid::new_v4();
        let delete_id = Uuid::new_v4();
        let backfill_id = Uuid::new_v4();
        let detach = MemoryRepairCommand::DetachParent {
            id: detach_id,
            expected_parent: Some(Uuid::new_v4()),
        };
        let delete = MemoryRepairCommand::DeleteRecord { id: delete_id };
        let backfill = MemoryRepairCommand::BackfillProvenance {
            id: backfill_id,
            provenance: serde_json::json!({}),
        };
        assert_eq!(memory_repair_id(&detach), detach_id);
        assert_eq!(memory_repair_id(&delete), delete_id);
        assert_eq!(memory_repair_id(&backfill), backfill_id);
    }

    #[test]
    fn memory_repair_action_pins_each_repair_command_variant() {
        let detach = MemoryRepairCommand::DetachParent {
            id: Uuid::new_v4(),
            expected_parent: Some(Uuid::new_v4()),
        };
        let delete = MemoryRepairCommand::DeleteRecord { id: Uuid::new_v4() };
        let backfill = MemoryRepairCommand::BackfillProvenance {
            id: Uuid::new_v4(),
            provenance: serde_json::json!({"source": "audit-row"}),
        };
        assert_eq!(memory_repair_action(&detach), "detach_parent");
        assert_eq!(memory_repair_action(&delete), "delete_record");
        assert_eq!(memory_repair_action(&backfill), "backfill_provenance");
    }

    #[test]
    fn covenant_home_pins_env_precedence() {
        use std::sync::Mutex;

        static HOME_ENV_LOCK: Mutex<()> = Mutex::new(());
        let _guard = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let saved_covenant_home = std::env::var("COVENANT_HOME").ok();
        let saved_home = std::env::var("HOME").ok();

        std::env::set_var("COVENANT_HOME", "/explicit/path");
        std::env::set_var("HOME", "/should/be/ignored");
        assert_eq!(
            covenant_home().unwrap(),
            PathBuf::from("/explicit/path"),
            "COVENANT_HOME must win over HOME, with no .covenant suffix"
        );

        std::env::remove_var("COVENANT_HOME");
        std::env::set_var("HOME", "/home/u");
        assert_eq!(
            covenant_home().unwrap(),
            PathBuf::from("/home/u/.covenant"),
            "HOME fallback must join .covenant suffix"
        );

        std::env::remove_var("COVENANT_HOME");
        std::env::remove_var("HOME");
        let err = covenant_home().expect_err("missing HOME must fail");
        assert!(
            err.to_string().contains("HOME not set"),
            "expected 'HOME not set' context, got {err}"
        );

        match saved_covenant_home {
            Some(v) => std::env::set_var("COVENANT_HOME", v),
            None => std::env::remove_var("COVENANT_HOME"),
        }
        match saved_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn chain_status_from_env_pins_defaults_missing_and_ready() {
        use std::sync::Mutex;

        static CHAIN_ENV_LOCK: Mutex<()> = Mutex::new(());
        let _guard = CHAIN_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        const VARS: [&str; 5] = [
            "COVENANT_SOLANA_CLUSTER",
            "COVENANT_SOLANA_RPC_URL",
            "COVENANT_SOLANA_WS_URL",
            "COVENANT_PROTOCOL_PROGRAM_ID",
            "COVNT_MINT",
        ];
        let saved: Vec<(&str, Option<String>)> = VARS
            .iter()
            .map(|name| (*name, std::env::var(*name).ok()))
            .collect();

        for name in VARS {
            std::env::remove_var(name);
        }
        let bare = chain_status_from_env();
        assert_eq!(bare.cluster, "devnet", "cluster must default to devnet");
        assert_eq!(bare.rpc_url, None);
        assert_eq!(bare.ws_url, None);
        assert_eq!(bare.program_id, None);
        assert_eq!(bare.covnt_mint, None);
        assert!(!bare.ready, "no required vars set => ready must be false");
        assert_eq!(
            bare.missing,
            vec![
                "COVENANT_SOLANA_RPC_URL".to_string(),
                "COVENANT_PROTOCOL_PROGRAM_ID".to_string(),
                "COVNT_MINT".to_string(),
            ],
            "missing must enumerate exactly the three required env names in declaration order"
        );

        std::env::set_var("COVENANT_SOLANA_RPC_URL", "https://rpc.example/");
        std::env::set_var(
            "COVENANT_PROTOCOL_PROGRAM_ID",
            "cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y",
        );
        std::env::set_var("COVNT_MINT", "4uTpj4kb8r1NbMGbTwNKoDPvrPpevGNZN2hP4FWUW58E");
        let ready = chain_status_from_env();
        assert!(
            ready.ready,
            "all three required vars set => ready must be true"
        );
        assert!(ready.missing.is_empty(), "missing must be empty when ready");
        assert_eq!(ready.cluster, "devnet");
        assert_eq!(ready.rpc_url.as_deref(), Some("https://rpc.example/"));
        assert_eq!(
            ready.program_id.as_deref(),
            Some("cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y")
        );
        assert_eq!(
            ready.covnt_mint.as_deref(),
            Some("4uTpj4kb8r1NbMGbTwNKoDPvrPpevGNZN2hP4FWUW58E")
        );

        for name in VARS {
            std::env::remove_var(name);
        }
        std::env::set_var("COVENANT_SOLANA_CLUSTER", "mainnet");
        let cluster_only = chain_status_from_env();
        assert_eq!(cluster_only.cluster, "mainnet");
        assert!(
            !cluster_only.ready,
            "cluster alone is not sufficient for ready"
        );
        assert_eq!(cluster_only.missing.len(), 3);

        for (name, value) in saved {
            match value {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn read_operator_token_b58_pins_mode_trim_and_decode() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("operator.token");

        let original = PeerToken::generate();
        let b58 = original.to_b58();
        write_operator_token_0600(&path, &b58).expect("write valid token");

        let round_trip = read_operator_token_b58(&path).expect("read valid token");
        assert_eq!(round_trip.to_b58(), b58, "round-trip b58 must match");

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
            .expect("chmod 0o644 to force mode reject");
        let err_mode = read_operator_token_b58(&path).expect_err("0o644 must reject");
        assert_eq!(
            err_mode.kind(),
            std::io::ErrorKind::PermissionDenied,
            "mode-reject must forward PermissionDenied from require_operator_token_mode_0600, got {err_mode:?}"
        );

        fs::write(&path, b"not_b58_!!!\n").expect("rewrite with garbage");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("restore 0o600 so mode gate passes and decode is what fails");
        let err_decode = read_operator_token_b58(&path).expect_err("garbage b58 must reject");
        assert_eq!(
            err_decode.kind(),
            std::io::ErrorKind::InvalidData,
            "decode-reject must surface as InvalidData, got {err_decode:?}"
        );
        assert!(
            err_decode.to_string().contains("decode token"),
            "decode error must mention 'decode token', got {err_decode}"
        );

        let padded = format!("   {b58}\n\n");
        fs::write(&path, padded.as_bytes()).expect("rewrite with padded valid b58");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("chmod 0o600 after rewrite");
        let trimmed = read_operator_token_b58(&path)
            .expect("whitespace-padded valid b58 must round-trip via trim");
        assert_eq!(
            trimmed.to_b58(),
            b58,
            "trim contract must let padded b58 decode to the original token"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_operator_token_0600_pins_create_mode_and_overwrite() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("nested").join("operator.token");

        write_operator_token_0600(&path, "8a8a8a8a8a8a8a8a").expect("first write");
        assert!(
            path.parent().expect("parent").is_dir(),
            "parent dir created"
        );
        let bytes = fs::read(&path).expect("read after first write");
        assert_eq!(
            bytes, b"8a8a8a8a8a8a8a8a\n",
            "first write must produce b58 bytes followed by newline 0x0A"
        );
        let mode = fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode must be 0o600 after write");
        require_operator_token_mode_0600(&path)
            .expect("mode-0600 reader gate must accept after first write");

        write_operator_token_0600(&path, "DIFFERENTb58body").expect("second write");
        let bytes2 = fs::read(&path).expect("read after second write");
        assert_eq!(
            bytes2, b"DIFFERENTb58body\n",
            "second write must fully replace the file, no concatenation"
        );
        let mode2 = fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode2, 0o600, "mode must be 0o600 after overwrite");
        require_operator_token_mode_0600(&path)
            .expect("mode-0600 reader gate must accept after overwrite");
    }

    #[cfg(unix)]
    #[test]
    fn require_operator_token_mode_0600_pins_accept_reject_paths() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().expect("tempdir");

        let p0600 = dir.path().join("tok.0600");
        fs::write(&p0600, b"x").expect("write tok.0600");
        fs::set_permissions(&p0600, fs::Permissions::from_mode(0o600)).expect("chmod 0o600");
        require_operator_token_mode_0600(&p0600).expect("0o600 must pass");

        fs::set_permissions(&p0600, fs::Permissions::from_mode(0o400)).expect("chmod 0o400");
        require_operator_token_mode_0600(&p0600).expect("0o400 must pass");

        fs::set_permissions(&p0600, fs::Permissions::from_mode(0o000)).expect("chmod 0o000");
        require_operator_token_mode_0600(&p0600).expect("0o000 must pass");

        fs::set_permissions(&p0600, fs::Permissions::from_mode(0o600))
            .expect("chmod 0o600 (restore)");

        let p0640 = dir.path().join("tok.0640");
        fs::write(&p0640, b"x").expect("write tok.0640");
        fs::set_permissions(&p0640, fs::Permissions::from_mode(0o640)).expect("chmod 0o640");
        let err640 = require_operator_token_mode_0600(&p0640).expect_err("0o640 must reject");
        let msg640 = err640.to_string();
        assert!(
            msg640.contains("mode is") && msg640.contains("expected 0o600"),
            "0o640 reject missing expected fragments: {msg640}"
        );

        let p0604 = dir.path().join("tok.0604");
        fs::write(&p0604, b"x").expect("write tok.0604");
        fs::set_permissions(&p0604, fs::Permissions::from_mode(0o604)).expect("chmod 0o604");
        let err604 = require_operator_token_mode_0600(&p0604).expect_err("0o604 must reject");
        let msg604 = err604.to_string();
        assert!(
            msg604.contains("mode is") && msg604.contains("expected 0o600"),
            "0o604 reject missing expected fragments: {msg604}"
        );

        let link = dir.path().join("tok.link");
        std::os::unix::fs::symlink(&p0600, &link).expect("create symlink");
        let err_link = require_operator_token_mode_0600(&link)
            .expect_err("symlink must reject even when target is 0o600");
        let msg_link = err_link.to_string();
        assert!(
            msg_link.contains("symlink"),
            "symlink reject missing 'symlink': {msg_link}"
        );
    }

    #[test]
    fn a2a_auto_retry_scheduler_config_from_env_pins_env_to_field_mapping() {
        use std::sync::Mutex;

        static A2A_RETRY_ENV_LOCK: Mutex<()> = Mutex::new(());
        let _guard = A2A_RETRY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let names = [
            "COVENANT_A2A_AUTO_RETRY_SCHEDULER",
            "COVENANT_A2A_AUTO_RETRY_INTERVAL_MS",
            "COVENANT_A2A_AUTO_RETRY_MIN_LEASE_AGE_MS",
            "COVENANT_A2A_AUTO_RETRY_MAX_ATTEMPTS",
            "COVENANT_A2A_AUTO_RETRY_MAX_REQUEUES",
            "COVENANT_A2A_AUTO_RETRY_SCAN_LIMIT",
        ];
        let saved: Vec<Option<String>> = names.iter().map(|n| std::env::var(n).ok()).collect();
        for name in &names {
            std::env::remove_var(name);
        }

        let defaults = a2a_auto_retry_scheduler_config_from_env().unwrap();
        assert_eq!(defaults, A2AAutoRetrySchedulerConfig::default());
        assert!(!defaults.enabled);
        assert_eq!(defaults.interval_ms, 60_000);

        std::env::set_var("COVENANT_A2A_AUTO_RETRY_SCHEDULER", "true");
        std::env::set_var("COVENANT_A2A_AUTO_RETRY_INTERVAL_MS", "1234");
        std::env::set_var("COVENANT_A2A_AUTO_RETRY_MIN_LEASE_AGE_MS", "777");
        std::env::set_var("COVENANT_A2A_AUTO_RETRY_MAX_ATTEMPTS", "9");
        std::env::set_var("COVENANT_A2A_AUTO_RETRY_MAX_REQUEUES", "4");
        std::env::set_var("COVENANT_A2A_AUTO_RETRY_SCAN_LIMIT", "55");

        let full = a2a_auto_retry_scheduler_config_from_env().unwrap();
        assert!(full.enabled);
        assert!(full.policy.enabled);
        assert_eq!(full.interval_ms, 1234);
        assert_eq!(full.policy.min_lease_age_ms, 777);
        assert_eq!(full.policy.max_attempts, 9);
        assert_eq!(full.policy.max_requeues, 4);
        assert_eq!(full.policy.scan_limit, 55);

        for (name, value) in names.iter().zip(saved.iter()) {
            match value {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
    }

    #[test]
    fn a2a_auto_retry_scheduler_config_from_values_pins_defaults_and_overrides() {
        let default =
            a2a_auto_retry_scheduler_config_from_values(None, None, None, None, None, None)
                .unwrap();
        assert_eq!(default, A2AAutoRetrySchedulerConfig::default());
        assert!(!default.enabled);
        assert!(!default.policy.enabled);
        assert_eq!(default.interval_ms, 60_000);

        let enabled =
            a2a_auto_retry_scheduler_config_from_values(Some("true"), None, None, None, None, None)
                .unwrap();
        assert!(enabled.enabled);
        assert!(
            enabled.policy.enabled,
            "config.policy.enabled must mirror config.enabled"
        );

        let disabled = a2a_auto_retry_scheduler_config_from_values(
            Some("false"),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(!disabled.enabled);
        assert!(!disabled.policy.enabled);

        let zero_interval =
            a2a_auto_retry_scheduler_config_from_values(None, Some("0"), None, None, None, None)
                .unwrap_err()
                .to_string();
        assert!(
            zero_interval.contains("COVENANT_A2A_AUTO_RETRY_INTERVAL_MS must be greater than zero"),
            "expected zero-interval rejection with named-env context, got {zero_interval}"
        );

        let full = a2a_auto_retry_scheduler_config_from_values(
            Some("true"),
            Some("1000"),
            Some("500"),
            Some("7"),
            Some("3"),
            Some("50"),
        )
        .unwrap();
        assert!(full.enabled);
        assert!(full.policy.enabled);
        assert_eq!(full.interval_ms, 1000);
        assert_eq!(full.policy.min_lease_age_ms, 500);
        assert_eq!(full.policy.max_attempts, 7);
        assert_eq!(full.policy.max_requeues, 3);
        assert_eq!(full.policy.scan_limit, 50);
    }

    #[test]
    fn hermes_gateway_config_from_values_pins_env_to_struct_mapping() {
        // Unset / empty base_url → None: the daemon must not pretend a
        // Hermes gateway is configured when no env was set; otherwise a
        // hermes-runtime agent would silently dispatch against an empty
        // URL.
        assert!(hermes_gateway_config_from_values(None, None).is_none());
        assert!(hermes_gateway_config_from_values(Some("   "), Some("k")).is_none());

        // Trimmed base_url, key absent → no api_key on the struct.
        let bare = hermes_gateway_config_from_values(Some("  http://127.0.0.1:8642/v1 "), None)
            .expect("non-empty base_url must produce a config");
        assert_eq!(bare.base_url, "http://127.0.0.1:8642/v1");
        assert_eq!(bare.api_key, None);

        // Both fields set + trimmed.
        let with_key =
            hermes_gateway_config_from_values(Some("http://127.0.0.1:8642/v1"), Some(" key "))
                .unwrap();
        assert_eq!(with_key.api_key.as_deref(), Some("key"));

        // Whitespace-only api_key collapses to None, not Some(""): an
        // empty bearer would be sent on the wire and Hermes would
        // reject with 401 instead of falling back to anonymous-on-
        // loopback.
        let empty_key =
            hermes_gateway_config_from_values(Some("http://127.0.0.1:8642/v1"), Some("   "))
                .unwrap();
        assert_eq!(empty_key.api_key, None);
    }

    #[test]
    fn runtime_runner_from_config_pins_backend_dispatch() {
        let _: fn(Arc<covenant_runtime::SubprocessTracker>) -> covenant_runtime::SubprocessRunner =
            covenant_runtime::SubprocessRunner::with_tracker;
        let _: fn(PathBuf, PathBuf, PathBuf) -> covenant_runtime::GvisorRunner =
            |runsc, rootfs, scratch| {
                covenant_runtime::GvisorRunner::with_paths(runsc, rootfs, scratch)
            };

        let tracker = Arc::new(covenant_runtime::SubprocessTracker::new());
        let local_a =
            runtime_runner_from_config(&RuntimeRunnerConfig::TrustedLocal, tracker.clone());
        let local_b =
            runtime_runner_from_config(&RuntimeRunnerConfig::TrustedLocal, tracker.clone());
        assert!(
            !Arc::ptr_eq(&local_a, &local_b),
            "expected a fresh Arc per call; a singleton would mask a per-config swap"
        );
        assert!(Arc::strong_count(&local_a) >= 1);

        let gvisor = runtime_runner_from_config(
            &RuntimeRunnerConfig::LinuxGvisor {
                runsc_path: PathBuf::from("/usr/local/bin/runsc"),
                rootfs: PathBuf::from("/r"),
                scratch_root: PathBuf::from("/s"),
            },
            tracker.clone(),
        );
        assert!(Arc::strong_count(&gvisor) >= 1);
        assert!(
            !Arc::ptr_eq(&local_a, &gvisor),
            "TrustedLocal and LinuxGvisor must yield distinct allocations"
        );
    }

    #[test]
    fn runtime_runner_config_from_env_pins_env_to_arg_mapping() {
        use std::sync::Mutex;

        static RUNTIME_ENV_LOCK: Mutex<()> = Mutex::new(());
        let _guard = RUNTIME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let names = [
            "COVENANT_RUNTIME_BACKEND",
            "COVENANT_GVISOR_ROOTFS",
            "COVENANT_RUNSC",
            "COVENANT_GVISOR_SCRATCH",
        ];
        let saved: Vec<Option<String>> = names.iter().map(|n| std::env::var(n).ok()).collect();
        for name in &names {
            std::env::remove_var(name);
        }

        let home = Path::new("/tmp/runtime-env-pin");
        let defaults = runtime_runner_config_from_env(home).unwrap();
        assert_eq!(defaults, RuntimeRunnerConfig::TrustedLocal);

        std::env::set_var("COVENANT_RUNTIME_BACKEND", "linux-gvisor");
        std::env::set_var("COVENANT_GVISOR_ROOTFS", "/rfs");
        std::env::set_var("COVENANT_RUNSC", "/usr/bin/runsc");
        std::env::set_var("COVENANT_GVISOR_SCRATCH", "/scratch");

        let full = runtime_runner_config_from_env(home).unwrap();
        assert_eq!(
            full,
            RuntimeRunnerConfig::LinuxGvisor {
                runsc_path: PathBuf::from("/usr/bin/runsc"),
                rootfs: PathBuf::from("/rfs"),
                scratch_root: PathBuf::from("/scratch"),
            },
            "each env var must land in its named field",
        );

        std::env::remove_var("COVENANT_GVISOR_SCRATCH");
        let scratch_default = runtime_runner_config_from_env(home).unwrap();
        assert_eq!(
            scratch_default,
            RuntimeRunnerConfig::LinuxGvisor {
                runsc_path: PathBuf::from("/usr/bin/runsc"),
                rootfs: PathBuf::from("/rfs"),
                scratch_root: home.join("runtime").join("gvisor"),
            },
            "scratch_root must fall back to <home>/runtime/gvisor",
        );

        for (name, value) in names.iter().zip(saved.iter()) {
            match value {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
    }

    #[test]
    fn runtime_runner_config_from_values_pins_backend_matrix() {
        let home = Path::new("/tmp/h");

        for raw in [
            None,
            Some(""),
            Some("   "),
            Some("trusted-local"),
            Some(" trusted-local "),
        ] {
            let config = runtime_runner_config_from_values(home, raw, None, None, None).unwrap();
            assert_eq!(
                config,
                RuntimeRunnerConfig::TrustedLocal,
                "expected TrustedLocal for backend={raw:?}",
            );
        }

        let default_gvisor =
            runtime_runner_config_from_values(home, Some("linux-gvisor"), Some("/rfs"), None, None)
                .unwrap();
        assert_eq!(
            default_gvisor,
            RuntimeRunnerConfig::LinuxGvisor {
                runsc_path: PathBuf::from("runsc"),
                rootfs: PathBuf::from("/rfs"),
                scratch_root: home.join("runtime").join("gvisor"),
            },
        );

        let override_gvisor = runtime_runner_config_from_values(
            home,
            Some(" linux-gvisor "),
            Some(" /rfs "),
            Some(" /usr/bin/runsc "),
            Some(" /scratch "),
        )
        .unwrap();
        assert_eq!(
            override_gvisor,
            RuntimeRunnerConfig::LinuxGvisor {
                runsc_path: PathBuf::from("/usr/bin/runsc"),
                rootfs: PathBuf::from("/rfs"),
                scratch_root: PathBuf::from("/scratch"),
            },
            "expected trim on all four gvisor inputs",
        );

        let missing_rootfs =
            runtime_runner_config_from_values(home, Some("linux-gvisor"), None, None, None)
                .unwrap_err();
        assert!(
            missing_rootfs
                .to_string()
                .contains("COVENANT_GVISOR_ROOTFS is required"),
            "expected rootfs-required error, got {missing_rootfs}",
        );
        let empty_rootfs =
            runtime_runner_config_from_values(home, Some("linux-gvisor"), Some("   "), None, None)
                .unwrap_err();
        assert!(
            empty_rootfs
                .to_string()
                .contains("COVENANT_GVISOR_ROOTFS is required"),
            "expected whitespace-only rootfs to be treated as missing, got {empty_rootfs}",
        );

        let unsupported =
            runtime_runner_config_from_values(home, Some("docker"), None, None, None).unwrap_err();
        let msg = unsupported.to_string();
        assert!(
            msg.contains("unsupported COVENANT_RUNTIME_BACKEND")
                && msg.contains("expected trusted-local or linux-gvisor"),
            "expected unsupported-backend error with enumeration, got {msg}",
        );
    }

    #[test]
    fn parse_env_u32_and_usize_pin_trim_value_and_overflow() {
        assert_eq!(parse_env_u32("X", "7").unwrap(), 7);
        assert_eq!(parse_env_u32("X", " 8 ").unwrap(), 8);
        let bad_u32 = parse_env_u32("COVENANT_X", "notanint")
            .unwrap_err()
            .to_string();
        assert!(
            bad_u32.contains("COVENANT_X must be an integer"),
            "expected name-prefixed error context, got {bad_u32:?}"
        );
        let overflow = parse_env_u32("COVENANT_X", "4294967296")
            .unwrap_err()
            .to_string();
        assert!(
            overflow.contains("COVENANT_X must be an integer"),
            "u32::MAX+1 must overflow-reject with the same context, got {overflow:?}"
        );

        assert_eq!(parse_env_usize("X", "9").unwrap(), 9);
        assert_eq!(parse_env_usize("X", " 10 ").unwrap(), 10);
        let bad_usize = parse_env_usize("COVENANT_X", "notanint")
            .unwrap_err()
            .to_string();
        assert!(
            bad_usize.contains("COVENANT_X must be an integer"),
            "expected name-prefixed error context, got {bad_usize:?}"
        );
    }

    #[test]
    fn parse_env_u64_pins_trim_and_error_context() {
        assert_eq!(parse_env_u64("X", "123").unwrap(), 123);
        assert_eq!(parse_env_u64("X", " 456 ").unwrap(), 456);
        assert_eq!(parse_env_u64("X", "4294967296").unwrap(), 4_294_967_296u64);
        let err = parse_env_u64("COVENANT_X", "notanint")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("COVENANT_X must be an integer"),
            "expected name-prefixed error context, got {err:?}"
        );
        let empty_err = parse_env_u64("COVENANT_X", "").unwrap_err().to_string();
        assert!(
            empty_err.contains("COVENANT_X must be an integer"),
            "expected name-prefixed error context for empty input, got {empty_err:?}"
        );
    }

    #[test]
    fn audit_failure_response_pins_message_and_variant() {
        let err = AuditError::ChainCorruption {
            events: 0,
            chain: 0,
        };
        let response = audit_failure_response(err);
        match response {
            Response::Error { message } => {
                assert_eq!(message, "audit write failed; refusing to proceed");
            }
            other => panic!("expected Response::Error, got {other:?}"),
        }
    }

    #[test]
    fn budget_pause_checkpoint_pins_each_field() {
        let intent_id = Uuid::new_v4();
        let agent = AgentId::new("x@local", [7u8; 32]);
        let resume_state = budget_resume_state("intent", "x", "active_dispatch");
        let ck = budget_pause_checkpoint(
            intent_id,
            agent.clone(),
            BudgetPauseReason::BudgetExhausted,
            11,
            22,
            33,
            44,
            resume_state.clone(),
        );
        assert_eq!(ck.version, BudgetPauseCheckpoint::VERSION);
        assert_eq!(ck.intent_id, intent_id);
        assert_eq!(ck.agent, agent);
        assert_eq!(ck.reason, BudgetPauseReason::BudgetExhausted);
        assert_eq!(ck.requested_credits, 11);
        assert_eq!(ck.tokens_remaining, 22);
        assert_eq!(ck.refill_eta_ms, 33);
        assert_eq!(ck.saved_at_ms, 44);
        assert_eq!(ck.resume_state, resume_state);
    }

    #[test]
    fn budget_resume_state_pins_three_keys() {
        let active = budget_resume_state("compute something", "agentA", "active_dispatch");
        assert_eq!(active.len(), 3);
        assert_eq!(
            active["intent_text"],
            serde_json::json!("compute something")
        );
        assert_eq!(active["matched_agent"], serde_json::json!("agentA"));
        assert_eq!(active["source"], serde_json::json!("active_dispatch"));

        let exhausted = budget_resume_state("other intent", "agentB", "budget_exhausted");
        assert_eq!(exhausted.len(), 3);
        assert_eq!(exhausted["intent_text"], serde_json::json!("other intent"));
        assert_eq!(exhausted["matched_agent"], serde_json::json!("agentB"));
        assert_eq!(exhausted["source"], serde_json::json!("budget_exhausted"));
    }

    #[test]
    fn agent_id_for_card_pins_synth_display_and_pubkey_shape() {
        let card = stub_card("agentA", vec![]);
        let id = agent_id_for_card(&card);
        assert_eq!(id.display, "agentA@agent");
        assert_eq!(&id.pubkey[..6], b"agentA");
        assert_eq!(&id.pubkey[6..], &[0u8; 26]);

        let long = "a".repeat(40);
        let long_card = stub_card(&long, vec![]);
        let long_id = agent_id_for_card(&long_card);
        assert_eq!(long_id.pubkey, [b'a'; 32]);
        assert!(long_id.display.ends_with("@agent"));
        assert!(long_id.display.starts_with(&long));
    }

    #[test]
    fn token_b58_prefix_pins_first_six_chars() {
        let token = PeerToken::from_bytes([1u8; 32]);
        let prefix = token_b58_prefix(&token);
        let full = token.to_b58();
        assert_eq!(prefix.chars().count(), 6);
        assert_eq!(prefix, full.chars().take(6).collect::<String>());
    }

    #[test]
    fn parse_env_bool_pins_accepted_synonyms_and_reject_path() {
        for raw in ["1", "true", "yes", "on", "TRUE", " true ", "Yes", "ON"] {
            assert!(parse_env_bool(raw).unwrap(), "expected true for {raw:?}");
        }
        for raw in ["0", "false", "no", "off", "FALSE", " 0 ", "No", "OFF"] {
            assert!(!parse_env_bool(raw).unwrap(), "expected false for {raw:?}");
        }
        for raw in ["teu", "", "2", "maybe"] {
            let err = parse_env_bool(raw).unwrap_err().to_string();
            let needle = raw.trim().to_ascii_lowercase();
            assert!(
                err.contains(&needle),
                "expected error for {raw:?} to mention {needle:?}, got {err:?}"
            );
        }
    }

    #[test]
    fn memory_read_actions_pins_each_tier_and_none() {
        assert_eq!(
            memory_read_actions(Some(MemoryTier::Working)),
            vec!["memory.read".to_string(), "memory.read.working".to_string()]
        );
        assert_eq!(
            memory_read_actions(Some(MemoryTier::Episodic)),
            vec![
                "memory.read".to_string(),
                "memory.read.episodic".to_string()
            ]
        );
        assert_eq!(
            memory_read_actions(Some(MemoryTier::LongTerm)),
            vec![
                "memory.read".to_string(),
                "memory.read.longterm".to_string()
            ]
        );
        assert_eq!(
            memory_read_actions(None),
            vec![
                "memory.read".to_string(),
                "memory.read.working".to_string(),
                "memory.read.episodic".to_string(),
                "memory.read.longterm".to_string(),
            ]
        );
    }

    #[test]
    fn chain_receipt_allowed_pins_match_paths() {
        let receipt = SettlementReceipt {
            id: Uuid::new_v4(),
            payer: AgentId::new("payer@local", [2u8; 32]),
            resource: ResourceKind::Memory,
            memory_record_id: None,
            credits_consumed: 1,
            settled_at: 1_000,
            chain: None,
            cluster: Some("devnet".into()),
            batch_id: Some("batch-1".into()),
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        };
        let payer_b58 = receipt.payer.pubkey_base58();

        let allow_all = vec![("chain.receipts.read".to_string(), serde_json::json!({}))];
        assert!(chain_receipt_allowed(&allow_all, &receipt));

        let resource_match = vec![(
            "chain.receipts.read".to_string(),
            serde_json::json!({"version": 1, "resource": "memory"}),
        )];
        assert!(chain_receipt_allowed(&resource_match, &receipt));

        let cluster_match = vec![(
            "chain.receipts.read".to_string(),
            serde_json::json!({"version": 1, "cluster": "devnet"}),
        )];
        assert!(chain_receipt_allowed(&cluster_match, &receipt));

        let batch_match = vec![(
            "chain.receipts.read".to_string(),
            serde_json::json!({"version": 1, "batch_id": "batch-1"}),
        )];
        assert!(chain_receipt_allowed(&batch_match, &receipt));

        let payer_match = vec![(
            "chain.receipts.read".to_string(),
            serde_json::json!({"version": 1, "payer_pubkey_b58": payer_b58}),
        )];
        assert!(chain_receipt_allowed(&payer_match, &receipt));

        let denying_then_allow = vec![
            (
                "chain.receipts.read".to_string(),
                serde_json::json!({"version": 1, "resource": "compute"}),
            ),
            ("chain.receipts.read".to_string(), serde_json::json!({})),
        ];
        assert!(chain_receipt_allowed(&denying_then_allow, &receipt));

        let wrong_resource = vec![(
            "chain.receipts.read".to_string(),
            serde_json::json!({"version": 1, "resource": "compute"}),
        )];
        assert!(!chain_receipt_allowed(&wrong_resource, &receipt));

        let wrong_cluster = vec![(
            "chain.receipts.read".to_string(),
            serde_json::json!({"version": 1, "cluster": "mainnet"}),
        )];
        assert!(!chain_receipt_allowed(&wrong_cluster, &receipt));

        let wrong_batch = vec![(
            "chain.receipts.read".to_string(),
            serde_json::json!({"version": 1, "batch_id": "batch-2"}),
        )];
        assert!(!chain_receipt_allowed(&wrong_batch, &receipt));

        let invalid_scope = vec![("chain.receipts.read".to_string(), serde_json::json!(true))];
        assert!(!chain_receipt_allowed(&invalid_scope, &receipt));

        assert!(!chain_receipt_allowed(&[], &receipt));
    }

    #[test]
    fn memory_read_record_allowed_pins_scope_match_paths() {
        let record = MemoryRecord {
            id: Uuid::new_v4(),
            tier: MemoryTier::Working,
            owner: AgentId::new("owner@local", [1u8; 32]),
            text: "pinned".into(),
            embedding: vec![],
            metadata: serde_json::json!({}),
            created_at: 1_000,
            parent: None,
        };
        let record_id = record.id.to_string();

        let allow_all = vec![("memory.read".to_string(), serde_json::json!({}))];
        assert!(memory_read_record_allowed(&allow_all, &record));

        let working_tier = vec![(
            "memory.read.working".to_string(),
            serde_json::json!({"version": 1, "tiers": ["working"]}),
        )];
        assert!(memory_read_record_allowed(&working_tier, &record));

        let id_match = vec![(
            "memory.read".to_string(),
            serde_json::json!({"version": 1, "record_id": record_id}),
        )];
        assert!(memory_read_record_allowed(&id_match, &record));

        let before_after_record = vec![(
            "memory.read".to_string(),
            serde_json::json!({"version": 1, "before_ms": record.created_at + 1}),
        )];
        assert!(memory_read_record_allowed(&before_after_record, &record));

        let denying_then_allow = vec![
            (
                "memory.read.working".to_string(),
                serde_json::json!({"version": 1, "tiers": ["episodic"]}),
            ),
            ("memory.read".to_string(), serde_json::json!({})),
        ];
        assert!(memory_read_record_allowed(&denying_then_allow, &record));

        let wrong_tier = vec![(
            "memory.read.working".to_string(),
            serde_json::json!({"version": 1, "tiers": ["episodic"]}),
        )];
        assert!(!memory_read_record_allowed(&wrong_tier, &record));

        let wrong_id = vec![(
            "memory.read".to_string(),
            serde_json::json!({"version": 1, "record_id": Uuid::new_v4().to_string()}),
        )];
        assert!(!memory_read_record_allowed(&wrong_id, &record));

        let before_at_record = vec![(
            "memory.read".to_string(),
            serde_json::json!({"version": 1, "before_ms": record.created_at}),
        )];
        assert!(!memory_read_record_allowed(&before_at_record, &record));

        let invalid_scope = vec![("memory.read".to_string(), serde_json::json!(true))];
        assert!(!memory_read_record_allowed(&invalid_scope, &record));

        assert!(!memory_read_record_allowed(&[], &record));
    }

    #[test]
    fn a2a_entry_matches_deadline_within_pins_filter_matrix() {
        let sender = AgentId::new("sender@local", [1u8; 32]);
        let recipient = AgentId::new("recipient@local", [2u8; 32]);
        let make_entry = |deadline_ms: Option<u64>| covenant_a2a::A2ATaskQueueEntry {
            state: covenant_a2a::A2ATaskQueueState::Queued,
            task: covenant_a2a::A2ATask {
                id: Uuid::new_v4(),
                sender: sender.clone(),
                recipient: recipient.clone(),
                intent_text: "anything".into(),
                task_kind: None,
                parent: None,
                deadline_ms,
                idempotency: None,
            },
            lease_id: None,
            leased_to: None,
            leased_at_ms: None,
            attempt: 0,
        };
        let no_deadline = make_entry(None);
        let deadline150 = make_entry(Some(150));
        assert!(a2a_entry_matches_deadline_within(&no_deadline, None, 100));
        assert!(!a2a_entry_matches_deadline_within(
            &no_deadline,
            Some(50),
            100
        ));
        assert!(!a2a_entry_matches_deadline_within(
            &deadline150,
            Some(100),
            0
        ));
        assert!(a2a_entry_matches_deadline_within(
            &deadline150,
            Some(100),
            100
        ));
        assert!(a2a_entry_matches_deadline_within(
            &deadline150,
            Some(50),
            100
        ));
        assert!(!a2a_entry_matches_deadline_within(
            &deadline150,
            Some(10),
            100
        ));
    }

    #[test]
    fn a2a_entry_matches_min_lease_age_pins_filter_matrix() {
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: AgentId::new("sender@local", [1u8; 32]),
            recipient: AgentId::new("recipient@local", [2u8; 32]),
            intent_text: "anything".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        let queued = covenant_a2a::A2ATaskQueueEntry {
            state: covenant_a2a::A2ATaskQueueState::Queued,
            task: task.clone(),
            lease_id: None,
            leased_to: None,
            leased_at_ms: None,
            attempt: 0,
        };
        let in_flight = covenant_a2a::A2ATaskQueueEntry {
            state: covenant_a2a::A2ATaskQueueState::InFlight,
            task,
            lease_id: Some(Uuid::new_v4()),
            leased_to: Some(AgentId::new("leasee@local", [3u8; 32])),
            leased_at_ms: Some(100),
            attempt: 1,
        };
        assert!(a2a_entry_matches_min_lease_age(&queued, None, 200));
        assert!(a2a_entry_matches_min_lease_age(&in_flight, None, 200));
        assert!(a2a_entry_matches_min_lease_age(&queued, Some(50), 200));
        assert!(a2a_entry_matches_min_lease_age(&in_flight, Some(50), 200));
        assert!(!a2a_entry_matches_min_lease_age(&in_flight, Some(500), 200));
        assert!(!a2a_entry_matches_min_lease_age(&in_flight, Some(50), 80));
    }

    #[test]
    fn a2a_entry_visible_to_peer_pins_each_visibility_path() {
        let sender = AgentId::new("sender@local", [1u8; 32]);
        let recipient = AgentId::new("recipient@local", [2u8; 32]);
        let lessee = AgentId::new("lessee@local", [3u8; 32]);
        let outsider = AgentId::new("outsider@local", [9u8; 32]);
        let task = covenant_a2a::A2ATask {
            id: Uuid::new_v4(),
            sender: sender.clone(),
            recipient: recipient.clone(),
            intent_text: "anything".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        let queued = covenant_a2a::A2ATaskQueueEntry {
            state: covenant_a2a::A2ATaskQueueState::Queued,
            task: task.clone(),
            lease_id: None,
            leased_to: None,
            leased_at_ms: None,
            attempt: 0,
        };
        let in_flight = covenant_a2a::A2ATaskQueueEntry {
            state: covenant_a2a::A2ATaskQueueState::InFlight,
            task,
            lease_id: Some(Uuid::new_v4()),
            leased_to: Some(lessee.clone()),
            leased_at_ms: Some(0),
            attempt: 1,
        };
        assert!(a2a_entry_visible_to_peer(&queued, &sender));
        assert!(a2a_entry_visible_to_peer(&queued, &recipient));
        assert!(!a2a_entry_visible_to_peer(&queued, &lessee));
        assert!(!a2a_entry_visible_to_peer(&queued, &outsider));
        assert!(a2a_entry_visible_to_peer(&in_flight, &sender));
        assert!(a2a_entry_visible_to_peer(&in_flight, &recipient));
        assert!(a2a_entry_visible_to_peer(&in_flight, &lessee));
        assert!(!a2a_entry_visible_to_peer(&in_flight, &outsider));
    }

    #[test]
    fn a2a_duplicate_risk_pins_each_arm() {
        let idempotent = covenant_a2a::A2ARepairCommand::Requeue {
            lease_id: Some(Uuid::new_v4()),
            duplicate_risk: covenant_a2a::A2ADuplicateRisk::Idempotent,
        };
        let operator_accepted = covenant_a2a::A2ARepairCommand::Requeue {
            lease_id: Some(Uuid::new_v4()),
            duplicate_risk: covenant_a2a::A2ADuplicateRisk::OperatorAccepted,
        };
        let force_error = covenant_a2a::A2ARepairCommand::ForceError {
            lease_id: Some(Uuid::new_v4()),
            message: "stuck".into(),
        };
        assert_eq!(a2a_duplicate_risk(&idempotent), Some("idempotent"));
        assert_eq!(
            a2a_duplicate_risk(&operator_accepted),
            Some("operator_accepted")
        );
        assert_eq!(a2a_duplicate_risk(&force_error), None);
    }

    /// Wire response rounds tokens_remaining to a powers-of-5 bucket.
    /// Sanity covers the bucket boundaries.
    #[test]
    fn round_tokens_remaining_collapses_to_powers_of_five() {
        assert_eq!(round_tokens_remaining(0), 0);
        assert_eq!(round_tokens_remaining(1), 1);
        assert_eq!(round_tokens_remaining(4), 1);
        assert_eq!(round_tokens_remaining(5), 5);
        assert_eq!(round_tokens_remaining(9), 5);
        assert_eq!(round_tokens_remaining(10), 10);
        assert_eq!(round_tokens_remaining(49), 10);
        assert_eq!(round_tokens_remaining(50), 50);
        assert_eq!(round_tokens_remaining(99), 50);
        assert_eq!(round_tokens_remaining(100), 100);
        assert_eq!(round_tokens_remaining(499), 100);
        assert_eq!(round_tokens_remaining(500), 500);
        assert_eq!(round_tokens_remaining(999), 500);
        assert_eq!(round_tokens_remaining(1000), 1_000);
        assert_eq!(round_tokens_remaining(u64::MAX), 10_000_000_000);
    }

    /// Resume verb plumbing. A `BudgetExhausted` audit row recorded for
    /// a given `intent_id` is the only state the resume verb needs: it
    /// scans the audit, extracts `intent_text`, and runs it through
    /// `dispatch_intent`. Synthesised audit row here so the test doesn't
    /// have to actually exhaust then refill (no clock-injection at the
    /// InMemoryLedger layer).
    #[tokio::test]
    async fn resume_intent_re_dispatches_from_budget_exhausted_audit_row() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let s = Server::new(
            Arc::new(Router::from_cards(vec![stub_card_with_budget(
                "research",
                vec!["tool.web_search"],
                10,
            )])),
            Arc::new(MockRunner::new("resumed result")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );
        s.register_agent_budgets().await.unwrap();
        s.op_respond(Request::GrantCapability {
            action: "tool.web_search".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        grant_action(&s, "memory.write").await;

        // Synthesise a BudgetExhausted row as if a previous dispatch had
        // been rejected. The resume verb scans recent audit, finds this
        // row by intent_id, and re-dispatches the captured text. Tag the
        // synthesised row with the daemon's real pubkey so the per-peer
        // audit filter passes it through to the find_map.
        let exhausted_intent = Uuid::new_v4();
        audit
            .record(AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: s.identity.agent_id(),
                kind: AuditKind::BudgetExhausted {
                    agent_display: "research@agent".into(),
                    intent_id: exhausted_intent,
                    intent_text: "find recent papers".into(),
                    requested: 1,
                    tokens_remaining: 0,
                    refill_eta_ms: 0,
                },
            })
            .await
            .unwrap();

        let resp = s
            .op_respond(Request::ResumeIntent {
                intent_id: exhausted_intent,
            })
            .await;
        match resp {
            Response::IntentResult { text, .. } => assert_eq!(text, "resumed result"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Resume with no matching audit row returns Error, not a fresh
    /// dispatch on an empty intent.
    #[tokio::test]
    async fn resume_intent_returns_error_when_no_audit_row_matches() {
        let s = server_with(
            vec![stub_card_with_budget(
                "research",
                vec!["tool.web_search"],
                10,
            )],
            "should not run",
        );
        let resp = s
            .op_respond(Request::ResumeIntent {
                intent_id: Uuid::new_v4(),
            })
            .await;
        match resp {
            Response::Error { message } => {
                assert!(
                    message.contains("no BudgetExhausted audit row"),
                    "expected resume-not-found message, got {message:?}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_skips_budget_for_zero_credit_agents() {
        let s = server_with(
            vec![stub_card_with_budget(
                "research",
                vec!["tool.web_search"],
                0,
            )],
            "mocked summary",
        );
        // Don't call register_agent_budgets — the agent has no bucket,
        // so try_debit will return NoCapacity, which dispatch treats as
        // a warn-and-pass for v0.
        s.op_respond(Request::GrantCapability {
            action: "tool.web_search".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        grant_action(&s, "memory.write").await;
        let resp = s
            .op_respond(Request::SubmitIntent {
                text: "find recent papers on agent memory".into(),
                prefer_stream: None,
            })
            .await;
        match resp {
            Response::IntentResult { text, .. } => assert_eq!(text, "mocked summary"),
            other => panic!("expected IntentResult for zero-budget agent, got {other:?}"),
        }
    }

    // ---- Write-side audit invariant ----
    //
    // The peer-action sites in `Server::respond` build `AuditEvent`s with
    // `issuer = peer.clone()` and pass them to `record_peer_event`, which
    // asserts the issuer matches the authenticated peer. The forward-
    // looking goal is to catch the next regression that builds an event
    // for one peer's action and accidentally signs it as another (or as
    // the daemon). `cargo test` runs with `debug_assertions = on`, so
    // `debug_assert_eq!` panics — `#[should_panic]` is the natural test
    // shape.

    fn forged_event_with_pubkey(pubkey: [u8; 32], display: &str) -> AuditEvent {
        AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: epoch_ms(),
            issuer: AgentId::new(display.to_string(), pubkey),
            kind: AuditKind::CapabilityCheck {
                agent_id: "tool:test".into(),
                required_actions: vec!["tool.call.test".into()],
                missing_actions: vec![],
                passed: true,
            },
        }
    }

    #[tokio::test]
    #[should_panic(expected = "audit invariant")]
    async fn record_peer_event_panics_when_issuer_does_not_match_peer() {
        let s = server_with(vec![], "");
        let honest_peer = s.identity.agent_id();
        let forged = forged_event_with_pubkey([9u8; 32], "evil@local");
        s.record_peer_event(&honest_peer, forged).await;
    }

    #[tokio::test]
    #[should_panic(expected = "audit invariant")]
    async fn record_daemon_event_panics_when_issuer_is_not_self_identity() {
        let s = server_with(vec![], "");
        let forged = forged_event_with_pubkey([9u8; 32], "evil@local");
        s.record_daemon_event(forged).await;
    }

    #[tokio::test]
    async fn record_peer_event_records_when_issuer_matches_peer() {
        let s = server_with(vec![], "");
        let peer = s.identity.agent_id();
        let event = AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: epoch_ms(),
            issuer: peer.clone(),
            kind: AuditKind::CapabilityCheck {
                agent_id: "tool:positive".into(),
                required_actions: vec!["tool.call.positive".into()],
                missing_actions: vec![],
                passed: true,
            },
        };
        let event_id = event.id;
        s.record_peer_event(&peer, event).await;
        let recent = s.audit.recent(16).await.expect("audit.recent");
        assert!(
            recent.iter().any(|e| e.id == event_id),
            "expected event {event_id} to land in audit.recent"
        );
    }

    #[tokio::test]
    async fn record_daemon_event_records_when_issuer_is_self_identity() {
        // AuthenticationFailed is a must-record kind: it routes through
        // `record_daemon_event_required` and surfaces the audit error to
        // the caller. The test exercises the helper directly so the
        // assertion covers any future daemon-internal call site that
        // doesn't go through `record_auth_failure`.
        let s = server_with(vec![], "");
        let event = AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: epoch_ms(),
            issuer: s.identity.agent_id(),
            kind: AuditKind::AuthenticationFailed {
                transport: "test".into(),
                reason: "synthetic".into(),
            },
        };
        let event_id = event.id;
        s.record_daemon_event_required(event)
            .await
            .expect("must-record audit row must persist");
        let recent = s.audit.recent(16).await.expect("audit.recent");
        assert!(
            recent.iter().any(|e| e.id == event_id),
            "expected daemon event {event_id} to land in audit.recent"
        );
    }

    #[tokio::test]
    async fn recent_debits_returns_empty_when_router_has_no_agents() {
        let s = server_with(vec![], "");
        match s.op_respond(Request::RecentDebits { limit: 10 }).await {
            Response::Debits { debits } => assert!(debits.is_empty()),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_debits_skips_zero_budget_agents() {
        // A `budget_credits_per_hour = 0` manifest never has its bucket
        // seeded by `register_agent_budgets`, so a `recent_debits` query
        // for it would return `NoCapacity`. The fan-out must skip those
        // cards before calling into the ledger.
        let s = server_with(
            vec![stub_card("zero", vec!["tool.web_search"])],
            "mocked summary",
        );
        s.register_agent_budgets().await.unwrap();
        match s.op_respond(Request::RecentDebits { limit: 10 }).await {
            Response::Debits { debits } => assert!(debits.is_empty()),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_debits_returns_debit_after_dispatch() {
        let s = server_with(
            vec![stub_card_with_budget(
                "research",
                vec!["tool.web_search"],
                10,
            )],
            "mocked summary",
        );
        s.register_agent_budgets().await.unwrap();
        s.op_respond(Request::GrantCapability {
            action: "tool.web_search".into(),
            scope: None,
            expires_at: None,
        })
        .await;
        grant_action(&s, "memory.write").await;
        let resp = s
            .op_respond(Request::SubmitIntent {
                text: "find recent papers on agent memory".into(),
                prefer_stream: None,
            })
            .await;
        assert!(matches!(resp, Response::IntentResult { .. }));
        match s.op_respond(Request::RecentDebits { limit: 10 }).await {
            Response::Debits { debits } => {
                assert_eq!(debits.len(), 1);
                assert_eq!(debits[0].agent.display, "research@agent");
                assert_eq!(debits[0].credits, 1);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_debits_sorts_newest_first_across_agents() {
        // Two agents, two debits each. The flat list must be newest-first
        // by `at_ms` regardless of which agent the debits came from.
        let s = server_with(
            vec![
                stub_card_with_budget("alpha", vec![], 100),
                stub_card_with_budget("beta", vec![], 100),
            ],
            "ignored",
        );
        s.register_agent_budgets().await.unwrap();
        let alpha = agent_id_for_card(&stub_card_with_budget("alpha", vec![], 100));
        let beta = agent_id_for_card(&stub_card_with_budget("beta", vec![], 100));
        // Hand-debit so the test controls timing without depending on
        // dispatch wall-clock granularity.
        s.budget.try_debit(&alpha, 1, Uuid::new_v4()).await.unwrap();
        // Wait one ms so the second debit is strictly later.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        s.budget.try_debit(&beta, 1, Uuid::new_v4()).await.unwrap();
        match s.op_respond(Request::RecentDebits { limit: 10 }).await {
            Response::Debits { debits } => {
                assert_eq!(debits.len(), 2);
                assert!(
                    debits[0].at_ms >= debits[1].at_ms,
                    "expected newest-first; got {} then {}",
                    debits[0].at_ms,
                    debits[1].at_ms
                );
                assert_eq!(debits[0].agent.display, "beta@agent");
                assert_eq!(debits[1].agent.display, "alpha@agent");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Constructs a Server with a tempdir-bound home and a pre-seeded
    /// operator token (b58 written to `<home>/peers/operator.token` at
    /// mode 0600 + registered to the daemon identity in the peer
    /// registry). Returns the server, the tempdir handle (drop = teardown),
    /// the old token, and the operator's `AgentId` for assertions.
    async fn server_with_operator_token() -> (Server, tempfile::TempDir, PeerToken, AgentId) {
        let dir = tempfile::tempdir().expect("tempdir");
        let identity = Arc::new(LocalIdentity::generate("user@local"));
        let peers = Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new());

        let old_token = PeerToken::generate();
        let operator = identity.agent_id();
        peers
            .register(PeerEntry {
                token: old_token,
                agent_id: operator.clone(),
                registered_at: epoch_ms(),
            })
            .await
            .expect("register old token");

        let token_path = dir.path().join("peers").join("operator.token");
        write_operator_token_0600(&token_path, &old_token.to_b58()).expect("seed operator.token");

        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("ignored")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            Arc::new(covenant_audit::InMemoryAuditLog::new()),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            identity,
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            peers,
            Arc::new(covenant_budget::InMemoryLedger::new()),
        )
        .with_home(dir.path().to_path_buf());
        (s, dir, old_token, operator)
    }

    /// Happy path: rotation under the operator identity returns the new
    /// token, the registry resolves it to the operator, the old token
    /// no longer resolves, and the on-disk file holds the new b58.
    #[tokio::test]
    async fn rotate_token_succeeds_under_operator_identity() {
        let (s, dir, old_token, operator) = server_with_operator_token().await;

        let new_b58 = match s.op_respond(Request::RotateOperatorToken).await {
            Response::OperatorTokenRotated { token_b58 } => token_b58,
            other => panic!("unexpected: {other:?}"),
        };
        let new_token = PeerToken::from_b58(&new_b58).expect("decode new b58");

        // Registry: new resolves to operator; old returns None.
        assert_eq!(
            s.peers.resolve(&new_token).await.unwrap(),
            Some(operator.clone()),
            "new token must resolve to the operator identity"
        );
        assert_eq!(
            s.peers.resolve(&old_token).await.unwrap(),
            None,
            "old token must be revoked after rotation"
        );

        // Disk: file holds the new b58, mode 0600.
        let token_path = dir.path().join("peers").join("operator.token");
        let on_disk = std::fs::read_to_string(&token_path).expect("read operator.token");
        assert_eq!(on_disk.trim(), new_b58);
        require_operator_token_mode_0600(&token_path).expect("0600 enforced post-rotate");
    }

    /// C3 gate enforcement. A non-operator peer (whose pubkey doesn't
    /// match `self.identity.pubkey`) must be rejected regardless of
    /// authentication state. The "any authenticated peer can rotate"
    /// alternative was rejected for exactly this reason — in Phase-1
    /// multi-peer a guest peer would inherit operator-rotation capability
    /// via authentication alone.
    #[tokio::test]
    async fn rotate_token_rejects_when_peer_is_not_operator_identity() {
        let (s, _dir, old_token, _operator) = server_with_operator_token().await;
        // A foreign peer authenticated by a different ed25519 keypair —
        // valid registration on the wire, but pubkey ≠ identity.pubkey.
        let foreign = AgentId::new("guest@local", [9u8; 32]);
        let resp = s.respond(Request::RotateOperatorToken, &foreign).await;
        match resp {
            Response::Error { message } => {
                assert!(
                    message.contains("operator identity"),
                    "rejection message must name the gate; got {message:?}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
        // Old token must still resolve — the rotation didn't run.
        assert!(
            s.peers.resolve(&old_token).await.unwrap().is_some(),
            "rejected rotation must leave the old token alive"
        );
    }

    /// Verifies the audit row layout: issuer is the operator peer
    /// (peer-event invariant), kind is `OperatorTokenRotated`, and the
    /// embedded prefixes are the 6-char base58 prefixes of the
    /// before/after tokens (no full token bytes in the audit log).
    #[tokio::test]
    async fn rotate_token_records_audit_event_with_token_prefixes() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let identity = Arc::new(LocalIdentity::generate("user@local"));
        let peers = Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new());
        let old_token = PeerToken::generate();
        let operator = identity.agent_id();
        peers
            .register(PeerEntry {
                token: old_token,
                agent_id: operator.clone(),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        write_operator_token_0600(
            &dir.path().join("peers").join("operator.token"),
            &old_token.to_b58(),
        )
        .unwrap();

        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("ignored")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            identity,
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            peers,
            Arc::new(covenant_budget::InMemoryLedger::new()),
        )
        .with_home(dir.path().to_path_buf());

        let new_b58 = match s.op_respond(Request::RotateOperatorToken).await {
            Response::OperatorTokenRotated { token_b58 } => token_b58,
            other => panic!("unexpected: {other:?}"),
        };

        let events = audit.recent(50).await.unwrap();
        let row = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::OperatorTokenRotated { .. }))
            .expect("OperatorTokenRotated row");
        assert_eq!(row.issuer.pubkey, operator.pubkey, "peer-event invariant");
        match &row.kind {
            AuditKind::OperatorTokenRotated {
                peer_display,
                old_token_prefix,
                new_token_prefix,
            } => {
                assert_eq!(peer_display, &operator.display);
                let expected_old: String = old_token.to_b58().chars().take(6).collect();
                let expected_new: String = new_b58.chars().take(6).collect();
                assert_eq!(old_token_prefix, &expected_old);
                assert_eq!(new_token_prefix, &expected_new);
                assert_eq!(old_token_prefix.len(), 6);
                assert_eq!(new_token_prefix.len(), 6);
                assert_ne!(
                    old_token_prefix, new_token_prefix,
                    "the rotation must produce a fresh token"
                );
            }
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    /// Guards against the `with_home` builder being skipped. Without a
    /// configured home, the rotation can't read or write the on-disk
    /// token, so the verb returns `Error` with a message naming the
    /// missing home. Tests that don't construct a tempdir-bound server
    /// (most of them) shouldn't accidentally rotate either.
    #[tokio::test]
    async fn rotate_token_errors_when_server_has_no_home() {
        let s = server_with(vec![], "ignored");
        match s.op_respond(Request::RotateOperatorToken).await {
            Response::Error { message } => {
                assert!(
                    message.contains("home"),
                    "message must mention the missing home; got {message:?}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// The rotation must short-circuit on the C3 gate before touching
    /// the registry. A foreign peer with no on-disk token available
    /// should still be rejected on the identity check, not on a
    /// downstream "read operator token" io-error. Tests that the gate
    /// orderings match the docstring's enumerated steps.
    #[tokio::test]
    async fn rotate_token_identity_gate_runs_before_disk_read() {
        // Server with a configured home but NO on-disk token — if the
        // identity gate ran second, the response would be a `read operator
        // token` io-error, not the identity-check rejection.
        let dir = tempfile::tempdir().expect("tempdir");
        let s = server_with(vec![], "ignored").with_home(dir.path().to_path_buf());
        let foreign = AgentId::new("guest@local", [9u8; 32]);
        match s.respond(Request::RotateOperatorToken, &foreign).await {
            Response::Error { message } => {
                assert!(
                    message.contains("operator identity"),
                    "must rejected on identity gate; got {message:?}"
                );
                assert!(
                    !message.contains("read operator token"),
                    "must not have reached the disk read; got {message:?}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// A foreign peer's RotateOperatorToken attempt records an
    /// `OperatorTokenRotationRejected` audit row. Issuer is the daemon
    /// identity (matching the operator-feed audience model used by
    /// `AuthenticationFailed`); the rejected peer's identity is
    /// preserved in the kind payload (`peer_display` +
    /// `peer_pubkey_b58`). No `OperatorTokenRotated` row appears (the
    /// rotation didn't run).
    #[tokio::test]
    async fn rotate_token_rejection_records_audit_event_with_pubkey_b58() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let identity = Arc::new(LocalIdentity::generate("user@local"));
        let peers = Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new());
        // Seed the operator's on-disk token so we can prove the rotation
        // didn't run (and would otherwise have read it).
        let operator_token = PeerToken::generate();
        let operator = identity.agent_id();
        peers
            .register(PeerEntry {
                token: operator_token,
                agent_id: operator.clone(),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        write_operator_token_0600(
            &dir.path().join("peers").join("operator.token"),
            &operator_token.to_b58(),
        )
        .unwrap();

        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("ignored")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            identity,
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            peers,
            Arc::new(covenant_budget::InMemoryLedger::new()),
        )
        .with_home(dir.path().to_path_buf());

        let foreign_pubkey = [9u8; 32];
        let foreign = AgentId::new("guest@local", foreign_pubkey);
        match s.respond(Request::RotateOperatorToken, &foreign).await {
            Response::Error { .. } => {}
            other => panic!("expected Error, got {other:?}"),
        }

        let events = audit.recent(50).await.unwrap();
        let row = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::OperatorTokenRotationRejected { .. }))
            .expect("OperatorTokenRotationRejected row");
        assert_eq!(
            row.issuer.pubkey, operator.pubkey,
            "issuer is the daemon identity (operator-feed audience)"
        );
        match &row.kind {
            AuditKind::OperatorTokenRotationRejected {
                peer_display,
                peer_pubkey_b58,
            } => {
                assert_eq!(peer_display, &foreign.display);
                assert_eq!(peer_pubkey_b58, &bs58::encode(foreign_pubkey).into_string());
            }
            other => panic!("unexpected kind: {other:?}"),
        }
        // The rotation must NOT have run — no successful row.
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.kind, AuditKind::OperatorTokenRotated { .. })),
            "rejected attempt must not have produced a success row"
        );
    }

    /// The rejection row is visible to the operator's `/audit` feed
    /// under the per-peer filter (`issuer.pubkey == peer.pubkey`).
    /// Regression test for a security finding: the natural
    /// `issuer = peer` shape (mirror of `A2ARecipientRejected`) hid
    /// probes from the operator. Setting `issuer =
    /// self.identity.agent_id()` matches the `AuthenticationFailed`
    /// audience model.
    #[tokio::test]
    async fn rotate_token_rejection_visible_to_operator_audit_feed() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let identity = Arc::new(LocalIdentity::generate("user@local"));
        let operator = identity.agent_id();
        let peers = Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new());
        let operator_token = PeerToken::generate();
        peers
            .register(PeerEntry {
                token: operator_token,
                agent_id: operator.clone(),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        write_operator_token_0600(
            &dir.path().join("peers").join("operator.token"),
            &operator_token.to_b58(),
        )
        .unwrap();

        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("ignored")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            identity,
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            peers,
            Arc::new(covenant_budget::InMemoryLedger::new()),
        )
        .with_home(dir.path().to_path_buf());

        // Foreign peer attempts rotation and is rejected.
        let foreign = AgentId::new("guest@local", [9u8; 32]);
        match s.respond(Request::RotateOperatorToken, &foreign).await {
            Response::Error { .. } => {}
            other => panic!("expected Error, got {other:?}"),
        }

        // Operator opens /audit. The per-peer filter
        // (`issuer.pubkey == peer.pubkey`) keeps only rows where
        // issuer == operator. The rejection row's issuer is the
        // daemon identity == operator, so it must appear.
        let resp = s
            .respond(
                Request::RecentAudit {
                    limit: 50,
                    since_ms: None,
                    prefer_stream: None,
                },
                &operator,
            )
            .await;
        let events = match resp {
            Response::AuditEvents { events } => events,
            other => panic!("unexpected: {other:?}"),
        };
        assert!(
            events
                .iter()
                .any(|e| matches!(e.kind, AuditKind::OperatorTokenRotationRejected { .. })),
            "operator must see the rejection row in their filtered /audit feed"
        );

        // And the foreign peer's filtered feed must NOT contain the
        // row — it carries the operator's pubkey, not the foreign
        // peer's. Probing attacker doesn't get to confirm the probe.
        let resp_foreign = s
            .respond(
                Request::RecentAudit {
                    limit: 50,
                    since_ms: None,
                    prefer_stream: None,
                },
                &foreign,
            )
            .await;
        let events_foreign = match resp_foreign {
            Response::AuditEvents { events } => events,
            other => panic!("unexpected: {other:?}"),
        };
        assert!(
            !events_foreign
                .iter()
                .any(|e| matches!(e.kind, AuditKind::OperatorTokenRotationRejected { .. })),
            "foreign peer must not see the rejection row in their filtered /audit feed"
        );
    }

    /// The operator's own rotation must not produce a rejection row.
    /// v0 single-peer regression: the rejection arm is dead code under
    /// `peer == operator`, so the audit log only carries the success
    /// row.
    #[tokio::test]
    async fn rotate_token_operator_does_not_record_rejection() {
        let (s, _dir, _old_token, operator) = server_with_operator_token().await;
        match s.respond(Request::RotateOperatorToken, &operator).await {
            Response::OperatorTokenRotated { .. } => {}
            other => panic!("expected OperatorTokenRotated, got {other:?}"),
        }
        let events = s.audit.recent(50).await.unwrap();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.kind, AuditKind::OperatorTokenRotationRejected { .. })),
            "operator's own rotation must not produce a rejection row"
        );
    }

    /// The rejection audit row records before the `Response::Error`
    /// returns. Catches the regression where someone later moves the
    /// audit call after the early-return for "tidiness." Done here by
    /// asserting two foreign-peer attempts produce two distinct audit
    /// rows (the audit must persist even on the rejection path for both
    /// attempts to surface).
    #[tokio::test]
    async fn rotate_token_rejection_audit_persists_per_attempt() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let identity = Arc::new(LocalIdentity::generate("user@local"));
        let peers = Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new());
        let operator_token = PeerToken::generate();
        let operator = identity.agent_id();
        peers
            .register(PeerEntry {
                token: operator_token,
                agent_id: operator.clone(),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        write_operator_token_0600(
            &dir.path().join("peers").join("operator.token"),
            &operator_token.to_b58(),
        )
        .unwrap();

        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("ignored")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            identity,
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            peers,
            Arc::new(covenant_budget::InMemoryLedger::new()),
        )
        .with_home(dir.path().to_path_buf());

        let foreign_a = AgentId::new("guestA@local", [9u8; 32]);
        let foreign_b = AgentId::new("guestB@local", [7u8; 32]);
        for peer in [&foreign_a, &foreign_b] {
            match s.respond(Request::RotateOperatorToken, peer).await {
                Response::Error { .. } => {}
                other => panic!("expected Error, got {other:?}"),
            }
        }

        let events = audit.recent(50).await.unwrap();
        let rejected: Vec<_> = events
            .iter()
            .filter(|e| matches!(e.kind, AuditKind::OperatorTokenRotationRejected { .. }))
            .collect();
        assert_eq!(rejected.len(), 2, "two attempts must produce two rows");
        // Each row's issuer is the daemon identity (operator); the
        // attempting peer's pubkey lives in the kind payload.
        assert!(
            rejected.iter().all(|e| e.issuer.pubkey == operator.pubkey),
            "every rejection row's issuer must be the daemon identity"
        );
        let kinds: Vec<&AuditKind> = rejected.iter().map(|e| &e.kind).collect();
        let pubkey_a = bs58::encode([9u8; 32]).into_string();
        let pubkey_b = bs58::encode([7u8; 32]).into_string();
        assert!(
            kinds.iter().any(|k| matches!(
                k,
                AuditKind::OperatorTokenRotationRejected { peer_pubkey_b58, .. }
                    if peer_pubkey_b58 == &pubkey_a
            )),
            "expected guestA's pubkey in a kind payload"
        );
        assert!(
            kinds.iter().any(|k| matches!(
                k,
                AuditKind::OperatorTokenRotationRejected { peer_pubkey_b58, .. }
                    if peer_pubkey_b58 == &pubkey_b
            )),
            "expected guestB's pubkey in a kind payload"
        );
    }

    /// Display-collision probe: a foreign peer registers against
    /// `user@local` (the operator's display) but their pubkey differs.
    /// The audit row's `peer_pubkey_b58` carries the unforgeable
    /// identifier; without it, an operator scanning the audit log by
    /// `peer_display` alone would miss the attack class the C3 gate
    /// exists to surface.
    #[tokio::test]
    async fn rotate_token_rejection_records_distinct_pubkey_under_display_collision() {
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let identity = Arc::new(LocalIdentity::generate("user@local"));
        let operator = identity.agent_id();
        let peers = Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new());
        let operator_token = PeerToken::generate();
        peers
            .register(PeerEntry {
                token: operator_token,
                agent_id: operator.clone(),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        write_operator_token_0600(
            &dir.path().join("peers").join("operator.token"),
            &operator_token.to_b58(),
        )
        .unwrap();

        let s = Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("ignored")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit.clone(),
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            identity,
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::default()),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            peers,
            Arc::new(covenant_budget::InMemoryLedger::new()),
        )
        .with_home(dir.path().to_path_buf());

        // Same display string as the operator, different pubkey.
        let attacker_pubkey = [13u8; 32];
        assert_ne!(
            attacker_pubkey, operator.pubkey,
            "test premise: attacker's pubkey differs from operator"
        );
        let attacker = AgentId::new(operator.display.clone(), attacker_pubkey);

        match s.respond(Request::RotateOperatorToken, &attacker).await {
            Response::Error { .. } => {}
            other => panic!("expected Error, got {other:?}"),
        }
        let events = audit.recent(50).await.unwrap();
        let row = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::OperatorTokenRotationRejected { .. }))
            .expect("rejection row");
        match &row.kind {
            AuditKind::OperatorTokenRotationRejected {
                peer_display,
                peer_pubkey_b58,
            } => {
                assert_eq!(peer_display, &operator.display);
                assert_eq!(
                    peer_pubkey_b58,
                    &bs58::encode(attacker_pubkey).into_string()
                );
                assert_ne!(
                    peer_pubkey_b58,
                    &bs58::encode(operator.pubkey).into_string(),
                    "row must distinguish attacker's pubkey from operator's"
                );
            }
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    /// Operator-only peers list returns redacted summaries for both
    /// live and revoked entries. Closes the display-collision probe
    /// post-incident response gap.
    #[tokio::test]
    async fn list_peers_returns_summaries_for_operator() {
        let s = server_with(vec![], "");
        // Seed three peers: operator (auto-registered? no — fresh
        // server has empty registry), one live guest, one revoked.
        let live_token = PeerToken::generate();
        let dead_token = PeerToken::generate();
        s.peers
            .register(PeerEntry {
                token: live_token,
                agent_id: AgentId::new("guest@local", [1u8; 32]),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        s.peers
            .register(PeerEntry {
                token: dead_token,
                agent_id: AgentId::new("ghost@local", [2u8; 32]),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        s.peers.revoke(&dead_token).await.unwrap();

        let resp = s
            .op_respond(Request::ListPeers {
                limit: 10,
                pubkey_prefix: None,
                status_filter: None,
            })
            .await;
        let peers = match resp {
            Response::PeerList { peers, .. } => peers,
            other => panic!("expected PeerList, got {other:?}"),
        };
        assert_eq!(peers.len(), 2, "live + revoked both surface");
        let dead = peers
            .iter()
            .find(|p| p.agent_id.display == "ghost@local")
            .expect("revoked entry");
        assert!(dead.revoked_at.is_some(), "revoked entry carries timestamp");
        let live = peers
            .iter()
            .find(|p| p.agent_id.display == "guest@local")
            .expect("live entry");
        assert!(live.revoked_at.is_none());
    }

    /// C3 gate enforcement. A non-operator peer is rejected with
    /// `Response::Error` and an `OperatorPeersListRejected` audit row
    /// whose issuer is the daemon identity (operator is the security
    /// audience). The peer's identity is preserved in the kind payload
    /// (`peer_display` + `peer_pubkey_b58`).
    #[tokio::test]
    async fn list_peers_rejects_non_operator_with_audit_row() {
        let s = server_with(vec![], "");
        let foreign_pubkey = [9u8; 32];
        let foreign = AgentId::new("guest@local", foreign_pubkey);
        match s
            .respond(
                Request::ListPeers {
                    limit: 10,
                    pubkey_prefix: None,
                    status_filter: None,
                },
                &foreign,
            )
            .await
        {
            Response::Error { message } => {
                assert!(
                    message.contains("operator identity"),
                    "rejection message must name the gate; got {message:?}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
        let events = s.audit.recent(50).await.unwrap();
        let row = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::OperatorPeersListRejected { .. }))
            .expect("OperatorPeersListRejected row");
        assert_eq!(
            row.issuer.pubkey,
            s.identity.agent_id().pubkey,
            "issuer is the daemon identity (operator-feed audience)"
        );
        match &row.kind {
            AuditKind::OperatorPeersListRejected {
                peer_display,
                peer_pubkey_b58,
            } => {
                assert_eq!(peer_display, &foreign.display);
                assert_eq!(peer_pubkey_b58, &bs58::encode(foreign_pubkey).into_string());
            }
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    /// Regression test for the per-peer audit-filter audience. The
    /// rejection row must reach the operator's `/audit` feed and must
    /// NOT reach the rejected peer's `/audit` feed (no oracle for the
    /// probing attacker). Mirrors the
    /// `rotate_token_rejection_visible_to_operator_audit_feed` test.
    #[tokio::test]
    async fn list_peers_rejection_visible_to_operator_audit_feed() {
        let s = server_with(vec![], "");
        let operator = s.identity.agent_id();
        let foreign = AgentId::new("guest@local", [9u8; 32]);
        match s
            .respond(
                Request::ListPeers {
                    limit: 10,
                    pubkey_prefix: None,
                    status_filter: None,
                },
                &foreign,
            )
            .await
        {
            Response::Error { .. } => {}
            other => panic!("expected Error, got {other:?}"),
        }

        let resp_op = s
            .respond(
                Request::RecentAudit {
                    limit: 50,
                    since_ms: None,
                    prefer_stream: None,
                },
                &operator,
            )
            .await;
        let events_op = match resp_op {
            Response::AuditEvents { events } => events,
            other => panic!("unexpected: {other:?}"),
        };
        assert!(
            events_op
                .iter()
                .any(|e| matches!(e.kind, AuditKind::OperatorPeersListRejected { .. })),
            "operator must see the rejection row in their filtered /audit feed"
        );

        let resp_foreign = s
            .respond(
                Request::RecentAudit {
                    limit: 50,
                    since_ms: None,
                    prefer_stream: None,
                },
                &foreign,
            )
            .await;
        let events_foreign = match resp_foreign {
            Response::AuditEvents { events } => events,
            other => panic!("unexpected: {other:?}"),
        };
        assert!(
            !events_foreign
                .iter()
                .any(|e| matches!(e.kind, AuditKind::OperatorPeersListRejected { .. })),
            "rejected peer must not see the row (no oracle)"
        );
    }

    #[tokio::test]
    async fn list_peers_rejects_scope_pubkey_mismatch_and_audits() {
        let s = server_with(vec![], "");
        let delegate = AgentId::new("delegate@local", [9u8; 32]);
        let target_pubkey = [1u8; 32];
        let other_pubkey = [2u8; 32];
        let target_pubkey_b58 = bs58::encode(target_pubkey).into_string();
        let other_pubkey_b58 = bs58::encode(other_pubkey).into_string();
        s.peers
            .register(PeerEntry {
                token: PeerToken::generate(),
                agent_id: AgentId::new("target@local", target_pubkey),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        s.peers
            .register(PeerEntry {
                token: PeerToken::generate(),
                agent_id: AgentId::new("other@local", other_pubkey),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        grant_scoped_action_to(
            &s,
            &delegate,
            "peers.list",
            serde_json::json!({
                "version": 1,
                "peer_pubkey_b58": target_pubkey_b58
            }),
        )
        .await;

        let rejected = s
            .respond(
                Request::ListPeers {
                    limit: 10,
                    pubkey_prefix: Some(other_pubkey_b58),
                    status_filter: None,
                },
                &delegate,
            )
            .await;
        match rejected {
            Response::Error { message } => assert!(message.contains("capability scope")),
            other => panic!("expected scope rejection, got {other:?}"),
        }
        assert!(s.audit.recent(50).await.unwrap().iter().any(|event| {
            matches!(
                &event.kind,
                AuditKind::CapabilityScopeRejected { action, .. } if action == "peers.list"
            )
        }));

        let allowed = s
            .respond(
                Request::ListPeers {
                    limit: 10,
                    pubkey_prefix: Some(target_pubkey_b58),
                    status_filter: None,
                },
                &delegate,
            )
            .await;
        match allowed {
            Response::PeerList { peers, .. } => {
                assert_eq!(peers.len(), 1);
                assert_eq!(peers[0].agent_id.display, "target@local");
            }
            other => panic!("expected scoped list success, got {other:?}"),
        }
    }

    /// Server-side `pubkey_prefix` filter. Paste the b58 of an audit
    /// row's `peer_pubkey_b58` and the daemon returns only matching
    /// registry entries.
    #[tokio::test]
    async fn list_peers_filters_by_pubkey_prefix() {
        let s = server_with(vec![], "");
        let target_pubkey = [0xfeu8; 32];
        let other_pubkey = [0x01u8; 32];
        s.peers
            .register(PeerEntry {
                token: PeerToken::generate(),
                agent_id: AgentId::new("target@local", target_pubkey),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        s.peers
            .register(PeerEntry {
                token: PeerToken::generate(),
                agent_id: AgentId::new("other@local", other_pubkey),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        let target_b58 = bs58::encode(target_pubkey).into_string();
        let prefix: String = target_b58.chars().take(6).collect();
        let resp = s
            .op_respond(Request::ListPeers {
                limit: 10,
                pubkey_prefix: Some(prefix),
                status_filter: None,
            })
            .await;
        let peers = match resp {
            Response::PeerList { peers, .. } => peers,
            other => panic!("expected PeerList, got {other:?}"),
        };
        assert_eq!(peers.len(), 1, "only the matching pubkey surfaces");
        assert_eq!(peers[0].agent_id.display, "target@local");
    }

    /// Wire-format security: a `Response::PeerList` must never carry a
    /// peer's full token b58. Catches a regression where someone reuses
    /// `PeerEntry` (which serializes the full token) as the response
    /// shape instead of `PeerSummary`.
    #[tokio::test]
    async fn list_peers_response_never_contains_full_token_b58() {
        let s = server_with(vec![], "");
        let token = PeerToken::generate();
        s.peers
            .register(PeerEntry {
                token,
                agent_id: AgentId::new("guest@local", [1u8; 32]),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        let resp = s
            .op_respond(Request::ListPeers {
                limit: 10,
                pubkey_prefix: None,
                status_filter: None,
            })
            .await;
        let json = serde_json::to_string(&resp).expect("serialize PeerList");
        let full_b58 = token.to_b58();
        assert!(
            !json.contains(&full_b58),
            "response must not carry full token b58: {json}"
        );
        let prefix: String = full_b58.chars().take(6).collect();
        assert!(
            json.contains(&prefix),
            "response should still expose the 6-char redacted prefix"
        );
    }

    /// `Response::PeerList` carries the daemon's own identity pubkey
    /// as `operator_pubkey_b58` so the web UI can hide the revoke
    /// button on the operator's own row (clicking it would brick auth
    /// in v0 single-peer). The value is the b58 encoding of
    /// `self.identity.pubkey` — exactly the encoding used by the
    /// `peer_pubkey_b58` audit-row field, so existing redaction rules
    /// apply.
    #[tokio::test]
    async fn list_peers_response_carries_operator_pubkey_b58() {
        let s = server_with(vec![], "");
        s.peers
            .register(PeerEntry {
                token: PeerToken::generate(),
                agent_id: AgentId::new("guest@local", [1u8; 32]),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        let resp = s
            .op_respond(Request::ListPeers {
                limit: 10,
                pubkey_prefix: None,
                status_filter: None,
            })
            .await;
        match resp {
            Response::PeerList {
                operator_pubkey_b58,
                ..
            } => {
                assert_eq!(
                    operator_pubkey_b58,
                    bs58::encode(s.identity.agent_id().pubkey).into_string(),
                );
            }
            other => panic!("expected PeerList, got {other:?}"),
        }
    }

    /// `operator_pubkey_b58` is consumer-stable across every successful
    /// `list_peers` response: it does not depend on the prefix filter,
    /// the registry contents, or whether the operator's bootstrap entry
    /// happens to be in the registry. Web UI's self-row predicate is
    /// therefore safe to apply on every poll without recomputing the
    /// comparator.
    #[tokio::test]
    async fn list_peers_operator_pubkey_b58_is_stable_across_filters() {
        let s = server_with(vec![], "");
        s.peers
            .register(PeerEntry {
                token: PeerToken::generate(),
                agent_id: AgentId::new("guest@local", [1u8; 32]),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        let unfiltered = s
            .op_respond(Request::ListPeers {
                limit: 10,
                pubkey_prefix: None,
                status_filter: None,
            })
            .await;
        let filtered = s
            .op_respond(Request::ListPeers {
                limit: 10,
                pubkey_prefix: Some("zzzzzz".into()),
                status_filter: None,
            })
            .await;
        let a = match unfiltered {
            Response::PeerList {
                operator_pubkey_b58,
                ..
            } => operator_pubkey_b58,
            other => panic!("unexpected: {other:?}"),
        };
        let b = match filtered {
            Response::PeerList {
                peers,
                operator_pubkey_b58,
                ..
            } => {
                assert!(peers.is_empty(), "no peer matches the bogus prefix");
                operator_pubkey_b58
            }
            other => panic!("unexpected: {other:?}"),
        };
        assert_eq!(a, b);
    }

    /// Wire-format invariant: `Response::PeerList` JSON has a top-level
    /// `operator_pubkey_b58` field whose value matches the b58 encoding
    /// of `self.identity.pubkey`. Catches a regression where someone
    /// removes the field thinking the web UI's per-row pubkey already
    /// suffices (it doesn't — without the comparator, the UI cannot
    /// distinguish the operator's own row).
    #[tokio::test]
    async fn list_peers_response_serialises_operator_pubkey_b58_field() {
        let s = server_with(vec![], "");
        let resp = s
            .op_respond(Request::ListPeers {
                limit: 10,
                pubkey_prefix: None,
                status_filter: None,
            })
            .await;
        let json = serde_json::to_string(&resp).expect("serialize PeerList");
        let expected = bs58::encode(s.identity.agent_id().pubkey).into_string();
        assert!(
            json.contains(&format!("\"operator_pubkey_b58\":\"{expected}\"")),
            "expected operator_pubkey_b58 field with daemon identity b58: {json}"
        );
    }

    /// `#[serde(default)]` lets a stale CLI built before the field
    /// landed deserialise a new daemon's `PeerList` response and
    /// receive `operator_pubkey_b58: ""`. The empty string never
    /// matches a real pubkey b58, so the consumer's self-row predicate
    /// falls through to the pre-field behaviour (no false-self hides)
    /// — strictly safer than a hard deserialize error.
    #[test]
    fn list_peers_response_deserialises_without_operator_pubkey_b58_field() {
        let json = r#"{"kind":"peer_list","peers":[]}"#;
        let resp: Response = serde_json::from_str(json).expect("deserialize");
        match resp {
            Response::PeerList {
                peers,
                operator_pubkey_b58,
                truncated,
            } => {
                assert!(peers.is_empty());
                assert_eq!(operator_pubkey_b58, "");
                assert!(!truncated, "missing field defaults to false");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Operator-only `peers revoke` succeeds under the C3 gate and
    /// emits a `PeerRevoked` audit row whose issuer is the operator
    /// (peer-event audience). The kind payload names the revoked peer
    /// (display + pubkey-b58 + token-prefix); the operator is the
    /// issuer because they took the action.
    #[tokio::test]
    async fn revoke_peer_succeeds_under_operator_identity_with_audit_row() {
        let s = server_with(vec![], "");
        let guest_token = PeerToken::generate();
        let guest_pubkey = [7u8; 32];
        s.peers
            .register(PeerEntry {
                token: guest_token,
                agent_id: AgentId::new("guest@local", guest_pubkey),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        let prefix: String = guest_token.to_b58().chars().take(6).collect();
        let resp = s
            .op_respond(Request::RevokePeer {
                token_prefix: prefix.clone(),
                force: false,
                match_limit: None,
            })
            .await;
        match resp {
            Response::PeerRevoked { outcome } => match outcome {
                RevokeOutcome::Revoked(summary) => {
                    assert_eq!(summary.agent_id.display, "guest@local");
                    assert_eq!(summary.agent_id.pubkey, guest_pubkey);
                    assert_eq!(summary.token_prefix, prefix);
                    assert!(summary.revoked_at.is_some());
                }
                other => panic!("expected Revoked, got {other:?}"),
            },
            other => panic!("expected PeerRevoked, got {other:?}"),
        }
        // The token no longer resolves.
        assert_eq!(s.peers.resolve(&guest_token).await.unwrap(), None);
        // PeerRevoked audit row exists with operator-issuer.
        let events = s.audit.recent(50).await.unwrap();
        let row = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::PeerRevoked { .. }))
            .expect("PeerRevoked row");
        assert_eq!(
            row.issuer.pubkey,
            s.identity.agent_id().pubkey,
            "issuer is the operator (peer-event audience)"
        );
        match &row.kind {
            AuditKind::PeerRevoked {
                peer_display,
                peer_pubkey_b58,
                token_prefix,
            } => {
                assert_eq!(peer_display, "guest@local");
                assert_eq!(peer_pubkey_b58, &bs58::encode(guest_pubkey).into_string());
                assert_eq!(token_prefix, &prefix);
            }
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    /// C3 gate enforcement. A non-operator peer is rejected with
    /// `Response::Error` and an `OperatorPeerRevokeRejected` audit row
    /// whose issuer is the daemon identity (operator is the security
    /// audience, not the rejected peer).
    #[tokio::test]
    async fn revoke_peer_rejects_non_operator_with_audit_row() {
        let s = server_with(vec![], "");
        let foreign_pubkey = [11u8; 32];
        let foreign = AgentId::new("guest@local", foreign_pubkey);
        match s
            .respond(
                Request::RevokePeer {
                    token_prefix: "abcdef".into(),
                    force: false,
                    match_limit: None,
                },
                &foreign,
            )
            .await
        {
            Response::Error { message } => {
                assert!(
                    message.contains("operator identity"),
                    "rejection message must name the gate; got {message:?}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
        let events = s.audit.recent(50).await.unwrap();
        let row = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::OperatorPeerRevokeRejected { .. }))
            .expect("OperatorPeerRevokeRejected row");
        assert_eq!(
            row.issuer.pubkey,
            s.identity.agent_id().pubkey,
            "issuer is the daemon identity (operator-feed audience)"
        );
        match &row.kind {
            AuditKind::OperatorPeerRevokeRejected {
                peer_display,
                peer_pubkey_b58,
            } => {
                assert_eq!(peer_display, &foreign.display);
                assert_eq!(peer_pubkey_b58, &bs58::encode(foreign_pubkey).into_string());
            }
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    /// Audience-filter regression. The rejection row must reach the
    /// operator's `/audit` feed and must NOT reach the rejected peer's
    /// feed (no oracle for the probing attacker). Mirrors
    /// `list_peers_rejection_visible_to_operator_audit_feed`.
    #[tokio::test]
    async fn revoke_peer_rejection_visible_to_operator_audit_feed() {
        let s = server_with(vec![], "");
        let operator = s.identity.agent_id();
        let foreign = AgentId::new("guest@local", [11u8; 32]);
        match s
            .respond(
                Request::RevokePeer {
                    token_prefix: "abcdef".into(),
                    force: false,
                    match_limit: None,
                },
                &foreign,
            )
            .await
        {
            Response::Error { .. } => {}
            other => panic!("expected Error, got {other:?}"),
        }
        let resp_op = s
            .respond(
                Request::RecentAudit {
                    limit: 50,
                    since_ms: None,
                    prefer_stream: None,
                },
                &operator,
            )
            .await;
        let events_op = match resp_op {
            Response::AuditEvents { events } => events,
            other => panic!("unexpected: {other:?}"),
        };
        assert!(
            events_op
                .iter()
                .any(|e| matches!(e.kind, AuditKind::OperatorPeerRevokeRejected { .. })),
            "operator must see the rejection row in their filtered /audit feed"
        );

        let resp_foreign = s
            .respond(
                Request::RecentAudit {
                    limit: 50,
                    since_ms: None,
                    prefer_stream: None,
                },
                &foreign,
            )
            .await;
        let events_foreign = match resp_foreign {
            Response::AuditEvents { events } => events,
            other => panic!("unexpected: {other:?}"),
        };
        assert!(
            !events_foreign
                .iter()
                .any(|e| matches!(e.kind, AuditKind::OperatorPeerRevokeRejected { .. })),
            "rejected peer must not see the row (no oracle)"
        );
    }

    #[tokio::test]
    async fn revoke_peer_rejects_scope_token_mismatch_and_audits() {
        let s = server_with(vec![], "");
        let delegate = AgentId::new("delegate@local", [11u8; 32]);
        let target_token = PeerToken::from_bytes([21u8; 32]);
        let other_token = PeerToken::from_bytes([22u8; 32]);
        let target_prefix: String = target_token.to_b58().chars().take(6).collect();
        let other_prefix: String = other_token.to_b58().chars().take(6).collect();
        assert!(
            !other_prefix.starts_with(&target_prefix),
            "test premise: prefixes must differ"
        );
        s.peers
            .register(PeerEntry {
                token: target_token,
                agent_id: AgentId::new("target@local", [1u8; 32]),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        s.peers
            .register(PeerEntry {
                token: other_token,
                agent_id: AgentId::new("other@local", [2u8; 32]),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        grant_scoped_action_to(
            &s,
            &delegate,
            "peers.revoke",
            serde_json::json!({
                "version": 1,
                "token_prefix": target_prefix,
                "force": false
            }),
        )
        .await;

        let rejected = s
            .respond(
                Request::RevokePeer {
                    token_prefix: other_prefix,
                    force: false,
                    match_limit: None,
                },
                &delegate,
            )
            .await;
        match rejected {
            Response::Error { message } => assert!(message.contains("capability scope")),
            other => panic!("expected scope rejection, got {other:?}"),
        }
        assert!(s.peers.resolve(&other_token).await.unwrap().is_some());
        assert!(s.audit.recent(50).await.unwrap().iter().any(|event| {
            matches!(
                &event.kind,
                AuditKind::CapabilityScopeRejected { action, .. } if action == "peers.revoke"
            )
        }));

        let allowed = s
            .respond(
                Request::RevokePeer {
                    token_prefix: target_token.to_b58(),
                    force: false,
                    match_limit: None,
                },
                &delegate,
            )
            .await;
        match allowed {
            Response::PeerRevoked {
                outcome: RevokeOutcome::Revoked(summary),
            } => assert_eq!(summary.agent_id.display, "target@local"),
            other => panic!("expected scoped revoke success, got {other:?}"),
        }
        assert_eq!(s.peers.resolve(&target_token).await.unwrap(), None);
    }

    /// Ambiguous outcome. Two peers whose tokens share a 1-char b58
    /// prefix; the operator's revoke matches both and the daemon
    /// returns `Ambiguous { matches }`. The registry is unchanged (both
    /// still resolve) and NO audit row is recorded — non-rejection
    /// failures are not security events.
    #[tokio::test]
    async fn revoke_peer_returns_ambiguous_when_prefix_matches_multiple() {
        let s = server_with(vec![], "");
        let (t1, e1) = peer_with_token_b58_starting_with("1", "a@local", [1u8; 32]);
        let (t2, e2) = peer_with_token_b58_starting_with("1", "b@local", [2u8; 32]);
        s.peers.register(e1).await.unwrap();
        s.peers.register(e2).await.unwrap();
        let pre_count = s.audit.recent(50).await.unwrap().len();
        let resp = s
            .op_respond(Request::RevokePeer {
                token_prefix: "1".into(),
                force: false,
                match_limit: None,
            })
            .await;
        match resp {
            Response::PeerRevoked { outcome } => match outcome {
                RevokeOutcome::Ambiguous { matches, truncated } => {
                    assert_eq!(matches.len(), 2, "both seeded peers surface");
                    assert!(!truncated, "two matches under PEER_MATCH_LIMIT");
                    assert!(matches.iter().all(|m| m.revoked_at.is_none()));
                }
                other => panic!("expected Ambiguous, got {other:?}"),
            },
            other => panic!("expected PeerRevoked, got {other:?}"),
        }
        // Both still resolve — the registry is unchanged.
        assert!(s.peers.resolve(&t1).await.unwrap().is_some());
        assert!(s.peers.resolve(&t2).await.unwrap().is_some());
        // No audit row added — non-rejection failures are not security events.
        let post_count = s.audit.recent(50).await.unwrap().len();
        assert_eq!(
            post_count, pre_count,
            "ambiguous outcome must not emit any audit row"
        );
    }

    /// `Response::PeerList.truncated` is `true` when more registry rows
    /// existed than the caller's `limit` allowed. The daemon does not
    /// transform the registry's truncation flag — it threads through to
    /// the response so the operator can see "you're not seeing them
    /// all" without a second round-trip.
    #[tokio::test]
    async fn list_peers_response_marks_truncated_when_registry_truncates() {
        let s = server_with(vec![], "");
        for i in 0..3u8 {
            s.peers
                .register(PeerEntry {
                    token: PeerToken::generate(),
                    agent_id: AgentId::new(format!("p{i}@local"), [i; 32]),
                    registered_at: epoch_ms(),
                })
                .await
                .unwrap();
        }
        let resp = s
            .op_respond(Request::ListPeers {
                limit: 2,
                pubkey_prefix: None,
                status_filter: None,
            })
            .await;
        match resp {
            Response::PeerList {
                peers, truncated, ..
            } => {
                assert_eq!(peers.len(), 2, "list capped at limit");
                assert!(truncated, "third peer drops; flag set");
            }
            other => panic!("expected PeerList, got {other:?}"),
        }
    }

    /// `status_filter: Some(Live)` threads through to
    /// `PeerRegistry::list_summaries` and drops every revoked row
    /// before the limit-cap is applied. Regression against a refactor
    /// that filters in the `Server` boundary after the registry returns
    /// — that ordering would re-run the truncation logic and could
    /// under-fill or over-truncate the response.
    #[tokio::test]
    async fn list_peers_response_filters_by_live_status() {
        let s = server_with(vec![], "");
        let live_token = PeerToken::generate();
        s.peers
            .register(PeerEntry {
                token: live_token,
                agent_id: AgentId::new("alive@local", [1u8; 32]),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        let dead_token = PeerToken::generate();
        s.peers
            .register(PeerEntry {
                token: dead_token,
                agent_id: AgentId::new("ghost@local", [2u8; 32]),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        s.peers.revoke(&dead_token).await.unwrap();

        let resp = s
            .op_respond(Request::ListPeers {
                limit: 10,
                pubkey_prefix: None,
                status_filter: Some(covenant_peer_auth::PeerStatusFilter::Live),
            })
            .await;
        match resp {
            Response::PeerList {
                peers, truncated, ..
            } => {
                assert!(!truncated);
                assert_eq!(peers.len(), 1, "only the live row surfaces");
                assert_eq!(peers[0].agent_id.display, "alive@local");
                assert!(peers[0].revoked_at.is_none());
            }
            other => panic!("expected PeerList, got {other:?}"),
        }
    }

    /// `status_filter: Some(Revoked)` is the inverse — drops every live
    /// row. Pairs with the live-only test to pin both branches of the
    /// status filter at the daemon boundary.
    #[tokio::test]
    async fn list_peers_response_filters_by_revoked_status() {
        let s = server_with(vec![], "");
        let live_token = PeerToken::generate();
        s.peers
            .register(PeerEntry {
                token: live_token,
                agent_id: AgentId::new("alive@local", [1u8; 32]),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        let dead_token = PeerToken::generate();
        s.peers
            .register(PeerEntry {
                token: dead_token,
                agent_id: AgentId::new("ghost@local", [2u8; 32]),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        s.peers.revoke(&dead_token).await.unwrap();

        let resp = s
            .op_respond(Request::ListPeers {
                limit: 10,
                pubkey_prefix: None,
                status_filter: Some(covenant_peer_auth::PeerStatusFilter::Revoked),
            })
            .await;
        match resp {
            Response::PeerList {
                peers, truncated, ..
            } => {
                assert!(!truncated);
                assert_eq!(peers.len(), 1, "only the revoked row surfaces");
                assert_eq!(peers[0].agent_id.display, "ghost@local");
                assert!(peers[0].revoked_at.is_some());
            }
            other => panic!("expected PeerList, got {other:?}"),
        }
    }

    /// Stale CLI built before `status_filter` landed sends the field-less
    /// frame; new daemon parses missing field as `None` (`#[serde(default)]`)
    /// and returns the pre-filter shape. Forward-compat regression.
    #[tokio::test]
    async fn list_peers_request_status_filter_field_is_serde_default_for_forward_compat() {
        let raw = r#"{"kind":"list_peers","limit":10,"pubkey_prefix":null}"#;
        let req: Request = serde_json::from_str(raw).unwrap();
        match req {
            Request::ListPeers {
                limit,
                pubkey_prefix,
                status_filter,
            } => {
                assert_eq!(limit, 10);
                assert_eq!(pubkey_prefix, None);
                assert_eq!(status_filter, None, "missing field defaults to None");
            }
            other => panic!("expected ListPeers, got {other:?}"),
        }
    }

    /// `Response::PeerRevoked { outcome: Ambiguous { truncated } }` is
    /// `true` when more than `PEER_MATCH_LIMIT` registry entries match
    /// the operator's prefix. The daemon does not transform the
    /// registry's truncation flag — it threads through to the response.
    #[tokio::test]
    async fn revoke_peer_response_marks_ambiguous_truncated() {
        let s = server_with(vec![], "");
        for i in 0..(PEER_MATCH_LIMIT + 1) {
            let mut pubkey = [0u8; 32];
            pubkey[0] = i as u8;
            let (_, ent) =
                peer_with_token_b58_starting_with("1", &format!("collide{i}@local"), pubkey);
            s.peers.register(ent).await.unwrap();
        }
        let resp = s
            .op_respond(Request::RevokePeer {
                token_prefix: "1".into(),
                force: false,
                match_limit: None,
            })
            .await;
        match resp {
            Response::PeerRevoked { outcome } => match outcome {
                RevokeOutcome::Ambiguous { matches, truncated } => {
                    assert_eq!(
                        matches.len(),
                        PEER_MATCH_LIMIT,
                        "list capped at PEER_MATCH_LIMIT",
                    );
                    assert!(truncated, "more matches existed; flag set");
                }
                other => panic!("expected Ambiguous, got {other:?}"),
            },
            other => panic!("expected PeerRevoked, got {other:?}"),
        }
    }

    /// `Request::RevokePeer.match_limit = Some(N)` caps the response's
    /// `Ambiguous.matches` at `N` even when more than `PEER_MATCH_LIMIT`
    /// entries match. The CLI's `--limit-matches` flag rides this
    /// through; `truncated` is set whenever more entries existed than
    /// the caller's `N`.
    #[tokio::test]
    async fn revoke_peer_match_limit_overrides_constant() {
        let s = server_with(vec![], "");
        for i in 0..5u8 {
            let mut pubkey = [0u8; 32];
            pubkey[0] = i;
            let (_, ent) =
                peer_with_token_b58_starting_with("1", &format!("collide{i}@local"), pubkey);
            s.peers.register(ent).await.unwrap();
        }
        let resp = s
            .op_respond(Request::RevokePeer {
                token_prefix: "1".into(),
                force: false,
                match_limit: Some(2),
            })
            .await;
        match resp {
            Response::PeerRevoked {
                outcome: RevokeOutcome::Ambiguous { matches, truncated },
            } => {
                assert_eq!(matches.len(), 2, "caller-supplied cap honoured");
                assert!(truncated, "five matches existed beyond the cap of 2");
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    /// `match_limit: None` falls back to the daemon's `PEER_MATCH_LIMIT`
    /// constant. The Sprint 77 wire shape preserves the daemon-side
    /// safe default for stale CLIs that don't yet know about the field.
    #[tokio::test]
    async fn revoke_peer_match_limit_none_uses_daemon_constant() {
        let s = server_with(vec![], "");
        for i in 0..(PEER_MATCH_LIMIT + 2) {
            let mut pubkey = [0u8; 32];
            pubkey[0] = i as u8;
            let (_, ent) =
                peer_with_token_b58_starting_with("1", &format!("collide{i}@local"), pubkey);
            s.peers.register(ent).await.unwrap();
        }
        let resp = s
            .op_respond(Request::RevokePeer {
                token_prefix: "1".into(),
                force: false,
                match_limit: None,
            })
            .await;
        match resp {
            Response::PeerRevoked {
                outcome: RevokeOutcome::Ambiguous { matches, truncated },
            } => {
                assert_eq!(
                    matches.len(),
                    PEER_MATCH_LIMIT,
                    "None falls back to constant",
                );
                assert!(truncated);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    /// Wire-format regression: a `Response::PeerRevoked` must never
    /// carry a peer's full token b58. Mirrors
    /// `list_peers_response_never_contains_full_token_b58`.
    #[tokio::test]
    async fn revoke_peer_response_never_contains_full_token_b58() {
        let s = server_with(vec![], "");
        let token = PeerToken::generate();
        s.peers
            .register(PeerEntry {
                token,
                agent_id: AgentId::new("guest@local", [1u8; 32]),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        let prefix: String = token.to_b58().chars().take(6).collect();
        let resp = s
            .op_respond(Request::RevokePeer {
                token_prefix: prefix.clone(),
                force: false,
                match_limit: None,
            })
            .await;
        let json = serde_json::to_string(&resp).expect("serialize PeerRevoked");
        let full_b58 = token.to_b58();
        assert!(
            !json.contains(&full_b58),
            "response must not carry full token b58: {json}"
        );
        assert!(
            json.contains(&prefix),
            "response should still expose the 6-char redacted prefix"
        );
    }

    /// `match_limit: Some(0)` is rejected at the daemon boundary.
    /// Without this guard a `take(0 + 1)` peek collapses ambiguous-prefix
    /// detection into a unique-match revoke of an arbitrary row. The CLI
    /// already rejects 0 client-side; this regression test pins the
    /// daemon-side defence-in-depth against direct HTTP callers.
    #[tokio::test]
    async fn revoke_peer_rejects_zero_match_limit() {
        let s = server_with(vec![], "");
        s.peers
            .register(PeerEntry {
                token: PeerToken::generate(),
                agent_id: AgentId::new("guest@local", [1u8; 32]),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        match s
            .op_respond(Request::RevokePeer {
                token_prefix: "anything".into(),
                force: false,
                match_limit: Some(0),
            })
            .await
        {
            Response::Error { message } => assert!(message.contains("at least 1")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// Empty prefix is rejected at the daemon boundary. Without this
    /// guard the registry would return `Ambiguous { matches: <every
    /// entry> }`, which is operationally a footgun.
    #[tokio::test]
    async fn revoke_peer_rejects_empty_prefix() {
        let s = server_with(vec![], "");
        s.peers
            .register(PeerEntry {
                token: PeerToken::generate(),
                agent_id: AgentId::new("guest@local", [1u8; 32]),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        match s
            .op_respond(Request::RevokePeer {
                token_prefix: String::new(),
                force: false,
                match_limit: None,
            })
            .await
        {
            Response::Error { message } => assert!(message.contains("non-empty")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// Seed a `PeerEntry` whose `agent_id.pubkey` equals the daemon's
    /// own identity pubkey — i.e., the operator's bootstrap row as the
    /// daemon's `bootstrap_operator_token` would write it. The seeded
    /// entry's `agent_id.display` is `"operator@local"` to mirror the
    /// `boot_identity()` shape; for the self-revoke guard the predicate
    /// is identity-pubkey-centric so the display is irrelevant to the
    /// guard's decision but informative for the audit row payload.
    async fn seed_operator_self_entry(s: &Server) -> (PeerToken, [u8; 32]) {
        let token = PeerToken::generate();
        let op_pubkey = s.identity.agent_id().pubkey;
        s.peers
            .register(PeerEntry {
                token,
                agent_id: AgentId::new("operator@local", op_pubkey),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        (token, op_pubkey)
    }

    /// Self-revoke without `--force` returns `SelfRevokeForbidden`,
    /// leaves the registry unchanged, and emits a `PeerSelfRevokeBlocked`
    /// audit row whose issuer is the operator (peer-event audience).
    #[tokio::test]
    async fn revoke_peer_self_target_without_force_is_blocked() {
        let s = server_with(vec![], "");
        let (op_token, op_pubkey) = seed_operator_self_entry(&s).await;
        let prefix: String = op_token.to_b58().chars().take(6).collect();
        let resp = s
            .op_respond(Request::RevokePeer {
                token_prefix: prefix.clone(),
                force: false,
                match_limit: None,
            })
            .await;
        match resp {
            Response::PeerRevoked { outcome } => match outcome {
                RevokeOutcome::SelfRevokeForbidden(summary) => {
                    assert_eq!(summary.agent_id.pubkey, op_pubkey);
                    assert_eq!(summary.token_prefix, prefix);
                    assert!(summary.revoked_at.is_none(), "registry must be unchanged");
                }
                other => panic!("expected SelfRevokeForbidden, got {other:?}"),
            },
            other => panic!("expected PeerRevoked, got {other:?}"),
        }
        assert!(
            s.peers.resolve(&op_token).await.unwrap().is_some(),
            "operator token still resolves — registry was not mutated"
        );
        let events = s.audit.recent(50).await.unwrap();
        let row = events
            .iter()
            .find(|e| matches!(e.kind, AuditKind::PeerSelfRevokeBlocked { .. }))
            .expect("PeerSelfRevokeBlocked row");
        assert_eq!(
            row.issuer.pubkey,
            s.identity.agent_id().pubkey,
            "issuer is the operator (peer-event audience, not the daemon-issuer rejection model)"
        );
        match &row.kind {
            AuditKind::PeerSelfRevokeBlocked {
                peer_display,
                peer_pubkey_b58,
                token_prefix,
            } => {
                assert_eq!(peer_display, "operator@local");
                assert_eq!(peer_pubkey_b58, &bs58::encode(op_pubkey).into_string());
                assert_eq!(token_prefix, &prefix);
            }
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    /// `--force` overrides the self-revoke guard. Used by the
    /// operator's recovery-flow test (deliberately brick auth, then
    /// recover by deleting `peers/operator.token` and restarting the
    /// daemon). Emits the existing `PeerRevoked` audit row — no special
    /// "OperatorSelfRevokeForced" variant; correlation comes from
    /// `issuer.pubkey == peer_pubkey_b58` in the row payload.
    #[tokio::test]
    async fn revoke_peer_self_target_with_force_succeeds() {
        let s = server_with(vec![], "");
        let (op_token, op_pubkey) = seed_operator_self_entry(&s).await;
        let prefix: String = op_token.to_b58().chars().take(6).collect();
        let resp = s
            .op_respond(Request::RevokePeer {
                token_prefix: prefix.clone(),
                force: true,
                match_limit: None,
            })
            .await;
        match resp {
            Response::PeerRevoked { outcome } => match outcome {
                RevokeOutcome::Revoked(summary) => {
                    assert_eq!(summary.agent_id.pubkey, op_pubkey);
                    assert!(summary.revoked_at.is_some());
                }
                other => panic!("expected Revoked, got {other:?}"),
            },
            other => panic!("expected PeerRevoked, got {other:?}"),
        }
        assert_eq!(
            s.peers.resolve(&op_token).await.unwrap(),
            None,
            "force-revoked operator token no longer resolves"
        );
        let events = s.audit.recent(50).await.unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e.kind, AuditKind::PeerRevoked { .. })),
            "force-revoke emits the standard PeerRevoked row, not a special variant"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.kind, AuditKind::PeerSelfRevokeBlocked { .. })),
            "no PeerSelfRevokeBlocked row when force=true (the guard short-circuit was not taken)"
        );
    }

    /// Non-self targets are unaffected by the guard. A guest peer
    /// revoke with `force=false` proceeds normally — the standard
    /// revoke path is unchanged.
    #[tokio::test]
    async fn revoke_peer_non_self_target_unaffected_by_force_flag() {
        let s = server_with(vec![], "");
        let guest_token = PeerToken::generate();
        let guest_pubkey = [7u8; 32];
        assert_ne!(guest_pubkey, s.identity.agent_id().pubkey);
        s.peers
            .register(PeerEntry {
                token: guest_token,
                agent_id: AgentId::new("guest@local", guest_pubkey),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        let prefix: String = guest_token.to_b58().chars().take(6).collect();
        let resp = s
            .op_respond(Request::RevokePeer {
                token_prefix: prefix,
                force: false,
                match_limit: None,
            })
            .await;
        match resp {
            Response::PeerRevoked { outcome } => {
                assert!(matches!(outcome, RevokeOutcome::Revoked(_)));
            }
            other => panic!("expected PeerRevoked, got {other:?}"),
        }
    }

    /// `#[serde(default)]` regression: a `Request::RevokePeer` JSON
    /// without the `force` or `match_limit` fields deserialises with
    /// `force == false` (the safe default — guard fires) and
    /// `match_limit == None` (daemon falls back to its built-in
    /// constant). This is what a stale CLI built before either field
    /// landed produces; the new daemon must accept it.
    #[tokio::test]
    async fn revoke_peer_optional_fields_default_on_deserialize() {
        let json = r#"{"kind":"revoke_peer","token_prefix":"abcdef"}"#;
        let req: Request = serde_json::from_str(json).expect("stale-CLI frame deserialises");
        match req {
            Request::RevokePeer {
                token_prefix,
                force,
                match_limit,
            } => {
                assert_eq!(token_prefix, "abcdef");
                assert!(!force, "missing force field defaults to false");
                assert!(
                    match_limit.is_none(),
                    "missing match_limit field defaults to None"
                );
            }
            other => panic!("expected RevokePeer, got {other:?}"),
        }
    }

    /// Phase-1 forward-compat. The self-revoke guard's pubkey
    /// comparison MUST be operator-identity-centric, not caller-centric.
    /// In v0 these are equivalent because the only authenticated peer
    /// is the operator and its `peer.pubkey` always equals
    /// `self.identity.pubkey`. This test simulates the
    /// (impossible-in-v0) scenario where a non-operator caller's
    /// pubkey accidentally matches the matched-entry's pubkey: the
    /// guard must NOT fire for such a caller, because the rule is
    /// "you cannot revoke the *operator's* bootstrap row", not "you
    /// cannot revoke a row whose pubkey matches yours". The test
    /// exercises the C3 gate boundary; the inner guard is unreachable
    /// for non-operator callers because the C3 rejection happens
    /// first, and the assertion below documents that ordering as
    /// load-bearing for the multi-peer rewrite.
    #[tokio::test]
    async fn revoke_peer_self_target_uses_identity_pubkey_not_caller_pubkey() {
        let s = server_with(vec![], "");
        // A non-operator caller whose pubkey collides with a guest
        // entry's pubkey would, under G2 (caller-centric), have its
        // revoke blocked. Under G1 (identity-centric, what we want),
        // the request never reaches the guard — the C3 gate rejects it
        // first with `OperatorPeerRevokeRejected`.
        let collide_pubkey = [42u8; 32];
        s.peers
            .register(PeerEntry {
                token: PeerToken::generate(),
                agent_id: AgentId::new("guest@local", collide_pubkey),
                registered_at: epoch_ms(),
            })
            .await
            .unwrap();
        let foreign_caller = AgentId::new("foreign@local", collide_pubkey);
        match s
            .respond(
                Request::RevokePeer {
                    token_prefix: "anything".into(),
                    force: false,
                    match_limit: None,
                },
                &foreign_caller,
            )
            .await
        {
            Response::Error { message } => {
                assert!(message.contains("operator identity"));
            }
            other => panic!("expected C3 Error, got {other:?}"),
        }
        // The C3 rejection records `OperatorPeerRevokeRejected`
        // (daemon-issuer audience), NOT `PeerSelfRevokeBlocked` — the
        // ordering is what makes caller-centric and identity-centric
        // semantics indistinguishable in v0 from the caller's
        // perspective, but identity-centric is what the multi-peer
        // rewrite reads correctly without re-interpretation.
        let events = s.audit.recent(50).await.unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e.kind, AuditKind::OperatorPeerRevokeRejected { .. })),
            "C3 rejection row exists"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.kind, AuditKind::PeerSelfRevokeBlocked { .. })),
            "self-revoke guard never fired for a non-operator caller"
        );
    }

    /// Wire-format regression: `RevokeOutcome::SelfRevokeForbidden`
    /// carries a `PeerSummary` (token_prefix only, 6 chars), never the
    /// full base58 token. Mirrors the
    /// `revoke_peer_response_never_contains_full_token_b58` invariant.
    #[tokio::test]
    async fn self_revoke_forbidden_response_never_contains_full_token_b58() {
        let s = server_with(vec![], "");
        let (op_token, _) = seed_operator_self_entry(&s).await;
        let prefix: String = op_token.to_b58().chars().take(6).collect();
        let resp = s
            .op_respond(Request::RevokePeer {
                token_prefix: prefix,
                force: false,
                match_limit: None,
            })
            .await;
        let json = serde_json::to_string(&resp).expect("serialize PeerRevoked");
        let full_b58 = op_token.to_b58();
        assert!(
            !json.contains(&full_b58),
            "full token b58 must never appear in SelfRevokeForbidden wire payload"
        );
    }

    #[test]
    fn budget_seed_error_display_message_and_source_delegation_pin() {
        use covenant_budget::BudgetError;
        use std::error::Error;

        let err = BudgetSeedError {
            agent_id: "researcher@local".to_string(),
            source: BudgetError::NoCapacity("researcher@local".to_string()),
        };
        let message = format!("{err}");

        assert!(
            message.starts_with("seed budget for agent "),
            "BudgetSeedError Display must surface the literal 'seed budget for agent ' wrap-context prefix so operator dashboards can distinguish startup register_agent_budgets seed failures from in-flight intent budget rejections; a refactor that dropped the wrap context would silently merge the two operationally-distinct surfaces (dropped-wrap-prefix regression class): {message}"
        );
        assert!(
            message.contains("\"researcher@local\""),
            "BudgetSeedError Display must render the agent_id slot with Debug formatting ({{:?}}) so non-utf8 or control-byte agent names are escaped before reaching operator logs; a refactor that swapped {{agent_id:?}} for {{agent_id}} under a 'cleaner log output' rationale would silently un-escape malformed bytes (Debug-vs-Display formatting regression class on the agent_id interpolation): {message}"
        );
        assert!(
            message.contains("no capacity for researcher@local"),
            "BudgetSeedError Display must render the source slot with Display ({{0}}) so the inner BudgetError's operator-facing message (BudgetError::NoCapacity Display = 'no capacity for <agent>') surfaces verbatim; a refactor that swapped {{source}} for {{source:?}} under a 'preserve full error context' rationale would emit the BudgetError Debug rendering 'NoCapacity(\"researcher@local\")' instead (Debug-vs-Display formatting regression class on the source interpolation): {message}"
        );
        assert!(
            !message.contains("NoCapacity("),
            "BudgetSeedError Display must NOT surface the BudgetError Debug variant name 'NoCapacity('; a Debug refactor on the source interpolation would expose the bare variant identifier and leak internal struct shape into operator logs (Debug-vs-Display formatting regression class on the source interpolation): {message}"
        );

        let agent_idx = message
            .find("\"researcher@local\"")
            .expect("agent_id substring must appear");
        let source_idx = message
            .find("no capacity for researcher@local")
            .expect("source substring must appear");
        assert!(
            agent_idx < source_idx,
            "BudgetSeedError Display must render the agent_id slot BEFORE the source slot; a slot reorder (e.g., 'seed budget {{source}}: {{agent_id:?}}' under an 'alphabetize template variables' rationale) would silently swap operator dashboard columns between agent_id and the underlying budget rejection (slot-order regression class): agent_id at {agent_idx}, source at {source_idx}, message={message}"
        );

        let source = err
            .source()
            .expect("BudgetSeedError must surface the inner BudgetError via std::error::Error::source so anyhow chain printers and tracing's source-walking emitters can render the wrap context AND the inner cause (dropped-source-delegation regression class)");
        assert_eq!(
            format!("{source}"),
            format!(
                "{}",
                BudgetError::NoCapacity("researcher@local".to_string())
            ),
            "BudgetSeedError::source() must return the wrapped BudgetError so its Display rendering matches a direct format!('{{}}') of the same variant; a refactor that returned a different reference (e.g., &self or a stale clone) or implemented source() to return None would silently break anyhow-style chain printing (dropped-source-delegation regression class)"
        );
    }

    /// Generate a `PeerEntry` whose `token.to_b58()` starts with the
    /// supplied prefix. Random-rejection sampling — converges in ~58
    /// iterations per leading char of base58. Used by the daemon-side
    /// `revoke_peer_returns_ambiguous_when_prefix_matches_multiple`
    /// test to seed two peers with a deterministic shared prefix.
    fn peer_with_token_b58_starting_with(
        prefix: &str,
        name: &str,
        pubkey: [u8; 32],
    ) -> (PeerToken, PeerEntry) {
        for _ in 0..10_000 {
            let t = PeerToken::generate();
            if t.to_b58().starts_with(prefix) {
                let ent = PeerEntry {
                    token: t,
                    agent_id: AgentId::new(name, pubkey),
                    registered_at: epoch_ms(),
                };
                return (t, ent);
            }
        }
        panic!("could not find token starting with {prefix:?} after 10000 tries");
    }

    #[tokio::test]
    async fn server_preempt_intent_returns_not_in_flight_when_tracker_empty() {
        // The most basic preempt_intent contract: an intent_id that
        // was never registered (or already unregistered) must surface
        // as NotInFlight. A refactor that returned Preempted{outcome:
        // AlreadyDead} for the unknown case would be wrong — AlreadyDead
        // means "we tried to kill it and it was gone", which implies a
        // syscall happened. NotInFlight means no syscall.
        let server = server_with_ignore(vec![], "", IgnoreSet::default());
        let intent_id = Uuid::new_v4();
        let result = server
            .preempt_intent(
                intent_id,
                "test".into(),
                std::time::Duration::from_millis(100),
            )
            .await;
        assert!(
            matches!(result, PreemptResult::NotInFlight),
            "preempt_intent on an unknown intent_id must return NotInFlight; got {result:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn server_preempt_intent_kills_tracked_subprocess_and_emits_budget_preempted_audit() {
        // Spawn a real sleep subprocess (configured into its own
        // process group), register its pid in the Server's tracker
        // under a chosen intent_id, then call preempt_intent with a
        // short grace. The dispatcher SIGTERMs the group, the sleep
        // ignores cooperative termination only for the grace window,
        // so SIGKILL fires; the test asserts PreemptResult::Preempted
        // with outcome=SigKilled and that a BudgetPreempted audit
        // event was recorded under the chosen intent_id.
        use std::os::unix::process::CommandExt;
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let server = server_with_audit(audit.clone());

        let mut std_cmd = std::process::Command::new("sleep");
        std_cmd
            .arg("60")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0);
        let mut child = tokio::process::Command::from(std_cmd)
            .kill_on_drop(true)
            .spawn()
            .expect("spawn sleep");
        let pid = child.id().expect("child pid available before reap");

        let intent_id = Uuid::new_v4();
        server.subprocess_tracker().register(
            intent_id,
            covenant_runtime::TrackedSubprocess {
                agent_id: "tracked@local".into(),
                pid,
                started_at_ms: epoch_ms(),
            },
        );

        let (result, _exit) = tokio::join!(
            server.preempt_intent(
                intent_id,
                "test:budget_overshoot".into(),
                std::time::Duration::from_millis(250)
            ),
            child.wait(),
        );

        assert!(
            matches!(
                result,
                PreemptResult::Preempted {
                    outcome: covenant_runtime::PreemptOutcome::SigKilled
                        | covenant_runtime::PreemptOutcome::ExitedDuringGrace,
                }
            ),
            "preempt_intent on a tracked, alive subprocess must return Preempted with SigKilled or ExitedDuringGrace; got {result:?}"
        );

        let events = audit.recent(16).await.expect("audit recent must succeed");
        let found = events
            .iter()
            .filter(|e| {
                matches!(
                    &e.kind,
                    AuditKind::BudgetPreempted { intent_id: id, .. } if *id == intent_id
                )
            })
            .count();
        assert_eq!(
            found, 1,
            "preempt_intent must emit exactly one BudgetPreempted audit row keyed by the supplied intent_id; events seen: {events:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn server_preempt_intent_emits_signal_sent_sigterm_for_cooperative_exit() {
        // Bash subprocess installs a SIGTERM trap that exits cleanly,
        // then sleeps long enough that the preempt_subprocess_pg SIGTERM
        // is the one to terminate it. Asserts the daemon-layer mapping
        // PreemptOutcome::ExitedDuringGrace → BudgetPreempted{signal_sent="SIGTERM"}.
        // A refactor that lost the explicit ExitedDuringGrace arm
        // would either swallow the audit row or emit signal_sent="SIGKILL".
        use std::os::unix::process::CommandExt;
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let server = server_with_audit_and_budget(
            audit.clone(),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );

        // The trap is installed after a 50ms head-start so the
        // dispatcher's SIGTERM cannot race the trap installation. The
        // trap-armed exit must close stdin/stdout/stderr explicitly so
        // bash doesn't hold them past the trap fire. `exec 0<&-` etc.
        // keep the descriptors closed so .wait() returns promptly.
        let script = "sleep 0.05; trap 'exit 0' TERM; sleep 60";
        let mut std_cmd = std::process::Command::new("bash");
        std_cmd
            .arg("-c")
            .arg(script)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0);
        let mut child = tokio::process::Command::from(std_cmd)
            .kill_on_drop(true)
            .spawn()
            .expect("spawn bash trap subprocess");
        let pid = child.id().expect("child pid available before reap");

        // Wait long enough for the trap to install before preempt fires.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        let intent_id = Uuid::new_v4();
        server.subprocess_tracker().register(
            intent_id,
            covenant_runtime::TrackedSubprocess {
                agent_id: "cooperative@local".into(),
                pid,
                started_at_ms: epoch_ms(),
            },
        );

        let (result, _exit) = tokio::join!(
            server.preempt_intent(
                intent_id,
                "test:cooperative_exit".into(),
                std::time::Duration::from_millis(1_500)
            ),
            child.wait(),
        );

        assert!(
            matches!(
                result,
                PreemptResult::Preempted {
                    outcome: covenant_runtime::PreemptOutcome::ExitedDuringGrace,
                }
            ),
            "trap-cooperative subprocess must surface PreemptOutcome::ExitedDuringGrace; got {result:?}",
        );

        let events = audit.recent(32).await.expect("audit recent must succeed");
        let row = events
            .iter()
            .find_map(|e| match &e.kind {
                AuditKind::BudgetPreempted {
                    intent_id: id,
                    signal_sent,
                    ..
                } if *id == intent_id => Some(signal_sent.clone()),
                _ => None,
            })
            .unwrap_or_else(|| {
                panic!(
                    "expected exactly one BudgetPreempted row for intent_id {intent_id}; events seen: {events:?}"
                )
            });
        assert_eq!(
            row, "SIGTERM",
            "ExitedDuringGrace must map to signal_sent=\"SIGTERM\" so post-mortem can tell trap-cooperative exits from SIGKILL escalations",
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn server_preempt_intent_emits_signal_sent_none_for_already_dead_pid() {
        // Spawn a sleep subprocess, fully reap it, then register a
        // tracker entry pointing at the now-stale pid. preempt_intent
        // should map preempt_subprocess_pg's AlreadyDead (kill(-pid)
        // returns ESRCH on initial fire) to
        // BudgetPreempted{signal_sent="none"} — NOT BudgetPreemptFailed.
        // A refactor that treated ESRCH as a syscall error would
        // surface here as a missing BudgetPreempted row or a
        // BudgetPreemptFailed row.
        use std::os::unix::process::CommandExt;
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let server = server_with_audit_and_budget(
            audit.clone(),
            Arc::new(covenant_budget::InMemoryLedger::new()),
        );

        let mut std_cmd = std::process::Command::new("sleep");
        std_cmd
            .arg("0.01")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0);
        let mut child = tokio::process::Command::from(std_cmd)
            .kill_on_drop(true)
            .spawn()
            .expect("spawn sleep");
        let pid = child.id().expect("child pid available before reap");
        let _ = child.wait().await;

        // Sleep a beat to let the kernel finalize the process-table
        // cleanup, so kill(-pid, 0) actually returns ESRCH instead of
        // hitting a half-reaped zombie entry.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let intent_id = Uuid::new_v4();
        server.subprocess_tracker().register(
            intent_id,
            covenant_runtime::TrackedSubprocess {
                agent_id: "stale@local".into(),
                pid,
                started_at_ms: epoch_ms(),
            },
        );

        let result = server
            .preempt_intent(
                intent_id,
                "test:already_dead".into(),
                std::time::Duration::from_millis(100),
            )
            .await;

        assert!(
            matches!(
                result,
                PreemptResult::Preempted {
                    outcome: covenant_runtime::PreemptOutcome::AlreadyDead,
                }
            ),
            "already-reaped pid must surface PreemptOutcome::AlreadyDead, not SigKilled or PermissionDenied; got {result:?}",
        );

        let events = audit.recent(32).await.expect("audit recent must succeed");
        let row = events
            .iter()
            .find_map(|e| match &e.kind {
                AuditKind::BudgetPreempted {
                    intent_id: id,
                    signal_sent,
                    ..
                } if *id == intent_id => Some(signal_sent.clone()),
                AuditKind::BudgetPreemptFailed { intent_id: id, .. } if *id == intent_id => {
                    panic!(
                        "AlreadyDead must NOT map to BudgetPreemptFailed (ESRCH is benign — subprocess exited first); events seen: {events:?}"
                    )
                }
                _ => None,
            })
            .unwrap_or_else(|| {
                panic!(
                    "expected exactly one BudgetPreempted row for intent_id {intent_id}; events seen: {events:?}"
                )
            });
        assert_eq!(
            row, "none",
            "AlreadyDead must map to signal_sent=\"none\" so post-mortem distinguishes pre-dispatch exit from actively-signalled termination",
        );
    }

    fn server_with_audit_and_budget(
        audit: Arc<covenant_audit::InMemoryAuditLog>,
        budget: Arc<covenant_budget::InMemoryLedger>,
    ) -> Server {
        Server::new(
            Arc::new(Router::from_cards(vec![])),
            Arc::new(MockRunner::new("")),
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemorySettlement::new()),
            audit,
            Arc::new(covenant_permissions::InMemoryCapabilityStore::new()),
            Arc::new(covenant_llm::MockEmbedder::new(64)),
            Arc::new(LocalIdentity::generate("user@local")),
            Arc::new(IgnoreSet::default()),
            Arc::new(ToolRegistry::from_tools(vec![
                Arc::new(covenant_mcp::native::EchoTool),
                Arc::new(covenant_mcp::native::ClockTool),
            ])),
            Arc::new(covenant_a2a::InMemoryMailbox::new()),
            Arc::new(covenant_peer_auth::InMemoryPeerRegistry::new()),
            budget,
        )
    }

    #[tokio::test]
    async fn server_projection_tick_iteration_empty_tracker_returns_zero() {
        // Baseline: nothing in flight means nothing to preempt and no
        // audit emission. A refactor that emitted a heartbeat audit row
        // per tick would surface here as a non-empty audit log.
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let budget = Arc::new(covenant_budget::InMemoryLedger::new());
        let server = server_with_audit_and_budget(audit.clone(), budget);

        let count = server
            .run_projection_tick_iteration(std::time::Duration::from_millis(100))
            .await;

        assert_eq!(
            count, 0,
            "empty tracker must produce zero preempts; got {count}",
        );
        let events = audit.recent(8).await.expect("audit recent must succeed");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.kind, AuditKind::BudgetPreempted { .. })),
            "empty-tracker tick must not emit BudgetPreempted; got {events:?}",
        );
    }

    #[tokio::test]
    async fn server_projection_tick_iteration_skips_non_exhausted_agent() {
        // Tracker holds a live entry but the agent's bucket still has
        // tokens — the projection must not preempt. A refactor that
        // inverted the would_exceed branch (kill when budget OK) would
        // surface here as a Preempted return value and an audit row.
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let budget = Arc::new(covenant_budget::InMemoryLedger::new());
        let server = server_with_audit_and_budget(audit.clone(), budget.clone());

        let agent_card_id = "stillhealthy";
        let agent = agent_id_for_card_id(agent_card_id);
        budget
            .set_capacity(&agent, 1000)
            .await
            .expect("set_capacity must succeed");

        let intent_id = Uuid::new_v4();
        server.subprocess_tracker().register(
            intent_id,
            covenant_runtime::TrackedSubprocess {
                agent_id: agent_card_id.into(),
                // pid 0 is a sentinel: would_exceed must short-circuit
                // before the dispatcher ever reads it. If the test
                // observes a NotInFlight/AlreadyDead, the refactor
                // wired the kill path on a non-exhausted bucket.
                pid: 0,
                started_at_ms: epoch_ms(),
            },
        );

        let count = server
            .run_projection_tick_iteration(std::time::Duration::from_millis(100))
            .await;

        assert_eq!(
            count, 0,
            "non-exhausted agent must not be preempted; got {count}",
        );
        let events = audit.recent(8).await.expect("audit recent must succeed");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.kind, AuditKind::BudgetPreempted { .. })),
            "non-exhausted-agent tick must not emit BudgetPreempted; got {events:?}",
        );
    }

    #[test]
    fn projection_tick_config_from_values_uses_defaults_when_unset() {
        let config = projection_tick_config_from_values(None, None).expect("defaults");
        assert_eq!(config.period_ms, 250);
        assert_eq!(config.grace_ms, 2_000);
    }

    #[test]
    fn projection_tick_config_from_values_rejects_zero_period() {
        let err = projection_tick_config_from_values(Some("0"), None).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("COVENANT_BUDGET_PROJECTION_TICK_MS"),
            "rejection must name the offending env var so an operator can find it in logs: {err:?}",
        );
        assert!(
            msg.contains("greater than zero"),
            "rejection must say what the constraint is, not just that parsing failed: {err:?}",
        );
    }

    #[test]
    fn projection_tick_config_from_values_accepts_zero_grace() {
        // Zero grace is acceptable: preempt_subprocess_pg interprets it
        // as immediate SIGKILL (no SIGTERM-then-wait). A refactor that
        // bailed on zero grace would surface here as a returned Err.
        let config = projection_tick_config_from_values(Some("100"), Some("0"))
            .expect("zero grace must parse");
        assert_eq!(config.period_ms, 100);
        assert_eq!(config.grace_ms, 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_projection_tick_driver_preempts_exhausted_agent_on_first_tick() {
        // Spawn the driver against a Server holding one exhausted-budget
        // tracker entry that points at a real sleep subprocess. The
        // first interval.tick() fires immediately on tokio::time::interval
        // construction; the driver's first run_projection_tick_iteration
        // call observes the exhaustion and dispatches preempt_intent.
        // The test polls the audit log for the BudgetPreempted row up
        // to a deadline so kernel-scheduling jitter cannot flake it.
        use std::os::unix::process::CommandExt;
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let budget = Arc::new(covenant_budget::InMemoryLedger::new());
        let server = server_with_audit_and_budget(audit.clone(), budget.clone());

        let agent_card_id = "tickdrain";
        let agent = agent_id_for_card_id(agent_card_id);
        budget.set_capacity(&agent, 1).await.expect("set_capacity");
        budget
            .try_debit(&agent, 1, Uuid::new_v4())
            .await
            .expect("try_debit drains bucket");

        let mut std_cmd = std::process::Command::new("sleep");
        std_cmd
            .arg("60")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0);
        let mut child = tokio::process::Command::from(std_cmd)
            .kill_on_drop(true)
            .spawn()
            .expect("spawn sleep");
        let pid = child.id().expect("child pid available before reap");

        let intent_id = Uuid::new_v4();
        server.subprocess_tracker().register(
            intent_id,
            covenant_runtime::TrackedSubprocess {
                agent_id: agent_card_id.into(),
                pid,
                started_at_ms: epoch_ms(),
            },
        );

        let handle = spawn_projection_tick_driver(
            server.clone(),
            ProjectionTickConfig {
                period_ms: 50,
                grace_ms: 100,
            },
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let found = loop {
            let events = audit.recent(16).await.expect("audit recent");
            let count = events
                .iter()
                .filter(|e| {
                    matches!(
                        &e.kind,
                        AuditKind::BudgetPreempted { intent_id: id, .. } if *id == intent_id
                    )
                })
                .count();
            if count > 0 {
                break count;
            }
            if std::time::Instant::now() >= deadline {
                break 0;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        };

        handle.abort();
        let _ = child.wait().await;

        assert!(
            found > 0,
            "driver must dispatch at least one BudgetPreempted within the deadline; audit log: {:?}",
            audit.recent(16).await.expect("audit recent")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn server_projection_tick_iteration_preempts_exhausted_agent() {
        // Spawn a real sleep subprocess in its own process group,
        // register the entry, exhaust the agent's budget via try_debit,
        // then drive a single projection iteration. The tick must
        // observe would_exceed(agent, 1) == true and dispatch
        // preempt_intent, which signals the group and emits the
        // BudgetPreempted audit row. A refactor that dropped the
        // exhaustion->preempt arm would surface here as count == 0
        // and a missing audit row.
        use std::os::unix::process::CommandExt;
        let audit = Arc::new(covenant_audit::InMemoryAuditLog::new());
        let budget = Arc::new(covenant_budget::InMemoryLedger::new());
        let server = server_with_audit_and_budget(audit.clone(), budget.clone());

        let agent_card_id = "burnsdown";
        let agent = agent_id_for_card_id(agent_card_id);
        budget
            .set_capacity(&agent, 1)
            .await
            .expect("set_capacity must succeed");
        budget
            .try_debit(&agent, 1, Uuid::new_v4())
            .await
            .expect("try_debit must drain bucket");
        let exceed = budget
            .would_exceed(&agent, 1)
            .await
            .expect("would_exceed must succeed");
        assert!(
            exceed,
            "test precondition: bucket must be exhausted before driving the projection tick",
        );

        let mut std_cmd = std::process::Command::new("sleep");
        std_cmd
            .arg("60")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0);
        let mut child = tokio::process::Command::from(std_cmd)
            .kill_on_drop(true)
            .spawn()
            .expect("spawn sleep");
        let pid = child.id().expect("child pid available before reap");

        let intent_id = Uuid::new_v4();
        server.subprocess_tracker().register(
            intent_id,
            covenant_runtime::TrackedSubprocess {
                agent_id: agent_card_id.into(),
                pid,
                started_at_ms: epoch_ms(),
            },
        );

        let (count, _exit) = tokio::join!(
            server.run_projection_tick_iteration(std::time::Duration::from_millis(250)),
            child.wait(),
        );

        assert_eq!(
            count, 1,
            "exhausted agent with one tracked subprocess must produce exactly one preempt; got {count}",
        );

        let events = audit.recent(16).await.expect("audit recent must succeed");
        let preempted = events
            .iter()
            .filter_map(|e| match &e.kind {
                AuditKind::BudgetPreempted {
                    intent_id: id,
                    reason,
                    signal_sent,
                    ..
                } if *id == intent_id => Some((reason.clone(), signal_sent.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            preempted.len(),
            1,
            "projection tick must emit exactly one BudgetPreempted row for the killed intent; events seen: {events:?}",
        );
        let (reason, signal_sent) = preempted.into_iter().next().unwrap();
        assert_eq!(
            reason, "budget_overshoot",
            "projection tick must tag the preempt with reason=budget_overshoot so post-mortem can distinguish it from operator-driven preempts",
        );
        assert!(
            matches!(signal_sent.as_str(), "SIGTERM" | "SIGKILL" | "none"),
            "projection tick BudgetPreempted must carry a valid signal_sent variant; got {signal_sent:?}",
        );
    }

    #[tokio::test]
    async fn server_handle_purges_stream_tracker_entries_for_dropped_connection() {
        // Pre-register a synthetic entry under a chosen connection_id,
        // then call Server::handle with a UnixStream whose other end
        // has been closed. handle() must return cleanly (EOF on first
        // read), and the PurgeOnDrop guard must remove the entry.
        // A refactor that scoped the guard inside the auth-success
        // arm (instead of at the top of handle's body) would surface
        // here as a surviving entry on the unauthenticated-drop path.
        let server = server_with_ignore(vec![], "", IgnoreSet::default());
        let tracker = server.stream_tracker();

        let connection_id = Uuid::new_v4();
        let stream_id = Uuid::new_v4();
        tracker.register(
            connection_id,
            stream_id,
            stream_tracker::StreamEntry {
                verb: "synthetic".into(),
                schema: "covenant.ipc.v2.chunk.memory-record.v1".into(),
                started_at_ms: 0,
            },
        );
        assert_eq!(tracker.len(), 1);

        // Pair of connected UnixStreams. Dropping the client end
        // before the server reads makes the server see EOF on the
        // first frame; ProtocolInfo's pre-auth loop returns Ok in
        // that case.
        let (client, server_side) = tokio::net::UnixStream::pair().unwrap();
        drop(client);

        server
            .handle(connection_id, server_side)
            .await
            .expect("handle must return Ok on immediate EOF — pre-auth UnexpectedEof is the documented clean exit path");

        assert_eq!(
            tracker.len(),
            0,
            "PurgeOnDrop guard must drop the synthetic entry on every handle() exit path, including pre-auth EOF; a guard scoped inside the auth-success arm would leak entries when the client disconnects before authenticating"
        );
        assert_eq!(
            tracker.get(connection_id, stream_id),
            None,
            "the specific (connection_id, stream_id) tuple must be gone after handle returns; a refactor that called purge_connection on a different id would surface here"
        );
    }

    async fn server_with_authenticated_memory(records: usize) -> (Arc<Server>, PeerToken, AgentId) {
        let s = Arc::new(server_with_ignore(vec![], "", IgnoreSet::default()));
        grant_action(&s, "memory.read").await;
        let me = s.identity.agent_id();
        let token = PeerToken::generate();
        s.peers
            .register(PeerEntry {
                token,
                agent_id: me.clone(),
                registered_at: epoch_ms(),
            })
            .await
            .expect("register peer token");
        for i in 0..records as u8 {
            s.memory
                .put(MemoryRecord {
                    id: Uuid::from_bytes([i + 1; 16]),
                    tier: MemoryTier::Working,
                    owner: me.clone(),
                    text: format!("memory {i}"),
                    embedding: Vec::new(),
                    metadata: serde_json::json!({}),
                    created_at: 100 + i as u64,
                    parent: None,
                })
                .await
                .expect("put memory");
        }
        (s, token, me)
    }

    async fn authenticate_client(client: &mut tokio::net::UnixStream, token: PeerToken) {
        write_frame(
            client,
            &Request::Authenticate {
                token_b58: token.to_b58(),
            },
        )
        .await
        .expect("send authenticate");
        let resp: Response = read_frame(client).await.expect("read auth response");
        match resp {
            Response::Authenticated { .. } => {}
            other => panic!("expected Authenticated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_memory_with_prefer_stream_true_routes_to_streaming_path() {
        // ADR 0010 slice 3.d dispatch fork. With memory.read granted
        // and two records present, sending prefer_stream:Some(true)
        // must yield StreamBegin + 2 StreamChunk + StreamEnd
        // envelopes — NOT a Response::Memories terminal frame. A
        // regression that left the dispatch on the v1 path would
        // decode the first frame as Response::Memories and skip the
        // StreamEnvelope assertions.
        let (s, token, _me) = server_with_authenticated_memory(2).await;
        let (mut client, server_side) = tokio::net::UnixStream::pair().unwrap();
        let server_task = {
            let s = Arc::clone(&s);
            tokio::spawn(async move { s.handle(Uuid::new_v4(), server_side).await })
        };

        authenticate_client(&mut client, token).await;
        write_frame(
            &mut client,
            &Request::RecentMemory {
                tier: None,
                limit: 10,
                prefer_stream: Some(true),
            },
        )
        .await
        .expect("send recent_memory");

        let begin: StreamEnvelope = read_frame(&mut client).await.expect("read stream_begin");
        match begin {
            StreamEnvelope::StreamBegin { response_kind, .. } => {
                assert_eq!(response_kind, "memories");
            }
            other => panic!("expected StreamBegin, got {other:?}"),
        }
        for i in 0..2u32 {
            let chunk: StreamEnvelope = read_frame(&mut client).await.expect("read stream_chunk");
            match chunk {
                StreamEnvelope::StreamChunk { sequence, .. } => assert_eq!(sequence, i),
                other => panic!("expected StreamChunk at i={i}, got {other:?}"),
            }
        }
        let end: StreamEnvelope = read_frame(&mut client).await.expect("read stream_end");
        assert!(matches!(
            end,
            StreamEnvelope::StreamEnd { summary: None, .. }
        ));

        drop(client);
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn recent_memory_with_prefer_stream_omitted_returns_v1_response() {
        // v1 fixture replay protection: a request without
        // prefer_stream (Option<bool>::None) MUST receive a v1
        // Response::Memories terminal frame, byte-equivalent to
        // pre-ADR-0010 behavior. A dispatch that always streams
        // would break every existing IPC client.
        let (s, token, _me) = server_with_authenticated_memory(2).await;
        let (mut client, server_side) = tokio::net::UnixStream::pair().unwrap();
        let server_task = {
            let s = Arc::clone(&s);
            tokio::spawn(async move { s.handle(Uuid::new_v4(), server_side).await })
        };

        authenticate_client(&mut client, token).await;
        write_frame(
            &mut client,
            &Request::RecentMemory {
                tier: None,
                limit: 10,
                prefer_stream: None,
            },
        )
        .await
        .expect("send recent_memory");

        let resp: Response = read_frame(&mut client).await.expect("read response");
        match resp {
            Response::Memories { records } => assert_eq!(records.len(), 2),
            other => panic!("expected Response::Memories, got {other:?}"),
        }

        drop(client);
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn recent_memory_with_prefer_stream_false_returns_v1_response() {
        // ADR 0010 contract pin: prefer_stream:Some(false) is
        // wire-distinct from None and means "I know about v2
        // streaming but want the v1 shape this call". The dispatch
        // must match exactly Some(true), not .is_some() or
        // .unwrap_or(false), so Some(false) falls through to the v1
        // path. A regression that broadened the match would decode
        // the first frame as StreamEnvelope and fail Response decode.
        let (s, token, _me) = server_with_authenticated_memory(2).await;
        let (mut client, server_side) = tokio::net::UnixStream::pair().unwrap();
        let server_task = {
            let s = Arc::clone(&s);
            tokio::spawn(async move { s.handle(Uuid::new_v4(), server_side).await })
        };

        authenticate_client(&mut client, token).await;
        write_frame(
            &mut client,
            &Request::RecentMemory {
                tier: None,
                limit: 10,
                prefer_stream: Some(false),
            },
        )
        .await
        .expect("send recent_memory");

        let resp: Response = read_frame(&mut client).await.expect("read response");
        match resp {
            Response::Memories { records } => assert_eq!(records.len(), 2),
            other => panic!("expected Response::Memories on Some(false), got {other:?}"),
        }

        drop(client);
        let _ = server_task.await;
    }

    async fn server_with_authenticated_audit(events: usize) -> (Arc<Server>, PeerToken, AgentId) {
        let s = Arc::new(server_with_ignore(vec![], "", IgnoreSet::default()));
        let me = s.identity.agent_id();
        let token = PeerToken::generate();
        s.peers
            .register(PeerEntry {
                token,
                agent_id: me.clone(),
                registered_at: epoch_ms(),
            })
            .await
            .expect("register peer token");
        for i in 0..events as u8 {
            let event = AuditEvent {
                id: Uuid::from_bytes([i + 30; 16]),
                timestamp_ms: 1_700_000_000_000 + i as u64,
                issuer: me.clone(),
                kind: AuditKind::IntentDispatched {
                    intent_id: Uuid::from_bytes([i + 40; 16]),
                    intent_text: format!("intent {i}"),
                    matched_agent: Some("test-agent".into()),
                    result_hash_hex: format!("{:064x}", i as u64),
                    status: "ok".into(),
                },
            };
            s.record_peer_event(&me, event).await;
        }
        (s, token, me)
    }

    #[tokio::test]
    async fn recent_audit_with_prefer_stream_true_routes_to_streaming_path() {
        // ADR 0010 slice 4.d dispatch fork. With two audit events
        // recorded under the authenticated peer, sending
        // prefer_stream:Some(true) must yield StreamBegin (response_kind
        // "audit_events") + 2 StreamChunk + StreamEnd envelopes — NOT a
        // Response::AuditEvents terminal frame. A regression that left
        // the dispatch on the v1 path would decode the first frame as
        // Response::AuditEvents and skip the StreamEnvelope assertions.
        let (s, token, _me) = server_with_authenticated_audit(2).await;
        let (mut client, server_side) = tokio::net::UnixStream::pair().unwrap();
        let server_task = {
            let s = Arc::clone(&s);
            tokio::spawn(async move { s.handle(Uuid::new_v4(), server_side).await })
        };

        authenticate_client(&mut client, token).await;
        write_frame(
            &mut client,
            &Request::RecentAudit {
                limit: 10,
                since_ms: None,
                prefer_stream: Some(true),
            },
        )
        .await
        .expect("send recent_audit");

        let begin: StreamEnvelope = read_frame(&mut client).await.expect("read stream_begin");
        match begin {
            StreamEnvelope::StreamBegin { response_kind, .. } => {
                assert_eq!(response_kind, "audit_events");
            }
            other => panic!("expected StreamBegin, got {other:?}"),
        }
        for i in 0..2u32 {
            let chunk: StreamEnvelope = read_frame(&mut client).await.expect("read stream_chunk");
            match chunk {
                StreamEnvelope::StreamChunk { sequence, .. } => assert_eq!(sequence, i),
                other => panic!("expected StreamChunk at i={i}, got {other:?}"),
            }
        }
        let end: StreamEnvelope = read_frame(&mut client).await.expect("read stream_end");
        assert!(matches!(
            end,
            StreamEnvelope::StreamEnd { summary: None, .. }
        ));

        drop(client);
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn recent_audit_with_prefer_stream_omitted_returns_v1_response() {
        // v1 fixture replay protection: a request without prefer_stream
        // MUST receive a v1 Response::AuditEvents terminal frame,
        // byte-equivalent to pre-ADR-0010 behavior.
        let (s, token, _me) = server_with_authenticated_audit(2).await;
        let (mut client, server_side) = tokio::net::UnixStream::pair().unwrap();
        let server_task = {
            let s = Arc::clone(&s);
            tokio::spawn(async move { s.handle(Uuid::new_v4(), server_side).await })
        };

        authenticate_client(&mut client, token).await;
        write_frame(
            &mut client,
            &Request::RecentAudit {
                limit: 10,
                since_ms: None,
                prefer_stream: None,
            },
        )
        .await
        .expect("send recent_audit");

        let resp: Response = read_frame(&mut client).await.expect("read response");
        match resp {
            Response::AuditEvents { events } => assert_eq!(events.len(), 2),
            other => panic!("expected Response::AuditEvents, got {other:?}"),
        }

        drop(client);
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn recent_audit_with_prefer_stream_false_returns_v1_response() {
        // ADR 0010 contract pin: prefer_stream:Some(false) is
        // wire-distinct from None. The dispatch must match exactly
        // Some(true), so Some(false) falls through to the v1 path.
        let (s, token, _me) = server_with_authenticated_audit(2).await;
        let (mut client, server_side) = tokio::net::UnixStream::pair().unwrap();
        let server_task = {
            let s = Arc::clone(&s);
            tokio::spawn(async move { s.handle(Uuid::new_v4(), server_side).await })
        };

        authenticate_client(&mut client, token).await;
        write_frame(
            &mut client,
            &Request::RecentAudit {
                limit: 10,
                since_ms: None,
                prefer_stream: Some(false),
            },
        )
        .await
        .expect("send recent_audit");

        let resp: Response = read_frame(&mut client).await.expect("read response");
        match resp {
            Response::AuditEvents { events } => assert_eq!(events.len(), 2),
            other => panic!("expected Response::AuditEvents on Some(false), got {other:?}"),
        }

        drop(client);
        let _ = server_task.await;
    }

    async fn server_with_authenticated_intent() -> (Arc<Server>, PeerToken, AgentId) {
        let s = Arc::new(server_with_ignore(
            vec![stub_card("research", vec!["tool.web_search"])],
            "mocked summary",
            IgnoreSet::default(),
        ));
        grant_action(&s, "tool.web_search").await;
        grant_action(&s, "memory.write").await;
        let me = s.identity.agent_id();
        let token = PeerToken::generate();
        s.peers
            .register(PeerEntry {
                token,
                agent_id: me.clone(),
                registered_at: epoch_ms(),
            })
            .await
            .expect("register peer token");
        (s, token, me)
    }

    #[tokio::test]
    async fn submit_intent_with_prefer_stream_true_routes_to_streaming_path() {
        // ADR 0010 slice 5.d dispatch fork. With required grants in
        // place, sending prefer_stream:Some(true) must yield
        // StreamBegin (response_kind "intent_result") + 1 StreamChunk
        // + StreamEnd (with summary). A regression that left the
        // dispatch on the v1 path would decode the first frame as
        // Response::IntentResult and skip the StreamEnvelope path.
        let (s, token, _me) = server_with_authenticated_intent().await;
        let (mut client, server_side) = tokio::net::UnixStream::pair().unwrap();
        let server_task = {
            let s = Arc::clone(&s);
            tokio::spawn(async move { s.handle(Uuid::new_v4(), server_side).await })
        };

        authenticate_client(&mut client, token).await;
        write_frame(
            &mut client,
            &Request::SubmitIntent {
                text: "find recent papers on agent memory".into(),
                prefer_stream: Some(true),
            },
        )
        .await
        .expect("send submit_intent");

        let begin: StreamEnvelope = read_frame(&mut client).await.expect("read stream_begin");
        match begin {
            StreamEnvelope::StreamBegin { response_kind, .. } => {
                assert_eq!(response_kind, "intent_result");
            }
            other => panic!("expected StreamBegin, got {other:?}"),
        }
        let chunk: StreamEnvelope = read_frame(&mut client).await.expect("read stream_chunk");
        assert!(matches!(
            chunk,
            StreamEnvelope::StreamChunk { sequence: 0, .. }
        ));
        let end: StreamEnvelope = read_frame(&mut client).await.expect("read stream_end");
        match end {
            StreamEnvelope::StreamEnd { summary, .. } => {
                let s = summary.expect("StreamEnd.summary must carry IntentResult bookkeeping");
                assert_eq!(s["status"], "ok");
            }
            other => panic!("expected StreamEnd, got {other:?}"),
        }

        drop(client);
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn submit_intent_with_prefer_stream_omitted_returns_v1_response() {
        // v1 fixture replay protection: a request without prefer_stream
        // MUST receive a v1 Response::IntentResult terminal frame,
        // byte-equivalent to pre-ADR-0010 behavior.
        let (s, token, _me) = server_with_authenticated_intent().await;
        let (mut client, server_side) = tokio::net::UnixStream::pair().unwrap();
        let server_task = {
            let s = Arc::clone(&s);
            tokio::spawn(async move { s.handle(Uuid::new_v4(), server_side).await })
        };

        authenticate_client(&mut client, token).await;
        write_frame(
            &mut client,
            &Request::SubmitIntent {
                text: "find recent papers on agent memory".into(),
                prefer_stream: None,
            },
        )
        .await
        .expect("send submit_intent");

        let resp: Response = read_frame(&mut client).await.expect("read response");
        match resp {
            Response::IntentResult { text, status, .. } => {
                assert_eq!(text, "mocked summary");
                assert_eq!(status, "ok");
            }
            other => panic!("expected Response::IntentResult, got {other:?}"),
        }

        drop(client);
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn submit_intent_with_prefer_stream_false_returns_v1_response() {
        // ADR 0010 contract pin: prefer_stream:Some(false) is
        // wire-distinct from None. The dispatch must match exactly
        // Some(true), so Some(false) falls through to the v1 path.
        let (s, token, _me) = server_with_authenticated_intent().await;
        let (mut client, server_side) = tokio::net::UnixStream::pair().unwrap();
        let server_task = {
            let s = Arc::clone(&s);
            tokio::spawn(async move { s.handle(Uuid::new_v4(), server_side).await })
        };

        authenticate_client(&mut client, token).await;
        write_frame(
            &mut client,
            &Request::SubmitIntent {
                text: "find recent papers on agent memory".into(),
                prefer_stream: Some(false),
            },
        )
        .await
        .expect("send submit_intent");

        let resp: Response = read_frame(&mut client).await.expect("read response");
        match resp {
            Response::IntentResult { status, .. } => assert_eq!(status, "ok"),
            other => panic!("expected Response::IntentResult on Some(false), got {other:?}"),
        }

        drop(client);
        let _ = server_task.await;
    }

    fn pay_x402_req() -> Request {
        Request::PayX402 {
            provider: "xona".into(),
            endpoint: "https://example.test/endpoint".into(),
            method: "POST".into(),
            body: None,
            network: "solana:mainnet".into(),
            asset: "usdc-sol".into(),
            per_call_cap: "100000".into(),
            credits: 8,
        }
    }

    #[tokio::test]
    async fn pay_x402_rejects_when_capability_missing() {
        let s = server_with_audit(Arc::new(covenant_audit::InMemoryAuditLog::new()))
            .with_x402_dispatch(x402::X402Config {
                enabled: true,
                signer_binary: std::path::PathBuf::from("/bin/true"),
                signer_env: vec![],
            });
        let resp = s.op_respond(pay_x402_req()).await;
        match resp {
            Response::Error { message } => assert!(
                message.contains("x402.outbound.pay"),
                "error must name the missing capability so the operator can grant it: {message}"
            ),
            other => panic!("expected Error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn pay_x402_rejects_when_not_configured() {
        // Capability is granted, but no dispatch config wired — the
        // daemon must refuse rather than silently spending or
        // returning a generic error.
        let s = server_with_audit(Arc::new(covenant_audit::InMemoryAuditLog::new()));
        grant_action(&s, "x402.outbound.pay").await;
        let resp = s.op_respond(pay_x402_req()).await;
        match resp {
            Response::Error { message } => assert!(
                message.contains("not configured"),
                "error must say 'not configured' so the operator knows to call with_x402_dispatch: {message}"
            ),
            other => panic!("expected Error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn pay_x402_rejects_when_disabled() {
        // Capability granted + dispatch wired but `enabled: false`.
        // The operator might have temporarily disabled outbound
        // payments; the daemon must honour that flag.
        let s = server_with_audit(Arc::new(covenant_audit::InMemoryAuditLog::new()))
            .with_x402_dispatch(x402::X402Config::default());
        grant_action(&s, "x402.outbound.pay").await;
        let resp = s.op_respond(pay_x402_req()).await;
        match resp {
            Response::Error { message } => assert!(
                message.contains("disabled"),
                "error must clearly say 'disabled' so the operator knows to flip the flag: {message}"
            ),
            other => panic!("expected Error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn write_frame_error_writes_generic_message_for_frame_too_large() {
        // Client-distinguishability guard: handle() used to close the
        // connection on FrameTooLarge with no payload, so the client
        // could not tell a protocol violation from a transport reset.
        // write_frame_error now emits a Response::Error frame with a
        // generic message — generic so an unauthenticated peer cannot
        // probe internal byte counts via the wire shape.
        use covenant_ipc::read_frame;
        let (mut client, mut server) = tokio::io::duplex(1024);
        let err = IpcError::FrameTooLarge { got: 16_000_000 };
        Server::write_frame_error(Uuid::nil(), &mut server, &err).await;
        drop(server);
        let resp: Response = read_frame(&mut client)
            .await
            .expect("client must receive a framed Response::Error before EOF");
        match resp {
            Response::Error { message } => {
                assert_eq!(
                    message, "frame too large",
                    "frame-too-large arm must surface the generic message"
                );
                assert!(
                    !message.contains("16000000"),
                    "wire message must not echo the byte count: {message}"
                );
            }
            other => panic!("expected Response::Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn write_frame_error_writes_generic_message_for_serde_failure() {
        // Sibling guard: malformed-JSON failures also surface a
        // generic Response::Error so the client side branches the same
        // way the frame-size branch does, without leaking parse
        // position context.
        use covenant_ipc::read_frame;
        let (mut client, mut server) = tokio::io::duplex(1024);
        let serde_err = serde_json::from_str::<Request>("not json").expect_err("parse must fail");
        let err = IpcError::Serde(serde_err);
        Server::write_frame_error(Uuid::nil(), &mut server, &err).await;
        drop(server);
        let resp: Response = read_frame(&mut client)
            .await
            .expect("client must receive a framed Response::Error before EOF");
        match resp {
            Response::Error { message } => {
                assert_eq!(message, "malformed frame");
                assert!(!message.contains("expected"));
            }
            other => panic!("expected Response::Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn write_frame_error_skips_write_on_transport_io_failure() {
        // Io errors mean the socket is already torn — writing back
        // would be wasted work and could panic on a closed half. The
        // helper must short-circuit silently so the caller's error
        // propagates without a follow-on panic.
        let (mut client, mut server) = tokio::io::duplex(1024);
        let io_err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "peer reset");
        let err = IpcError::Io(io_err);
        Server::write_frame_error(Uuid::nil(), &mut server, &err).await;
        drop(server);
        let mut buf = [0u8; 1];
        let n = tokio::io::AsyncReadExt::read(&mut client, &mut buf)
            .await
            .expect("read should succeed");
        assert_eq!(n, 0, "no frame must have been written on the Io arm");
    }
}
