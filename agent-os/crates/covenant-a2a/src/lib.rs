//! Agent-to-agent task and result envelopes for Covenant.
//!
//! Defines the [`A2ATask`] and [`A2ATaskResult`] wire types, an async
//! [`Mailbox`] trait, and an in-memory implementation suitable for
//! tests and for orchestrator agents that fan tasks within a single
//! daemon process.
//!
//! [`A2ATask`] is a request from one agent to another; [`A2ATaskResult`]
//! is the response. Tasks form a tree via [`A2ATask::parent`] so an
//! orchestrator can fan a root intent across child agents and
//! reconstruct the result graph.

#![deny(unsafe_code)]

use async_trait::async_trait;
use covenant_mcp::Content;
use covenant_types::AgentId;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
// parking_lot::Mutex over std::sync::Mutex: no poison concept means a
// panic inside one Mailbox call cannot lock every subsequent caller out
// via PoisonError. The .lock() pattern that used to ride on
// each access is no longer needed — parking_lot's lock() returns the
// guard directly. Also a measurable perf win on contended paths.
use parking_lot::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex as AsyncMutex, Notify};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum A2ATaskStatus {
    Ok,
    Error,
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum A2ADuplicateSafety {
    Unsafe,
    Idempotent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct A2AIdempotency {
    pub duplicate_safety: A2ADuplicateSafety,
    pub key: String,
}

impl A2AIdempotency {
    pub fn new(duplicate_safety: A2ADuplicateSafety, key: impl Into<String>) -> Self {
        Self {
            duplicate_safety,
            key: key.into(),
        }
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2AIdempotencyCacheKey {
    pub sender_pubkey_b58: String,
    pub recipient_pubkey_b58: String,
    pub task_kind: String,
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct A2AIdempotencyCachedResult {
    pub source_task_id: Uuid,
    pub status: A2ATaskStatus,
    #[serde(default)]
    pub content: Vec<Content>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

impl A2AIdempotencyCachedResult {
    fn from_result(result: &A2ATaskResult) -> Self {
        Self {
            source_task_id: result.task_id,
            status: result.status.clone(),
            content: result.content.clone(),
            error_message: result.error_message.clone(),
        }
    }

    fn to_result(&self, task_id: Uuid) -> A2ATaskResult {
        A2ATaskResult {
            task_id,
            status: self.status.clone(),
            content: self.content.clone(),
            error_message: self.error_message.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct A2ATask {
    pub id: Uuid,
    pub sender: AgentId,
    pub recipient: AgentId,
    pub intent_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency: Option<A2AIdempotency>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum A2ATaskQueueState {
    Queued,
    InFlight,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct A2ATaskQueueEntry {
    pub state: A2ATaskQueueState,
    pub task: A2ATask,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leased_to: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leased_at_ms: Option<u64>,
    #[serde(default)]
    pub attempt: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum A2ADuplicateRisk {
    Idempotent,
    OperatorAccepted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum A2ARepairCommand {
    Requeue {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lease_id: Option<Uuid>,
        duplicate_risk: A2ADuplicateRisk,
    },
    ForceError {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lease_id: Option<Uuid>,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2ARepairRequest {
    pub task_id: Uuid,
    pub command: A2ARepairCommand,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum A2ARepairAction {
    Requeued,
    ForcedError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum A2ARepairState {
    Queued,
    ResultPending,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct A2ARepairOutcome {
    pub task_id: Uuid,
    pub action: A2ARepairAction,
    pub state: A2ARepairState,
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<A2ATaskResult>,
}

fn default_auto_retry_min_lease_age_ms() -> u64 {
    300_000
}

fn default_auto_retry_max_attempts() -> u32 {
    3
}

fn default_auto_retry_max_requeues() -> usize {
    1
}

fn default_auto_retry_scan_limit() -> usize {
    100
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct A2AAutoRetryPolicy {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_auto_retry_min_lease_age_ms")]
    pub min_lease_age_ms: u64,
    #[serde(default = "default_auto_retry_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_auto_retry_max_requeues")]
    pub max_requeues: usize,
    #[serde(default = "default_auto_retry_scan_limit")]
    pub scan_limit: usize,
}

impl Default for A2AAutoRetryPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            min_lease_age_ms: default_auto_retry_min_lease_age_ms(),
            max_attempts: default_auto_retry_max_attempts(),
            max_requeues: default_auto_retry_max_requeues(),
            scan_limit: default_auto_retry_scan_limit(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum A2AAutoRetrySkipReason {
    Disabled,
    NotInFlight,
    MissingLease,
    LeaseTooYoung,
    MissingIdempotency,
    UnsafeDuplicateSafety,
    MaxAttemptsReached,
    LimitReached,
    CapabilityScopeMismatch,
}

impl A2AAutoRetrySkipReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::NotInFlight => "not_in_flight",
            Self::MissingLease => "missing_lease",
            Self::LeaseTooYoung => "lease_too_young",
            Self::MissingIdempotency => "missing_idempotency",
            Self::UnsafeDuplicateSafety => "unsafe_duplicate_safety",
            Self::MaxAttemptsReached => "max_attempts_reached",
            Self::LimitReached => "limit_reached",
            Self::CapabilityScopeMismatch => "capability_scope_mismatch",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2AAutoRetrySkipped {
    pub task_id: Uuid,
    pub reason: A2AAutoRetrySkipReason,
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_age_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2AAutoRetryRequeued {
    pub task_id: Uuid,
    pub lease_id: Uuid,
    pub attempt: u32,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2AAutoRetryReport {
    pub policy: A2AAutoRetryPolicy,
    pub considered: usize,
    #[serde(default)]
    pub requeued: Vec<A2AAutoRetryRequeued>,
    #[serde(default)]
    pub skipped: Vec<A2AAutoRetrySkipped>,
}

impl A2AAutoRetryReport {
    pub fn new(policy: A2AAutoRetryPolicy) -> Self {
        Self {
            policy,
            considered: 0,
            requeued: Vec::new(),
            skipped: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum A2AAutoRetryDecision {
    Requeue {
        lease_id: Uuid,
        lease_age_ms: u64,
        idempotency_key: String,
    },
    Skip {
        reason: A2AAutoRetrySkipReason,
        lease_age_ms: Option<u64>,
    },
}

pub fn evaluate_auto_retry(
    entry: &A2ATaskQueueEntry,
    policy: &A2AAutoRetryPolicy,
    now_ms: u64,
) -> A2AAutoRetryDecision {
    if !policy.enabled {
        return A2AAutoRetryDecision::Skip {
            reason: A2AAutoRetrySkipReason::Disabled,
            lease_age_ms: None,
        };
    }

    if entry.state != A2ATaskQueueState::InFlight {
        return A2AAutoRetryDecision::Skip {
            reason: A2AAutoRetrySkipReason::NotInFlight,
            lease_age_ms: None,
        };
    }

    let (Some(lease_id), Some(leased_at_ms)) = (entry.lease_id, entry.leased_at_ms) else {
        return A2AAutoRetryDecision::Skip {
            reason: A2AAutoRetrySkipReason::MissingLease,
            lease_age_ms: None,
        };
    };
    let lease_age_ms = now_ms.saturating_sub(leased_at_ms);
    if lease_age_ms < policy.min_lease_age_ms {
        return A2AAutoRetryDecision::Skip {
            reason: A2AAutoRetrySkipReason::LeaseTooYoung,
            lease_age_ms: Some(lease_age_ms),
        };
    }

    let Some(idempotency) = &entry.task.idempotency else {
        return A2AAutoRetryDecision::Skip {
            reason: A2AAutoRetrySkipReason::MissingIdempotency,
            lease_age_ms: Some(lease_age_ms),
        };
    };
    if idempotency.key.trim().is_empty() {
        return A2AAutoRetryDecision::Skip {
            reason: A2AAutoRetrySkipReason::MissingIdempotency,
            lease_age_ms: Some(lease_age_ms),
        };
    }
    if idempotency.duplicate_safety != A2ADuplicateSafety::Idempotent {
        return A2AAutoRetryDecision::Skip {
            reason: A2AAutoRetrySkipReason::UnsafeDuplicateSafety,
            lease_age_ms: Some(lease_age_ms),
        };
    }
    if entry.attempt >= policy.max_attempts {
        return A2AAutoRetryDecision::Skip {
            reason: A2AAutoRetrySkipReason::MaxAttemptsReached,
            lease_age_ms: Some(lease_age_ms),
        };
    }

    A2AAutoRetryDecision::Requeue {
        lease_id,
        lease_age_ms,
        idempotency_key: idempotency.key.clone(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct A2ATaskResult {
    pub task_id: Uuid,
    pub status: A2ATaskStatus,
    #[serde(default)]
    pub content: Vec<Content>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

impl A2ATaskResult {
    pub fn ok(task_id: Uuid, content: Vec<Content>) -> Self {
        Self {
            task_id,
            status: A2ATaskStatus::Ok,
            content,
            error_message: None,
        }
    }

    pub fn error(task_id: Uuid, message: impl Into<String>) -> Self {
        Self {
            task_id,
            status: A2ATaskStatus::Error,
            content: Vec::new(),
            error_message: Some(message.into()),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum A2AError {
    #[error("mailbox closed")]
    Closed,
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("task {0} is not currently leased")]
    TaskNotInFlight(Uuid),
    #[error("lease mismatch for task {task_id}: expected {expected:?}, actual {actual:?}")]
    LeaseMismatch {
        task_id: Uuid,
        expected: Option<Uuid>,
        actual: Option<Uuid>,
    },
    #[error("invalid task: {0}")]
    InvalidTask(String),
    #[error("invalid repair request: {0}")]
    InvalidRepair(String),
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn validate_repair_request(request: &A2ARepairRequest) -> Result<(), A2AError> {
    if request.reason.trim().is_empty() {
        return Err(A2AError::InvalidRepair("reason must not be empty".into()));
    }
    if let A2ARepairCommand::ForceError { message, .. } = &request.command {
        if message.trim().is_empty() {
            return Err(A2AError::InvalidRepair(
                "force_error message must not be empty".into(),
            ));
        }
    }
    Ok(())
}

fn validate_task(task: &A2ATask) -> Result<(), A2AError> {
    if task
        .task_kind
        .as_deref()
        .is_some_and(|kind| kind.trim().is_empty())
    {
        return Err(A2AError::InvalidTask(
            "task_kind must not be empty when present".into(),
        ));
    }
    if let Some(idempotency) = &task.idempotency {
        if idempotency.key.trim().is_empty() {
            return Err(A2AError::InvalidTask(
                "idempotency key must not be empty".into(),
            ));
        }
    }
    Ok(())
}

fn idempotency_cache_key(task: &A2ATask) -> Option<A2AIdempotencyCacheKey> {
    let idempotency = task.idempotency.as_ref()?;
    if idempotency.duplicate_safety != A2ADuplicateSafety::Idempotent {
        return None;
    }
    let key = idempotency.key.trim();
    if key.is_empty() {
        return None;
    }
    Some(A2AIdempotencyCacheKey {
        sender_pubkey_b58: task.sender.pubkey_base58(),
        recipient_pubkey_b58: task.recipient.pubkey_base58(),
        task_kind: task
            .task_kind
            .as_deref()
            .map(str::trim)
            .filter(|kind| !kind.is_empty())
            .unwrap_or(&task.intent_text)
            .to_owned(),
        key: key.to_owned(),
    })
}

fn assert_lease_match(
    task_id: Uuid,
    expected: Option<Uuid>,
    actual: Option<Uuid>,
) -> Result<(), A2AError> {
    if expected.is_some() && expected != actual {
        return Err(A2AError::LeaseMismatch {
            task_id,
            expected,
            actual,
        });
    }
    Ok(())
}

/// Per-agent inbox. Tasks land in `recv_task`; results for tasks the agent
/// itself dispatched land in `recv_result`. Both `recv_*` are async and
/// resolve when something is available; impl is responsible for fairness.
#[async_trait]
pub trait Mailbox: Send + Sync {
    async fn send_task(&self, task: A2ATask) -> Result<(), A2AError>;
    async fn recv_task(&self) -> Result<A2ATask, A2AError>;
    /// Non-blocking, peer-scoped recv for the RPC-style daemon transports.
    /// Returns the oldest queued task whose `recipient` equals `recipient`,
    /// or `None` if no matching task is queued. Mailbox state is shared
    /// across peers, but each peer only observes — and only consumes —
    /// tasks addressed to it.
    async fn try_recv_task_for(&self, recipient: &AgentId) -> Result<Option<A2ATask>, A2AError>;
    async fn send_result(&self, result: A2ATaskResult) -> Result<(), A2AError>;
    async fn recv_result(&self) -> Result<A2ATaskResult, A2AError>;
    /// Non-blocking, peer-scoped result recv. Returns the oldest queued
    /// result whose underlying task's `sender` equals `peer` — i.e., the
    /// peer that originally dispatched the task is the one that can drain
    /// its response. Look up uses the senders map maintained by
    /// `send_task`; results whose `task_id` is unknown to the senders map
    /// are skipped (never returned to any peer).
    async fn try_recv_result_for(&self, peer: &AgentId) -> Result<Option<A2ATaskResult>, A2AError>;

    /// Read-only snapshot of the most recent queued tasks, oldest first up
    /// to `limit`. Does not consume from the queue. Operator-facing.
    async fn recent_tasks(&self, limit: usize) -> Result<Vec<A2ATask>, A2AError>;
    /// Read-only snapshot of queued and leased tasks. A leased task has
    /// been delivered to a recipient but has not produced a result yet.
    /// Leases survive daemon restart and are not automatically redelivered.
    async fn task_queue(&self, limit: usize) -> Result<Vec<A2ATaskQueueEntry>, A2AError>;
    /// Operator-controlled repair path for an in-flight lease. Repair
    /// commands never run automatically; callers must provide a reason
    /// and, for requeue, an explicit duplicate-work risk posture.
    async fn repair_task(&self, request: A2ARepairRequest) -> Result<A2ARepairOutcome, A2AError>;
    /// Read-only snapshot of the most recent queued results, oldest first
    /// up to `limit`. Does not consume from the queue. Operator-facing.
    async fn recent_results(&self, limit: usize) -> Result<Vec<A2ATaskResult>, A2AError>;

    /// Look up the original sender for a task that was previously
    /// dispatched through [`Mailbox::send_task`]. Returns `None` for any
    /// `task_id` the mailbox has never seen. Used by the daemon to gate
    /// `PostA2AResult` on the sender-scoped `a2a.respond.<sender>`
    /// capability.
    async fn lookup_task_sender(&self, task_id: Uuid) -> Result<Option<AgentId>, A2AError>;

    /// Drop dead state from the underlying event log so the on-disk
    /// representation stays bounded over a long-running daemon. Returns
    /// the number of log events removed (zero if nothing was eligible
    /// or the implementation has no on-disk log).
    ///
    /// "Dead state" is operator-driven and conservatively defined: a
    /// `task_id` is droppable iff (a) it has been received (`TaskRecv`
    /// in the log) AND (b) at least one result has been posted for it
    /// AND (c) every posted result has a matching `ResultRecv`. Any
    /// task still queued, drained-but-no-result-yet, or with results
    /// that have not been drained stays in the log. Fire-and-forget
    /// tasks (no result ever posted) are never droppable in v0 — a
    /// future timestamp-aware compaction mode would close that gap.
    async fn compact(&self) -> Result<u64, A2AError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskLease {
    lease_id: Uuid,
    task: A2ATask,
    leased_to: AgentId,
    leased_at_ms: u64,
    attempt: u32,
}

impl A2ATaskQueueEntry {
    fn queued(task: A2ATask) -> Self {
        Self {
            state: A2ATaskQueueState::Queued,
            task,
            lease_id: None,
            leased_to: None,
            leased_at_ms: None,
            attempt: 0,
        }
    }

    fn in_flight(lease: TaskLease) -> Self {
        Self {
            state: A2ATaskQueueState::InFlight,
            task: lease.task,
            lease_id: Some(lease.lease_id),
            leased_to: Some(lease.leased_to),
            leased_at_ms: Some(lease.leased_at_ms),
            attempt: lease.attempt,
        }
    }
}

/// In-process FIFO mailbox. Useful for tests and for orchestrator agents
/// that fan tasks within the same daemon.
pub struct InMemoryMailbox {
    tasks: Mutex<VecDeque<A2ATask>>,
    results: Mutex<VecDeque<A2ATaskResult>>,
    in_flight: Mutex<HashMap<Uuid, TaskLease>>,
    attempts: Mutex<HashMap<Uuid, u32>>,
    /// Permanent record of who sent each task, populated on
    /// [`Mailbox::send_task`] and never pruned. The daemon uses this map
    /// to attribute `PostA2AResult` calls back to the original sender so
    /// the capability check can use the sender-scoped action.
    senders: Mutex<HashMap<Uuid, AgentId>>,
    result_cache: Mutex<HashMap<A2AIdempotencyCacheKey, A2AIdempotencyCachedResult>>,
    task_notify: Notify,
    result_notify: Notify,
}

impl Default for InMemoryMailbox {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryMailbox {
    pub fn new() -> Self {
        Self {
            tasks: Mutex::new(VecDeque::new()),
            results: Mutex::new(VecDeque::new()),
            in_flight: Mutex::new(HashMap::new()),
            attempts: Mutex::new(HashMap::new()),
            senders: Mutex::new(HashMap::new()),
            result_cache: Mutex::new(HashMap::new()),
            task_notify: Notify::new(),
            result_notify: Notify::new(),
        }
    }

    fn lease_task(&self, task: A2ATask, leased_to: AgentId) -> A2ATask {
        let attempt = {
            let mut attempts = self.attempts.lock();
            let attempt = attempts
                .get(&task.id)
                .copied()
                .unwrap_or(0)
                .saturating_add(1);
            attempts.insert(task.id, attempt);
            attempt
        };
        let lease = TaskLease {
            lease_id: Uuid::new_v4(),
            task: task.clone(),
            leased_to,
            leased_at_ms: epoch_ms(),
            attempt,
        };
        self.in_flight.lock().insert(task.id, lease);
        task
    }

    fn queued_entry(&self, task: A2ATask) -> A2ATaskQueueEntry {
        let attempt = self
            .attempts
            .lock()
            .get(&task.id)
            .copied()
            .unwrap_or(0);
        A2ATaskQueueEntry {
            attempt,
            ..A2ATaskQueueEntry::queued(task)
        }
    }
}

#[async_trait]
impl Mailbox for InMemoryMailbox {
    async fn send_task(&self, task: A2ATask) -> Result<(), A2AError> {
        validate_task(&task)?;
        if let Some(cached) = idempotency_cache_key(&task)
            .and_then(|key| self.result_cache.lock().get(&key).cloned())
        {
            let result = cached.to_result(task.id);
            self.senders
                .lock()
                .insert(task.id, task.sender.clone());
            self.attempts.lock().entry(task.id).or_insert(0);
            self.results.lock().push_back(result);
            self.result_notify.notify_one();
            return Ok(());
        }
        self.senders
            .lock()
            .insert(task.id, task.sender.clone());
        self.attempts.lock().entry(task.id).or_insert(0);
        self.tasks.lock().push_back(task);
        self.task_notify.notify_one();
        Ok(())
    }

    async fn recv_task(&self) -> Result<A2ATask, A2AError> {
        loop {
            if let Some(t) = self.tasks.lock().pop_front() {
                return Ok(self.lease_task(t.clone(), t.recipient.clone()));
            }
            self.task_notify.notified().await;
        }
    }

    async fn try_recv_task_for(&self, recipient: &AgentId) -> Result<Option<A2ATask>, A2AError> {
        let mut tasks = self.tasks.lock();
        let Some(pos) = tasks.iter().position(|t| t.recipient == *recipient) else {
            return Ok(None);
        };
        let task = tasks.remove(pos);
        drop(tasks);
        Ok(task.map(|t| self.lease_task(t, recipient.clone())))
    }

    async fn send_result(&self, result: A2ATaskResult) -> Result<(), A2AError> {
        let completed = self.in_flight.lock().remove(&result.task_id);
        if let Some(lease) = completed {
            if let Some(key) = idempotency_cache_key(&lease.task) {
                self.result_cache
                    .lock()
                    .insert(key, A2AIdempotencyCachedResult::from_result(&result));
            }
        }
        self.results.lock().push_back(result);
        self.result_notify.notify_one();
        Ok(())
    }

    async fn recv_result(&self) -> Result<A2ATaskResult, A2AError> {
        loop {
            if let Some(r) = self.results.lock().pop_front() {
                return Ok(r);
            }
            self.result_notify.notified().await;
        }
    }

    async fn try_recv_result_for(&self, peer: &AgentId) -> Result<Option<A2ATaskResult>, A2AError> {
        let senders = self.senders.lock();
        let mut results = self.results.lock();
        let pos = results
            .iter()
            .position(|r| senders.get(&r.task_id).map(|s| s == peer).unwrap_or(false));
        Ok(pos.and_then(|p| results.remove(p)))
    }

    async fn recent_tasks(&self, limit: usize) -> Result<Vec<A2ATask>, A2AError> {
        Ok(self
            .tasks
            .lock()
            .iter()
            .take(limit)
            .cloned()
            .collect())
    }

    async fn task_queue(&self, limit: usize) -> Result<Vec<A2ATaskQueueEntry>, A2AError> {
        let mut entries: Vec<A2ATaskQueueEntry> = self
            .tasks
            .lock()
            .iter()
            .cloned()
            .map(|task| self.queued_entry(task))
            .collect();
        let mut leased: Vec<A2ATaskQueueEntry> = self
            .in_flight
            .lock()
            .values()
            .cloned()
            .map(A2ATaskQueueEntry::in_flight)
            .collect();
        leased.sort_by(|a, b| {
            a.leased_at_ms
                .cmp(&b.leased_at_ms)
                .then_with(|| a.task.id.cmp(&b.task.id))
        });
        entries.extend(leased);
        entries.truncate(limit);
        Ok(entries)
    }

    async fn repair_task(&self, request: A2ARepairRequest) -> Result<A2ARepairOutcome, A2AError> {
        validate_repair_request(&request)?;

        match request.command {
            A2ARepairCommand::Requeue { lease_id, .. } => {
                let lease = {
                    let mut in_flight = self.in_flight.lock();
                    let lease = in_flight
                        .get(&request.task_id)
                        .cloned()
                        .ok_or(A2AError::TaskNotInFlight(request.task_id))?;
                    assert_lease_match(request.task_id, lease_id, Some(lease.lease_id))?;
                    in_flight.remove(&request.task_id);
                    lease
                };
                self.tasks.lock().push_back(lease.task);
                self.task_notify.notify_one();
                Ok(A2ARepairOutcome {
                    task_id: request.task_id,
                    action: A2ARepairAction::Requeued,
                    state: A2ARepairState::Queued,
                    attempt: lease.attempt,
                    result: None,
                })
            }
            A2ARepairCommand::ForceError { lease_id, message } => {
                let lease = {
                    let mut in_flight = self.in_flight.lock();
                    let lease = in_flight
                        .get(&request.task_id)
                        .cloned()
                        .ok_or(A2AError::TaskNotInFlight(request.task_id))?;
                    assert_lease_match(request.task_id, lease_id, Some(lease.lease_id))?;
                    in_flight.remove(&request.task_id);
                    lease
                };
                let result = A2ATaskResult::error(request.task_id, message);
                self.results.lock().push_back(result.clone());
                self.result_notify.notify_one();
                Ok(A2ARepairOutcome {
                    task_id: request.task_id,
                    action: A2ARepairAction::ForcedError,
                    state: A2ARepairState::ResultPending,
                    attempt: lease.attempt,
                    result: Some(result),
                })
            }
        }
    }

    async fn recent_results(&self, limit: usize) -> Result<Vec<A2ATaskResult>, A2AError> {
        Ok(self
            .results
            .lock()
            .iter()
            .take(limit)
            .cloned()
            .collect())
    }

    async fn lookup_task_sender(&self, task_id: Uuid) -> Result<Option<AgentId>, A2AError> {
        Ok(self.senders.lock().get(&task_id).cloned())
    }

    async fn compact(&self) -> Result<u64, A2AError> {
        // No on-disk event log to compact. The senders map could in
        // principle be pruned for fully-resolved tasks, but this impl
        // is for tests and short-lived in-process orchestrators where
        // the map's growth is bounded by the test's lifetime.
        Ok(0)
    }
}

/// Append-only event log of mailbox state transitions, written one
/// JSON line at a time. [`JsonlMailbox::open`] replays the log to
/// rebuild in-memory state, so a daemon restart does not drop queued
/// tasks, queued results, or the senders map used by the
/// `a2a.respond.<sender>` capability check.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum MailboxEvent {
    TaskSent {
        task: A2ATask,
    },
    TaskRecv {
        task_id: Uuid,
    },
    TaskLeased {
        task_id: Uuid,
        lease_id: Uuid,
        leased_to: AgentId,
        leased_at_ms: u64,
        attempt: u32,
    },
    TaskRequeued {
        task_id: Uuid,
        lease_id: Uuid,
        reason: String,
        duplicate_risk: A2ADuplicateRisk,
        requeued_at_ms: u64,
        attempt: u32,
    },
    TaskForceErrored {
        task_id: Uuid,
        lease_id: Uuid,
        result: A2ATaskResult,
        reason: String,
        forced_at_ms: u64,
        attempt: u32,
    },
    IdempotencyResultCached {
        cache_key: A2AIdempotencyCacheKey,
        result: A2AIdempotencyCachedResult,
    },
    IdempotencyResultReplayed {
        task: A2ATask,
        result: A2ATaskResult,
    },
    ResultPosted {
        result: A2ATaskResult,
    },
    ResultRecv {
        task_id: Uuid,
    },
}

struct MailboxState {
    tasks: VecDeque<A2ATask>,
    results: VecDeque<A2ATaskResult>,
    in_flight: HashMap<Uuid, TaskLease>,
    senders: HashMap<Uuid, AgentId>,
    attempts: HashMap<Uuid, u32>,
    result_cache: HashMap<A2AIdempotencyCacheKey, A2AIdempotencyCachedResult>,
}

impl MailboxState {
    fn empty() -> Self {
        Self {
            tasks: VecDeque::new(),
            results: VecDeque::new(),
            in_flight: HashMap::new(),
            senders: HashMap::new(),
            attempts: HashMap::new(),
            result_cache: HashMap::new(),
        }
    }

    fn apply(&mut self, ev: MailboxEvent) {
        match ev {
            MailboxEvent::TaskSent { task } => {
                self.senders.insert(task.id, task.sender.clone());
                self.attempts.entry(task.id).or_insert(0);
                self.tasks.push_back(task);
            }
            MailboxEvent::TaskRecv { task_id } => {
                self.lease_task(task_id, Uuid::nil(), None, 0, self.next_attempt(task_id));
            }
            MailboxEvent::TaskLeased {
                task_id,
                lease_id,
                leased_to,
                leased_at_ms,
                attempt,
            } => {
                self.lease_task(task_id, lease_id, Some(leased_to), leased_at_ms, attempt);
            }
            MailboxEvent::TaskRequeued {
                task_id, attempt, ..
            } => {
                self.requeue_task(task_id, attempt);
            }
            MailboxEvent::TaskForceErrored {
                task_id,
                result,
                attempt,
                ..
            } => {
                self.in_flight.remove(&task_id);
                self.attempts.insert(task_id, attempt);
                self.results.push_back(result);
            }
            MailboxEvent::IdempotencyResultCached { cache_key, result } => {
                self.result_cache.insert(cache_key, result);
            }
            MailboxEvent::IdempotencyResultReplayed { task, result } => {
                self.senders.insert(task.id, task.sender.clone());
                self.attempts.entry(task.id).or_insert(0);
                self.results.push_back(result);
            }
            MailboxEvent::ResultPosted { result } => {
                self.in_flight.remove(&result.task_id);
                self.results.push_back(result);
            }
            MailboxEvent::ResultRecv { task_id } => {
                if let Some(pos) = self.results.iter().position(|r| r.task_id == task_id) {
                    self.results.remove(pos);
                }
            }
        }
    }

    fn next_attempt(&self, task_id: Uuid) -> u32 {
        self.attempts
            .get(&task_id)
            .copied()
            .unwrap_or(0)
            .saturating_add(1)
    }

    fn lease_task(
        &mut self,
        task_id: Uuid,
        lease_id: Uuid,
        leased_to: Option<AgentId>,
        leased_at_ms: u64,
        attempt: u32,
    ) -> Option<A2ATask> {
        let pos = self.tasks.iter().position(|t| t.id == task_id)?;
        let task = self.tasks.remove(pos)?;
        let lease = TaskLease {
            lease_id,
            leased_to: leased_to.unwrap_or_else(|| task.recipient.clone()),
            leased_at_ms,
            attempt,
            task: task.clone(),
        };
        self.in_flight.insert(task_id, lease);
        self.attempts.insert(task_id, attempt);
        Some(task)
    }

    fn requeue_task(&mut self, task_id: Uuid, attempt: u32) -> Option<A2ATask> {
        let lease = self.in_flight.remove(&task_id)?;
        self.attempts.insert(task_id, attempt);
        let task = lease.task;
        self.tasks.push_back(task.clone());
        Some(task)
    }

    fn task_queue(&self, limit: usize) -> Vec<A2ATaskQueueEntry> {
        let mut entries: Vec<A2ATaskQueueEntry> = self
            .tasks
            .iter()
            .cloned()
            .map(|task| {
                let attempt = self.attempts.get(&task.id).copied().unwrap_or(0);
                A2ATaskQueueEntry {
                    attempt,
                    ..A2ATaskQueueEntry::queued(task)
                }
            })
            .collect();
        let mut leased: Vec<A2ATaskQueueEntry> = self
            .in_flight
            .values()
            .cloned()
            .map(A2ATaskQueueEntry::in_flight)
            .collect();
        leased.sort_by(|a, b| {
            a.leased_at_ms
                .cmp(&b.leased_at_ms)
                .then_with(|| a.task.id.cmp(&b.task.id))
        });
        entries.extend(leased);
        entries.truncate(limit);
        entries
    }
}

/// JSONL-backed [`Mailbox`]. Restart-resilient sibling of
/// [`InMemoryMailbox`]: every state transition is appended to a log
/// file before the in-memory state is mutated, so a crash between the
/// write and the mutation can only leave the on-disk log slightly
/// ahead of what was observed by callers — never behind.
pub struct JsonlMailbox {
    path: PathBuf,
    state: Mutex<MailboxState>,
    file_lock: AsyncMutex<()>,
    task_notify: Notify,
    result_notify: Notify,
}

impl JsonlMailbox {
    pub async fn open(path: PathBuf) -> Result<Self, A2AError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;

        let mut state = MailboxState::empty();
        let f = fs::File::open(&path).await?;
        let mut reader = BufReader::new(f);
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break;
            }
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                continue;
            }
            let ev: MailboxEvent = serde_json::from_str(trimmed)?;
            state.apply(ev);
        }

        Ok(Self {
            path,
            state: Mutex::new(state),
            file_lock: AsyncMutex::new(()),
            task_notify: Notify::new(),
            result_notify: Notify::new(),
        })
    }

    async fn append(&self, ev: &MailboxEvent) -> Result<(), A2AError> {
        let line = serde_json::to_string(ev)?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        f.write_all(line.as_bytes()).await?;
        f.write_all(b"\n").await?;
        f.flush().await?;
        Ok(())
    }
}

#[async_trait]
impl Mailbox for JsonlMailbox {
    async fn send_task(&self, task: A2ATask) -> Result<(), A2AError> {
        validate_task(&task)?;
        let _g = self.file_lock.lock().await;
        let replay = {
            let s = self.state.lock();
            idempotency_cache_key(&task)
                .and_then(|key| s.result_cache.get(&key).cloned())
                .map(|cached| cached.to_result(task.id))
        };
        if let Some(result) = replay {
            let event = MailboxEvent::IdempotencyResultReplayed {
                task: task.clone(),
                result,
            };
            self.append(&event).await?;
            self.state.lock().apply(event);
            self.result_notify.notify_one();
            return Ok(());
        }
        self.append(&MailboxEvent::TaskSent { task: task.clone() })
            .await?;
        {
            let mut s = self.state.lock();
            s.senders.insert(task.id, task.sender.clone());
            s.attempts.entry(task.id).or_insert(0);
            s.tasks.push_back(task);
        }
        self.task_notify.notify_one();
        Ok(())
    }

    async fn recv_task(&self) -> Result<A2ATask, A2AError> {
        loop {
            {
                let _g = self.file_lock.lock().await;
                let front = self
                    .state
                    .lock()
                    .tasks
                    .front()
                    .map(|t| (t.id, t.recipient.clone()));
                if let Some((id, recipient)) = front {
                    let lease_id = Uuid::new_v4();
                    let leased_at_ms = epoch_ms();
                    let attempt = self.state.lock().next_attempt(id);
                    self.append(&MailboxEvent::TaskLeased {
                        task_id: id,
                        lease_id,
                        leased_to: recipient,
                        leased_at_ms,
                        attempt,
                    })
                    .await?;
                    if let Some(t) = self.state.lock().lease_task(
                        id,
                        lease_id,
                        None,
                        leased_at_ms,
                        attempt,
                    ) {
                        return Ok(t);
                    }
                }
            }
            self.task_notify.notified().await;
        }
    }

    async fn try_recv_task_for(&self, recipient: &AgentId) -> Result<Option<A2ATask>, A2AError> {
        let _g = self.file_lock.lock().await;
        let target_id = {
            let s = self.state.lock();
            s.tasks
                .iter()
                .find(|t| t.recipient == *recipient)
                .map(|t| t.id)
        };
        let Some(id) = target_id else { return Ok(None) };
        let lease_id = Uuid::new_v4();
        let leased_at_ms = epoch_ms();
        let attempt = self.state.lock().next_attempt(id);
        self.append(&MailboxEvent::TaskLeased {
            task_id: id,
            lease_id,
            leased_to: recipient.clone(),
            leased_at_ms,
            attempt,
        })
        .await?;
        let mut s = self.state.lock();
        Ok(s.lease_task(id, lease_id, Some(recipient.clone()), leased_at_ms, attempt))
    }

    async fn send_result(&self, result: A2ATaskResult) -> Result<(), A2AError> {
        let _g = self.file_lock.lock().await;
        let cache_event = {
            let s = self.state.lock();
            s.in_flight
                .get(&result.task_id)
                .and_then(|lease| idempotency_cache_key(&lease.task))
                .map(|cache_key| MailboxEvent::IdempotencyResultCached {
                    cache_key,
                    result: A2AIdempotencyCachedResult::from_result(&result),
                })
        };
        self.append(&MailboxEvent::ResultPosted {
            result: result.clone(),
        })
        .await?;
        if let Some(event) = &cache_event {
            self.append(event).await?;
        }
        let mut s = self.state.lock();
        s.apply(MailboxEvent::ResultPosted { result });
        if let Some(event) = cache_event {
            s.apply(event);
        }
        self.result_notify.notify_one();
        Ok(())
    }

    async fn recv_result(&self) -> Result<A2ATaskResult, A2AError> {
        loop {
            {
                let _g = self.file_lock.lock().await;
                let front_id = self
                    .state
                    .lock()
                    .results
                    .front()
                    .map(|r| r.task_id);
                if let Some(id) = front_id {
                    self.append(&MailboxEvent::ResultRecv { task_id: id })
                        .await?;
                    if let Some(r) = self.state.lock().results.pop_front() {
                        return Ok(r);
                    }
                }
            }
            self.result_notify.notified().await;
        }
    }

    async fn try_recv_result_for(&self, peer: &AgentId) -> Result<Option<A2ATaskResult>, A2AError> {
        let _g = self.file_lock.lock().await;
        let target_task_id = {
            let s = self.state.lock();
            s.results.iter().find_map(|r| {
                s.senders
                    .get(&r.task_id)
                    .filter(|sender| *sender == peer)
                    .map(|_| r.task_id)
            })
        };
        let Some(task_id) = target_task_id else {
            return Ok(None);
        };
        self.append(&MailboxEvent::ResultRecv { task_id }).await?;
        let mut s = self.state.lock();
        let pos = s.results.iter().position(|r| r.task_id == task_id);
        Ok(pos.and_then(|p| s.results.remove(p)))
    }

    async fn recent_tasks(&self, limit: usize) -> Result<Vec<A2ATask>, A2AError> {
        Ok(self
            .state
            .lock()
            .tasks
            .iter()
            .take(limit)
            .cloned()
            .collect())
    }

    async fn task_queue(&self, limit: usize) -> Result<Vec<A2ATaskQueueEntry>, A2AError> {
        Ok(self.state.lock().task_queue(limit))
    }

    async fn repair_task(&self, request: A2ARepairRequest) -> Result<A2ARepairOutcome, A2AError> {
        validate_repair_request(&request)?;
        let _g = self.file_lock.lock().await;

        match request.command {
            A2ARepairCommand::Requeue {
                lease_id,
                duplicate_risk,
            } => {
                let lease = {
                    let s = self.state.lock();
                    s.in_flight
                        .get(&request.task_id)
                        .cloned()
                        .ok_or(A2AError::TaskNotInFlight(request.task_id))?
                };
                assert_lease_match(request.task_id, lease_id, Some(lease.lease_id))?;
                let event = MailboxEvent::TaskRequeued {
                    task_id: request.task_id,
                    lease_id: lease.lease_id,
                    reason: request.reason,
                    duplicate_risk,
                    requeued_at_ms: epoch_ms(),
                    attempt: lease.attempt,
                };
                self.append(&event).await?;
                self.state.lock().apply(event);
                self.task_notify.notify_one();
                Ok(A2ARepairOutcome {
                    task_id: request.task_id,
                    action: A2ARepairAction::Requeued,
                    state: A2ARepairState::Queued,
                    attempt: lease.attempt,
                    result: None,
                })
            }
            A2ARepairCommand::ForceError { lease_id, message } => {
                let lease = {
                    let s = self.state.lock();
                    s.in_flight
                        .get(&request.task_id)
                        .cloned()
                        .ok_or(A2AError::TaskNotInFlight(request.task_id))?
                };
                assert_lease_match(request.task_id, lease_id, Some(lease.lease_id))?;
                let result = A2ATaskResult::error(request.task_id, message);
                let event = MailboxEvent::TaskForceErrored {
                    task_id: request.task_id,
                    lease_id: lease.lease_id,
                    result: result.clone(),
                    reason: request.reason,
                    forced_at_ms: epoch_ms(),
                    attempt: lease.attempt,
                };
                self.append(&event).await?;
                self.state.lock().apply(event);
                self.result_notify.notify_one();
                Ok(A2ARepairOutcome {
                    task_id: request.task_id,
                    action: A2ARepairAction::ForcedError,
                    state: A2ARepairState::ResultPending,
                    attempt: lease.attempt,
                    result: Some(result),
                })
            }
        }
    }

    async fn recent_results(&self, limit: usize) -> Result<Vec<A2ATaskResult>, A2AError> {
        Ok(self
            .state
            .lock()
            .results
            .iter()
            .take(limit)
            .cloned()
            .collect())
    }

    async fn lookup_task_sender(&self, task_id: Uuid) -> Result<Option<AgentId>, A2AError> {
        Ok(self.state.lock().senders.get(&task_id).cloned())
    }

    async fn compact(&self) -> Result<u64, A2AError> {
        // Hold the file_lock across read-filter-rewrite so concurrent
        // send/recv can't race with the rewrite. Atomicity comes from
        // tempfile + rename.
        let _g = self.file_lock.lock().await;

        let raw = match fs::read_to_string(&self.path).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        let events: Vec<MailboxEvent> = raw
            .lines()
            .filter(|l| !l.is_empty())
            .map(serde_json::from_str)
            .collect::<Result<Vec<_>, _>>()?;

        let droppable = compute_droppable_task_ids(&events);
        if droppable.is_empty() {
            return Ok(0);
        }
        let kept: Vec<&MailboxEvent> = events
            .iter()
            .filter(|ev| !event_belongs_to_droppable(ev, &droppable))
            .collect();
        let dropped = (events.len() - kept.len()) as u64;
        if dropped == 0 {
            return Ok(0);
        }

        let tmp_path = self.path.with_extension("jsonl.tmp");
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)
            .await?;
        for ev in &kept {
            let line = serde_json::to_string(ev)?;
            f.write_all(line.as_bytes()).await?;
            f.write_all(b"\n").await?;
        }
        f.flush().await?;
        drop(f);
        fs::rename(&tmp_path, &self.path).await?;

        // Mirror the on-disk drop into in-memory state so the senders
        // map stays consistent without a reopen. Replay-on-open is the
        // ground truth; this update is purely a perf shortcut.
        let mut s = self.state.lock();
        for tid in &droppable {
            s.senders.remove(tid);
            s.in_flight.remove(tid);
            s.attempts.remove(tid);
        }
        Ok(dropped)
    }
}

fn compute_droppable_task_ids(events: &[MailboxEvent]) -> HashSet<Uuid> {
    // Per task_id: count delivery, ResultPosted, ResultRecv. TaskSent is
    // implicit from membership in `seen`.
    let mut seen: HashSet<Uuid> = HashSet::new();
    let mut delivered: HashSet<Uuid> = HashSet::new();
    let mut posted: HashMap<Uuid, u64> = HashMap::new();
    let mut drained: HashMap<Uuid, u64> = HashMap::new();
    for ev in events {
        match ev {
            MailboxEvent::TaskSent { task } => {
                seen.insert(task.id);
            }
            MailboxEvent::TaskRecv { task_id } => {
                delivered.insert(*task_id);
            }
            MailboxEvent::TaskLeased { task_id, .. } => {
                delivered.insert(*task_id);
            }
            MailboxEvent::TaskRequeued { .. } => {}
            MailboxEvent::TaskForceErrored { result, .. } => {
                *posted.entry(result.task_id).or_insert(0) += 1;
            }
            MailboxEvent::IdempotencyResultCached { .. } => {}
            MailboxEvent::IdempotencyResultReplayed { task, result } => {
                seen.insert(task.id);
                delivered.insert(task.id);
                *posted.entry(result.task_id).or_insert(0) += 1;
            }
            MailboxEvent::ResultPosted { result } => {
                *posted.entry(result.task_id).or_insert(0) += 1;
            }
            MailboxEvent::ResultRecv { task_id } => {
                *drained.entry(*task_id).or_insert(0) += 1;
            }
        }
    }
    seen.into_iter()
        .filter(|tid| delivered.contains(tid))
        .filter(|tid| {
            let p = posted.get(tid).copied().unwrap_or(0);
            let d = drained.get(tid).copied().unwrap_or(0);
            p > 0 && p == d
        })
        .collect()
}

fn event_belongs_to_droppable(ev: &MailboxEvent, droppable: &HashSet<Uuid>) -> bool {
    match ev {
        MailboxEvent::TaskSent { task } => droppable.contains(&task.id),
        MailboxEvent::TaskRecv { task_id } => droppable.contains(task_id),
        MailboxEvent::TaskLeased { task_id, .. } => droppable.contains(task_id),
        MailboxEvent::TaskRequeued { task_id, .. } => droppable.contains(task_id),
        MailboxEvent::TaskForceErrored { task_id, .. } => droppable.contains(task_id),
        MailboxEvent::IdempotencyResultCached { .. } => false,
        MailboxEvent::IdempotencyResultReplayed { task, .. } => droppable.contains(&task.id),
        MailboxEvent::ResultPosted { result } => droppable.contains(&result.task_id),
        MailboxEvent::ResultRecv { task_id } => droppable.contains(task_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_agent(name: &str) -> AgentId {
        AgentId::new(name, [0u8; 32])
    }

    fn dummy_task() -> A2ATask {
        A2ATask {
            id: Uuid::new_v4(),
            sender: dummy_agent("orchestrator@local"),
            recipient: dummy_agent("research@local"),
            intent_text: "find recent papers on agent memory".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        }
    }

    #[test]
    fn task_round_trips_through_json() {
        let t = dummy_task();
        let s = serde_json::to_string(&t).unwrap();
        assert!(s.contains("\"intent_text\":"));
        let back: A2ATask = serde_json::from_str(&s).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn task_round_trips_idempotency_metadata() {
        let mut t = dummy_task();
        t.idempotency = Some(A2AIdempotency::new(
            A2ADuplicateSafety::Idempotent,
            "research:agent-memory:2026-05-09",
        ));

        let s = serde_json::to_string(&t).unwrap();
        assert!(s.contains("\"duplicate_safety\":\"idempotent\""));
        assert!(s.contains("\"key\":\"research:agent-memory:2026-05-09\""));

        let back: A2ATask = serde_json::from_str(&s).unwrap();
        assert_eq!(back.idempotency, t.idempotency);
    }

    #[test]
    fn task_round_trips_task_kind_metadata() {
        let mut t = dummy_task();
        t.task_kind = Some("research.lookup".into());

        let s = serde_json::to_string(&t).unwrap();
        assert!(s.contains("\"task_kind\":\"research.lookup\""));

        let back: A2ATask = serde_json::from_str(&s).unwrap();
        assert_eq!(back.task_kind.as_deref(), Some("research.lookup"));
    }

    #[test]
    fn task_deserializes_legacy_without_idempotency_metadata() {
        let value = serde_json::json!({
            "id": Uuid::nil(),
            "sender": dummy_agent("orchestrator@local"),
            "recipient": dummy_agent("research@local"),
            "intent_text": "legacy task"
        });

        let task: A2ATask = serde_json::from_value(value).unwrap();
        assert_eq!(task.id, Uuid::nil());
        assert_eq!(task.idempotency, None);
        assert_eq!(task.task_kind, None);
    }

    #[test]
    fn auto_retry_policy_defaults_disabled() {
        let policy = A2AAutoRetryPolicy::default();
        assert!(!policy.enabled);
        assert_eq!(policy.min_lease_age_ms, 300_000);
        assert_eq!(policy.max_attempts, 3);
        assert_eq!(policy.max_requeues, 1);
        assert_eq!(policy.scan_limit, 100);
    }

    #[test]
    fn auto_retry_evaluates_only_old_idempotent_in_flight_tasks() {
        let mut task = dummy_task();
        task.idempotency = Some(A2AIdempotency::new(
            A2ADuplicateSafety::Idempotent,
            "task:key",
        ));
        let entry = A2ATaskQueueEntry {
            state: A2ATaskQueueState::InFlight,
            task,
            lease_id: Some(Uuid::nil()),
            leased_to: Some(dummy_agent("research@local")),
            leased_at_ms: Some(1_000),
            attempt: 1,
        };
        let policy = A2AAutoRetryPolicy {
            enabled: true,
            min_lease_age_ms: 300_000,
            max_attempts: 3,
            max_requeues: 1,
            scan_limit: 100,
        };

        let decision = evaluate_auto_retry(&entry, &policy, 301_000);
        match decision {
            A2AAutoRetryDecision::Requeue {
                lease_id,
                lease_age_ms,
                idempotency_key,
            } => {
                assert_eq!(lease_id, Uuid::nil());
                assert_eq!(lease_age_ms, 300_000);
                assert_eq!(idempotency_key, "task:key");
            }
            other => panic!("unexpected decision: {other:?}"),
        }
    }

    #[test]
    fn auto_retry_rejects_unsafe_or_exhausted_tasks() {
        let mut task = dummy_task();
        task.idempotency = Some(A2AIdempotency::new(A2ADuplicateSafety::Unsafe, "task:key"));
        let mut entry = A2ATaskQueueEntry {
            state: A2ATaskQueueState::InFlight,
            task,
            lease_id: Some(Uuid::nil()),
            leased_to: Some(dummy_agent("research@local")),
            leased_at_ms: Some(1_000),
            attempt: 1,
        };
        let policy = A2AAutoRetryPolicy {
            enabled: true,
            min_lease_age_ms: 0,
            max_attempts: 3,
            max_requeues: 1,
            scan_limit: 100,
        };

        assert!(matches!(
            evaluate_auto_retry(&entry, &policy, 1_000),
            A2AAutoRetryDecision::Skip {
                reason: A2AAutoRetrySkipReason::UnsafeDuplicateSafety,
                ..
            }
        ));

        entry.task.idempotency = Some(A2AIdempotency::new(
            A2ADuplicateSafety::Idempotent,
            "task:key",
        ));
        entry.attempt = 3;
        assert!(matches!(
            evaluate_auto_retry(&entry, &policy, 1_000),
            A2AAutoRetryDecision::Skip {
                reason: A2AAutoRetrySkipReason::MaxAttemptsReached,
                ..
            }
        ));
    }

    #[test]
    fn a2a_task_status_serde_pins_snake_case_wire_form() {
        // A2ATaskStatus rides inside A2ATaskResult and the receiver-side
        // A2AIdempotencyCachedResult JSON. Both are persisted across
        // daemon restarts, so a slug rename without a migration would
        // silently fail to deserialize every previously cached result.
        let cases: [(A2ATaskStatus, &str); 3] = [
            (A2ATaskStatus::Ok, "ok"),
            (A2ATaskStatus::Error, "error"),
            (A2ATaskStatus::Partial, "partial"),
        ];
        for (variant, slug) in cases {
            let wire = serde_json::to_string(&variant).unwrap();
            assert_eq!(wire, format!("\"{slug}\""));
            let back: A2ATaskStatus = serde_json::from_str(&wire).unwrap();
            assert_eq!(back, variant);
        }

        // The snake_case whitelist must reject other casings so a future
        // permissive arm cannot silently absorb mis-cased upstream JSON.
        assert!(serde_json::from_str::<A2ATaskStatus>("\"Ok\"").is_err());
        assert!(serde_json::from_str::<A2ATaskStatus>("\"error-partial\"").is_err());
    }

    #[test]
    fn a2a_auto_retry_skip_reason_as_str_pins_each_variant_slug() {
        // The slugs flow into covenantd::record_a2a_auto_retry_scheduler_scan
        // as keys in a BTreeMap<String, u64> of skipped-by-reason counters,
        // and from there into audit rows and downstream dashboards. A
        // renamed or swapped slug splits the same bucket across two names
        // silently. Pin every variant explicitly so adding a new variant
        // forces the author to extend this array AND the as_str() arm.
        let cases: [(A2AAutoRetrySkipReason, &str); 9] = [
            (A2AAutoRetrySkipReason::Disabled, "disabled"),
            (A2AAutoRetrySkipReason::NotInFlight, "not_in_flight"),
            (A2AAutoRetrySkipReason::MissingLease, "missing_lease"),
            (A2AAutoRetrySkipReason::LeaseTooYoung, "lease_too_young"),
            (
                A2AAutoRetrySkipReason::MissingIdempotency,
                "missing_idempotency",
            ),
            (
                A2AAutoRetrySkipReason::UnsafeDuplicateSafety,
                "unsafe_duplicate_safety",
            ),
            (
                A2AAutoRetrySkipReason::MaxAttemptsReached,
                "max_attempts_reached",
            ),
            (A2AAutoRetrySkipReason::LimitReached, "limit_reached"),
            (
                A2AAutoRetrySkipReason::CapabilityScopeMismatch,
                "capability_scope_mismatch",
            ),
        ];
        for (reason, expected) in cases {
            assert_eq!(
                reason.as_str(),
                expected,
                "{reason:?} must keep its documented slug; if this fires after \
                 renaming a variant, update the BTreeMap consumers in \
                 covenantd::record_a2a_auto_retry_scheduler_scan and any \
                 downstream dashboards before changing the slug",
            );
        }
    }

    #[test]
    fn a2a_repair_action_serde_pins_snake_case_wire_form() {
        // A2ARepairAction is the discriminator on every A2ARepairOutcome
        // emitted by manual and scheduled repair flows; the slug lands in
        // daemon repair audit rows and downstream operator dashboards.
        // ForcedError is load-bearing — the rename_all default would emit
        // "ForcedError" titlecase and silently bisect repair telemetry.
        let cases: [(A2ARepairAction, &str); 2] = [
            (A2ARepairAction::Requeued, "requeued"),
            (A2ARepairAction::ForcedError, "forced_error"),
        ];
        for (variant, slug) in cases {
            let wire = serde_json::to_string(&variant).unwrap();
            assert_eq!(wire, format!("\"{slug}\""));
            let back: A2ARepairAction = serde_json::from_str(&wire).unwrap();
            assert_eq!(back, variant);
        }

        assert!(
            serde_json::from_str::<A2ARepairAction>("\"ForcedError\"").is_err(),
            "titlecase ForcedError (the rename_all default) must be rejected",
        );
        assert!(
            serde_json::from_str::<A2ARepairAction>("\"forcedError\"").is_err(),
            "camelCase forcedError must be rejected so the snake_case whitelist stays tight",
        );
    }

    #[test]
    fn task_skips_optional_fields_when_none() {
        let t = dummy_task();
        let s = serde_json::to_string(&t).unwrap();
        assert!(!s.contains("parent"));
        assert!(!s.contains("deadline_ms"));
        assert!(!s.contains("idempotency"));
    }

    #[test]
    fn idempotency_cache_key_pins_safety_kind_fallback_and_empty_key_paths() {
        let base = dummy_task();
        let sender_b58 = base.sender.pubkey_base58();
        let recipient_b58 = base.recipient.pubkey_base58();

        // No idempotency metadata → not cacheable.
        assert!(idempotency_cache_key(&base).is_none());

        // duplicate_safety=Unsafe → not cacheable even when a key is set.
        let mut unsafe_task = base.clone();
        unsafe_task.idempotency = Some(A2AIdempotency::new(A2ADuplicateSafety::Unsafe, "k"));
        assert!(idempotency_cache_key(&unsafe_task).is_none());

        // Empty key → not cacheable (a single empty key would otherwise
        // collapse every Idempotent task into one cache slot).
        let mut empty_key = base.clone();
        empty_key.idempotency = Some(A2AIdempotency::new(A2ADuplicateSafety::Idempotent, ""));
        assert!(idempotency_cache_key(&empty_key).is_none());

        // Whitespace-only key → not cacheable; trim must happen before
        // the emptiness check, not after.
        let mut blank_key = base.clone();
        blank_key.idempotency = Some(A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "   "));
        assert!(idempotency_cache_key(&blank_key).is_none());

        // task_kind=Some(non-empty) → cache key uses the explicit kind
        // and the trimmed key value; sender/recipient base58 are bound.
        let mut with_kind = base.clone();
        with_kind.task_kind = Some("research.lookup".into());
        with_kind.idempotency = Some(A2AIdempotency::new(
            A2ADuplicateSafety::Idempotent,
            "  research:agent-memory:2026-05-12  ",
        ));
        let key = idempotency_cache_key(&with_kind).expect("kind+key must cache");
        assert_eq!(key.task_kind, "research.lookup");
        assert_eq!(key.key, "research:agent-memory:2026-05-12");
        assert_eq!(key.sender_pubkey_b58, sender_b58);
        assert_eq!(key.recipient_pubkey_b58, recipient_b58);

        // task_kind=None → fall back to intent_text so legacy senders
        // that did not learn the explicit kind metadata still share a
        // cache bucket per intent.
        let mut no_kind = base.clone();
        no_kind.idempotency = Some(A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "k"));
        let key = idempotency_cache_key(&no_kind).expect("intent fallback must cache");
        assert_eq!(key.task_kind, base.intent_text);

        // task_kind=Some(whitespace) → also fall back to intent_text;
        // an all-whitespace kind is treated as missing so it cannot
        // silently shard the cache off from the intent_text bucket.
        let mut blank_kind = base.clone();
        blank_kind.task_kind = Some("   ".into());
        blank_kind.idempotency = Some(A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "k"));
        let key = idempotency_cache_key(&blank_kind).expect("whitespace kind must fall back");
        assert_eq!(key.task_kind, base.intent_text);
    }

    #[test]
    fn validate_task_pins_task_kind_and_idempotency_key_emptiness_arms() {
        // Baseline: kind=None, idempotency=None is the legacy shape and
        // must validate so older senders keep working.
        assert!(validate_task(&dummy_task()).is_ok());

        // task_kind=Some(empty) → InvalidTask. An all-whitespace kind
        // must be rejected identically so the check cannot be silently
        // bypassed by typing spaces into the field.
        for empty in ["", "   "] {
            let mut t = dummy_task();
            t.task_kind = Some(empty.into());
            let err = validate_task(&t).unwrap_err();
            match err {
                A2AError::InvalidTask(message) => assert!(
                    message.contains("task_kind must not be empty when present"),
                    "unexpected InvalidTask payload: {message:?}",
                ),
                other => panic!("expected InvalidTask, got {other:?}"),
            }
        }

        // task_kind=Some(non-empty) validates Ok. A regression that
        // inverted the trim().is_empty() check would block every
        // explicit kind metadata send, so pin the accept path too.
        let mut kind_ok = dummy_task();
        kind_ok.task_kind = Some("research.lookup".into());
        assert!(validate_task(&kind_ok).is_ok());

        // idempotency present with empty/whitespace key → InvalidTask.
        // The check must run regardless of duplicate_safety so an
        // Unsafe-tagged send with an empty key still fails fast instead
        // of silently producing zero-byte cache keys in receivers that
        // later upgrade the tag.
        for empty in ["", "   "] {
            for safety in [A2ADuplicateSafety::Idempotent, A2ADuplicateSafety::Unsafe] {
                let mut t = dummy_task();
                t.idempotency = Some(A2AIdempotency::new(safety, empty));
                let err = validate_task(&t).unwrap_err();
                match err {
                    A2AError::InvalidTask(message) => assert!(
                        message.contains("idempotency key must not be empty"),
                        "unexpected InvalidTask payload: {message:?}",
                    ),
                    other => panic!("expected InvalidTask, got {other:?}"),
                }
            }
        }

        // idempotency present with non-empty key → Ok regardless of
        // duplicate_safety; idempotency_cache_key handles the
        // Unsafe-vs-Idempotent caching decision separately.
        for safety in [A2ADuplicateSafety::Idempotent, A2ADuplicateSafety::Unsafe] {
            let mut t = dummy_task();
            t.idempotency = Some(A2AIdempotency::new(safety, "k"));
            assert!(validate_task(&t).is_ok());
        }
    }

    #[test]
    fn validate_repair_request_pins_reason_and_force_error_message_arms() {
        let task_id = Uuid::new_v4();
        let requeue = || A2ARepairCommand::Requeue {
            lease_id: None,
            duplicate_risk: A2ADuplicateRisk::Idempotent,
        };
        let force_error = |message: &str| A2ARepairCommand::ForceError {
            lease_id: None,
            message: message.into(),
        };

        // Requeue with non-empty reason validates Ok. Pin the accept
        // path so an inverted reason check is loud.
        assert!(validate_repair_request(&A2ARepairRequest {
            task_id,
            command: requeue(),
            reason: "operator-initiated".into(),
        })
        .is_ok());

        // Empty/whitespace reason rejects with the documented message
        // for both Requeue and ForceError so the reason field cannot
        // silently widen across the two variants.
        for reason in ["", "   "] {
            for command in [requeue(), force_error("network reset")] {
                let err = validate_repair_request(&A2ARepairRequest {
                    task_id,
                    command,
                    reason: reason.into(),
                })
                .unwrap_err();
                match err {
                    A2AError::InvalidRepair(message) => assert!(
                        message.contains("reason must not be empty"),
                        "unexpected InvalidRepair payload: {message:?}",
                    ),
                    other => panic!("expected InvalidRepair, got {other:?}"),
                }
            }
        }

        // ForceError with empty/whitespace message rejects even when
        // reason is otherwise valid. The message check only fires for
        // ForceError; Requeue does not carry a message field.
        for message in ["", "   "] {
            let err = validate_repair_request(&A2ARepairRequest {
                task_id,
                command: force_error(message),
                reason: "operator-initiated".into(),
            })
            .unwrap_err();
            match err {
                A2AError::InvalidRepair(payload) => assert!(
                    payload.contains("force_error message must not be empty"),
                    "unexpected InvalidRepair payload: {payload:?}",
                ),
                other => panic!("expected InvalidRepair, got {other:?}"),
            }
        }

        // ForceError with non-empty message validates Ok when reason
        // is also non-empty. Confirms the accept path on the variant
        // that carries the extra field.
        assert!(validate_repair_request(&A2ARepairRequest {
            task_id,
            command: force_error("upstream returned 502"),
            reason: "operator-initiated".into(),
        })
        .is_ok());
    }

    #[test]
    fn event_belongs_to_droppable_pins_each_variant_lookup_and_idempotency_cache_invariant() {
        let mut task = dummy_task();
        let task_id = task.id;
        let other_id = Uuid::new_v4();
        let lease_id = Uuid::new_v4();
        let droppable: HashSet<Uuid> = std::iter::once(task_id).collect();
        let disjoint: HashSet<Uuid> = std::iter::once(other_id).collect();

        let make_result = |id: Uuid| A2ATaskResult::ok(id, vec![]);
        let make_cache_key = || A2AIdempotencyCacheKey {
            sender_pubkey_b58: task.sender.pubkey_base58(),
            recipient_pubkey_b58: task.recipient.pubkey_base58(),
            task_kind: task.intent_text.clone(),
            key: "k".into(),
        };
        let make_cached_result = || A2AIdempotencyCachedResult {
            source_task_id: task_id,
            status: A2ATaskStatus::Ok,
            content: vec![],
            error_message: None,
        };

        let task_id_variants: Vec<MailboxEvent> = vec![
            MailboxEvent::TaskSent { task: task.clone() },
            MailboxEvent::TaskRecv { task_id },
            MailboxEvent::TaskLeased {
                task_id,
                lease_id,
                leased_to: task.recipient.clone(),
                leased_at_ms: 0,
                attempt: 0,
            },
            MailboxEvent::TaskRequeued {
                task_id,
                lease_id,
                reason: "operator".into(),
                duplicate_risk: A2ADuplicateRisk::Idempotent,
                requeued_at_ms: 0,
                attempt: 0,
            },
            MailboxEvent::TaskForceErrored {
                task_id,
                lease_id,
                result: make_result(task_id),
                reason: "upstream".into(),
                forced_at_ms: 0,
                attempt: 0,
            },
            MailboxEvent::IdempotencyResultReplayed {
                task: task.clone(),
                result: make_result(task_id),
            },
            MailboxEvent::ResultPosted {
                result: make_result(task_id),
            },
            MailboxEvent::ResultRecv { task_id },
        ];

        for ev in &task_id_variants {
            assert!(
                event_belongs_to_droppable(ev, &droppable),
                "variant must be droppable when its task_id is in the set: {ev:?}",
            );
            assert!(
                !event_belongs_to_droppable(ev, &disjoint),
                "variant must not be droppable when its task_id is absent: {ev:?}",
            );
        }

        // IdempotencyResultCached must never be droppable even when the
        // underlying replayed task_id is in the set; the cache row is
        // operator-visible replay provenance and a compaction pass that
        // dropped it would silently break the audit trail.
        let cached = MailboxEvent::IdempotencyResultCached {
            cache_key: make_cache_key(),
            result: make_cached_result(),
        };
        assert!(!event_belongs_to_droppable(&cached, &droppable));
        assert!(!event_belongs_to_droppable(&cached, &disjoint));

        // Re-use task to anchor the suppression check on a fresh id so a
        // future refactor that introduced any droppable.contains lookup
        // for the cached variant would fail this assertion.
        task.id = other_id;
        let cached_other = MailboxEvent::IdempotencyResultCached {
            cache_key: make_cache_key(),
            result: A2AIdempotencyCachedResult {
                source_task_id: other_id,
                status: A2ATaskStatus::Ok,
                content: vec![],
                error_message: None,
            },
        };
        assert!(!event_belongs_to_droppable(&cached_other, &disjoint));
    }

    #[test]
    fn assert_lease_match_pins_expected_actual_and_none_paths() {
        let task_id = Uuid::new_v4();
        let lease_a = Uuid::new_v4();
        let lease_b = Uuid::new_v4();

        // expected=None means the caller did not pin a specific lease,
        // so any actual must be accepted including None and Some(x).
        assert!(assert_lease_match(task_id, None, None).is_ok());
        assert!(assert_lease_match(task_id, None, Some(lease_a)).is_ok());

        // Same-lease match returns Ok so operator-initiated repair under
        // a pinned lease still flows through.
        assert!(assert_lease_match(task_id, Some(lease_a), Some(lease_a)).is_ok());

        // Mismatched lease returns LeaseMismatch and binds task_id,
        // expected, and actual so the caller can surface both lease ids
        // in the audit trail.
        let err = assert_lease_match(task_id, Some(lease_a), Some(lease_b)).unwrap_err();
        match err {
            A2AError::LeaseMismatch {
                task_id: tid,
                expected,
                actual,
            } => {
                assert_eq!(tid, task_id);
                assert_eq!(expected, Some(lease_a));
                assert_eq!(actual, Some(lease_b));
            }
            other => panic!("expected LeaseMismatch, got {other:?}"),
        }

        // expected=Some, actual=None still mismatches so a never-leased
        // task cannot be silently repaired under an explicit lease_id.
        let err = assert_lease_match(task_id, Some(lease_a), None).unwrap_err();
        match err {
            A2AError::LeaseMismatch {
                task_id: tid,
                expected,
                actual,
            } => {
                assert_eq!(tid, task_id);
                assert_eq!(expected, Some(lease_a));
                assert_eq!(actual, None);
            }
            other => panic!("expected LeaseMismatch, got {other:?}"),
        }
    }

    #[test]
    fn result_status_serialises_snake_case() {
        let r = A2ATaskResult::ok(Uuid::new_v4(), vec![]);
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"status\":\"ok\""));
    }

    #[test]
    fn result_error_carries_message() {
        let r = A2ATaskResult::error(Uuid::new_v4(), "no agent matched");
        assert_eq!(r.status, A2ATaskStatus::Error);
        assert_eq!(r.error_message.as_deref(), Some("no agent matched"));
    }

    #[tokio::test]
    async fn in_memory_mailbox_round_trips_a_task() {
        let m = InMemoryMailbox::new();
        let t = dummy_task();
        m.send_task(t.clone()).await.unwrap();
        let got = m.recv_task().await.unwrap();
        assert_eq!(got, t);
    }

    #[tokio::test]
    async fn rejects_empty_idempotency_key() {
        let m = InMemoryMailbox::new();
        let mut t = dummy_task();
        t.idempotency = Some(A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "   "));

        let err = m.send_task(t).await.unwrap_err();
        assert!(matches!(err, A2AError::InvalidTask(_)));
    }

    #[tokio::test]
    async fn rejects_empty_task_kind() {
        let m = InMemoryMailbox::new();
        let mut t = dummy_task();
        t.task_kind = Some("   ".into());

        let err = m.send_task(t).await.unwrap_err();
        assert!(matches!(err, A2AError::InvalidTask(_)));
    }

    #[tokio::test]
    async fn in_memory_replays_cached_idempotent_result_without_delivery() {
        let m = InMemoryMailbox::new();
        let mut first = dummy_task();
        first.idempotency = Some(A2AIdempotency::new(
            A2ADuplicateSafety::Idempotent,
            "research:agent-memory",
        ));
        m.send_task(first.clone()).await.unwrap();
        let leased = m
            .try_recv_task_for(&first.recipient)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(leased.id, first.id);
        m.send_result(A2ATaskResult::ok(
            first.id,
            vec![Content::text("cached answer")],
        ))
        .await
        .unwrap();

        let mut duplicate = first.clone();
        duplicate.id = Uuid::new_v4();
        m.send_task(duplicate.clone()).await.unwrap();

        assert!(m
            .try_recv_task_for(&duplicate.recipient)
            .await
            .unwrap()
            .is_none());
        let original = m.try_recv_result_for(&first.sender).await.unwrap().unwrap();
        assert_eq!(original.task_id, first.id);
        let replayed = m.try_recv_result_for(&first.sender).await.unwrap().unwrap();
        assert_eq!(replayed.task_id, duplicate.id);
        assert_eq!(replayed.status, A2ATaskStatus::Ok);
        assert_eq!(replayed.content, vec![Content::text("cached answer")]);
    }

    #[tokio::test]
    async fn task_kind_drives_idempotency_cache_when_present() {
        let m = InMemoryMailbox::new();
        let mut first = dummy_task();
        first.intent_text = "draft release notes for alpha one".into();
        first.task_kind = Some("release.notes".into());
        first.idempotency = Some(A2AIdempotency::new(
            A2ADuplicateSafety::Idempotent,
            "release:v0.1.0-alpha.1",
        ));
        m.send_task(first.clone()).await.unwrap();
        let _ = m
            .try_recv_task_for(&first.recipient)
            .await
            .unwrap()
            .unwrap();
        m.send_result(A2ATaskResult::ok(
            first.id,
            vec![Content::text("notes ready")],
        ))
        .await
        .unwrap();

        let mut duplicate = first.clone();
        duplicate.id = Uuid::new_v4();
        duplicate.intent_text = "write final alpha release notes".into();
        m.send_task(duplicate.clone()).await.unwrap();

        assert!(m
            .try_recv_task_for(&duplicate.recipient)
            .await
            .unwrap()
            .is_none());
        let _ = m.try_recv_result_for(&first.sender).await.unwrap().unwrap();
        let replayed = m.try_recv_result_for(&first.sender).await.unwrap().unwrap();
        assert_eq!(replayed.task_id, duplicate.id);
        assert_eq!(replayed.content, vec![Content::text("notes ready")]);
    }

    #[tokio::test]
    async fn unsafe_tasks_do_not_populate_idempotency_cache() {
        let m = InMemoryMailbox::new();
        let mut first = dummy_task();
        first.idempotency = Some(A2AIdempotency::new(
            A2ADuplicateSafety::Unsafe,
            "research:agent-memory",
        ));
        m.send_task(first.clone()).await.unwrap();
        let _ = m
            .try_recv_task_for(&first.recipient)
            .await
            .unwrap()
            .unwrap();
        m.send_result(A2ATaskResult::ok(first.id, vec![]))
            .await
            .unwrap();

        let mut duplicate = first.clone();
        duplicate.id = Uuid::new_v4();
        m.send_task(duplicate.clone()).await.unwrap();

        let delivered = m
            .try_recv_task_for(&duplicate.recipient)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivered.id, duplicate.id);
    }

    #[tokio::test]
    async fn in_memory_mailbox_recv_blocks_until_send() {
        let m = std::sync::Arc::new(InMemoryMailbox::new());
        let m_recv = m.clone();
        let h = tokio::spawn(async move { m_recv.recv_task().await.unwrap() });
        // Tiny yield to give the recv task a chance to park.
        tokio::task::yield_now().await;
        let t = dummy_task();
        m.send_task(t.clone()).await.unwrap();
        let got = h.await.unwrap();
        assert_eq!(got, t);
    }

    #[tokio::test]
    async fn try_recv_returns_none_when_empty_and_some_after_send() {
        let m = InMemoryMailbox::new();
        let recipient = dummy_agent("research@local");
        assert!(m.try_recv_task_for(&recipient).await.unwrap().is_none());
        m.send_task(dummy_task()).await.unwrap();
        let got = m.try_recv_task_for(&recipient).await.unwrap();
        assert!(got.is_some());
        // After draining, empty again.
        assert!(m.try_recv_task_for(&recipient).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn lookup_task_sender_returns_sender_after_send_and_after_recv() {
        let m = InMemoryMailbox::new();
        let t = dummy_task();
        let sender = t.sender.clone();
        m.send_task(t.clone()).await.unwrap();

        // Available immediately after send.
        assert_eq!(
            m.lookup_task_sender(t.id).await.unwrap(),
            Some(sender.clone())
        );

        // Still available after the task has been recv'd — the result
        // can come back later and still attribute correctly.
        let _ = m.recv_task().await.unwrap();
        assert_eq!(m.lookup_task_sender(t.id).await.unwrap(), Some(sender));
    }

    #[tokio::test]
    async fn lookup_task_sender_returns_none_for_unknown_task_id() {
        let m = InMemoryMailbox::new();
        assert!(m
            .lookup_task_sender(Uuid::new_v4())
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn recent_returns_oldest_first_without_consuming() {
        let m = InMemoryMailbox::new();
        let t1 = dummy_task();
        let t2 = dummy_task();
        let t3 = dummy_task();
        m.send_task(t1.clone()).await.unwrap();
        m.send_task(t2.clone()).await.unwrap();
        m.send_task(t3.clone()).await.unwrap();

        let recent = m.recent_tasks(10).await.unwrap();
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].id, t1.id, "oldest first");
        assert_eq!(recent[2].id, t3.id, "newest last");

        // recent_tasks must not drain.
        let recipient = dummy_agent("research@local");
        assert!(m.try_recv_task_for(&recipient).await.unwrap().is_some());
        assert!(m.try_recv_task_for(&recipient).await.unwrap().is_some());
        assert!(m.try_recv_task_for(&recipient).await.unwrap().is_some());
        assert!(m.try_recv_task_for(&recipient).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn recent_respects_limit_and_handles_empty() {
        let m = InMemoryMailbox::new();
        assert!(m.recent_tasks(10).await.unwrap().is_empty());
        assert!(m.recent_results(10).await.unwrap().is_empty());

        for _ in 0..5 {
            m.send_task(dummy_task()).await.unwrap();
        }
        assert_eq!(m.recent_tasks(3).await.unwrap().len(), 3);
        assert_eq!(m.recent_tasks(0).await.unwrap().len(), 0);
        assert_eq!(m.recent_tasks(100).await.unwrap().len(), 5);
    }

    #[tokio::test]
    async fn in_memory_mailbox_keeps_tasks_and_results_distinct() {
        let m = InMemoryMailbox::new();
        let t = dummy_task();
        let r = A2ATaskResult::ok(t.id, vec![Content::text("done")]);
        m.send_task(t.clone()).await.unwrap();
        m.send_result(r.clone()).await.unwrap();

        let got_t = m.recv_task().await.unwrap();
        assert_eq!(got_t.id, t.id);
        let got_r = m.recv_result().await.unwrap();
        assert_eq!(got_r.task_id, r.task_id);
    }

    #[tokio::test]
    async fn jsonl_round_trips_a_task_through_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a2a").join("events.jsonl");
        let m = JsonlMailbox::open(path.clone()).await.unwrap();
        let t = dummy_task();
        m.send_task(t.clone()).await.unwrap();
        let got = m.recv_task().await.unwrap();
        assert_eq!(got, t);
    }

    #[tokio::test]
    async fn jsonl_preserves_task_idempotency_metadata_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a2a").join("events.jsonl");
        let m = JsonlMailbox::open(path.clone()).await.unwrap();
        let mut t = dummy_task();
        t.idempotency = Some(A2AIdempotency::new(
            A2ADuplicateSafety::Idempotent,
            "research:agent-memory:2026-05-09",
        ));
        m.send_task(t.clone()).await.unwrap();

        let reopened = JsonlMailbox::open(path).await.unwrap();
        let queue = reopened.task_queue(10).await.unwrap();
        assert_eq!(queue[0].task.idempotency, t.idempotency);
        let got = reopened
            .try_recv_task_for(&t.recipient)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.idempotency, t.idempotency);
    }

    #[tokio::test]
    async fn jsonl_replays_cached_idempotent_result_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut first = dummy_task();
        first.idempotency = Some(A2AIdempotency::new(
            A2ADuplicateSafety::Idempotent,
            "research:agent-memory",
        ));
        {
            let m = JsonlMailbox::open(path.clone()).await.unwrap();
            m.send_task(first.clone()).await.unwrap();
            let _ = m
                .try_recv_task_for(&first.recipient)
                .await
                .unwrap()
                .unwrap();
            m.send_result(A2ATaskResult::ok(
                first.id,
                vec![Content::text("cached answer")],
            ))
            .await
            .unwrap();
            let _ = m.try_recv_result_for(&first.sender).await.unwrap().unwrap();
            assert_eq!(m.compact().await.unwrap(), 4);
        }

        let reopened = JsonlMailbox::open(path).await.unwrap();
        let mut duplicate = first.clone();
        duplicate.id = Uuid::new_v4();
        reopened.send_task(duplicate.clone()).await.unwrap();

        assert!(reopened
            .try_recv_task_for(&duplicate.recipient)
            .await
            .unwrap()
            .is_none());
        let replayed = reopened
            .try_recv_result_for(&duplicate.sender)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(replayed.task_id, duplicate.id);
        assert_eq!(replayed.content, vec![Content::text("cached answer")]);
    }

    #[tokio::test]
    async fn jsonl_replays_pending_tasks_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let t1 = dummy_task();
        let t2 = dummy_task();
        {
            let m = JsonlMailbox::open(path.clone()).await.unwrap();
            m.send_task(t1.clone()).await.unwrap();
            m.send_task(t2.clone()).await.unwrap();
        }
        let m2 = JsonlMailbox::open(path.clone()).await.unwrap();
        let recent = m2.recent_tasks(10).await.unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].id, t1.id);
        assert_eq!(recent[1].id, t2.id);
    }

    #[tokio::test]
    async fn jsonl_drained_tasks_do_not_reappear_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let t1 = dummy_task();
        let t2 = dummy_task();
        {
            let m = JsonlMailbox::open(path.clone()).await.unwrap();
            m.send_task(t1.clone()).await.unwrap();
            m.send_task(t2.clone()).await.unwrap();
            let drained = m
                .try_recv_task_for(&dummy_agent("research@local"))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(drained.id, t1.id);
        }
        let m2 = JsonlMailbox::open(path.clone()).await.unwrap();
        let recent = m2.recent_tasks(10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, t2.id);
    }

    #[tokio::test]
    async fn jsonl_leased_tasks_are_in_flight_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let task = dummy_task();
        {
            let m = JsonlMailbox::open(path.clone()).await.unwrap();
            m.send_task(task.clone()).await.unwrap();
            let leased = m.try_recv_task_for(&task.recipient).await.unwrap().unwrap();
            assert_eq!(leased.id, task.id);
        }

        let reopened = JsonlMailbox::open(path).await.unwrap();
        assert!(reopened
            .try_recv_task_for(&task.recipient)
            .await
            .unwrap()
            .is_none());

        let queue = reopened.task_queue(10).await.unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].state, A2ATaskQueueState::InFlight);
        assert_eq!(queue[0].task.id, task.id);
        assert_eq!(queue[0].leased_to.as_ref(), Some(&task.recipient));
        assert_eq!(queue[0].attempt, 1);
        assert!(queue[0].lease_id.is_some());
        assert!(queue[0].leased_at_ms.is_some());
    }

    #[tokio::test]
    async fn result_post_clears_in_flight_queue_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let task = dummy_task();
        let result = A2ATaskResult::ok(task.id, vec![Content::text("done")]);

        let m = JsonlMailbox::open(path).await.unwrap();
        m.send_task(task.clone()).await.unwrap();
        let _ = m.try_recv_task_for(&task.recipient).await.unwrap().unwrap();
        assert_eq!(m.task_queue(10).await.unwrap().len(), 1);

        m.send_result(result.clone()).await.unwrap();
        assert!(m.task_queue(10).await.unwrap().is_empty());
        assert_eq!(m.recent_results(10).await.unwrap(), vec![result]);
    }

    #[tokio::test]
    async fn in_memory_requeue_restores_in_flight_task_and_increments_attempt() {
        let m = InMemoryMailbox::new();
        let task = dummy_task();
        m.send_task(task.clone()).await.unwrap();
        let _ = m.try_recv_task_for(&task.recipient).await.unwrap().unwrap();

        let in_flight = m.task_queue(10).await.unwrap();
        let lease_id = in_flight[0].lease_id;
        let outcome = m
            .repair_task(A2ARepairRequest {
                task_id: task.id,
                command: A2ARepairCommand::Requeue {
                    lease_id,
                    duplicate_risk: A2ADuplicateRisk::Idempotent,
                },
                reason: "worker crashed before posting a result".into(),
            })
            .await
            .unwrap();
        assert_eq!(outcome.action, A2ARepairAction::Requeued);
        assert_eq!(outcome.attempt, 1);

        let queued = m.task_queue(10).await.unwrap();
        assert_eq!(queued[0].state, A2ATaskQueueState::Queued);
        assert_eq!(queued[0].attempt, 1);

        let _ = m.try_recv_task_for(&task.recipient).await.unwrap().unwrap();
        let leased_again = m.task_queue(10).await.unwrap();
        assert_eq!(leased_again[0].attempt, 2);
    }

    #[tokio::test]
    async fn in_memory_repair_rejects_lease_mismatch() {
        let m = InMemoryMailbox::new();
        let task = dummy_task();
        m.send_task(task.clone()).await.unwrap();
        let _ = m.try_recv_task_for(&task.recipient).await.unwrap().unwrap();

        let result = m
            .repair_task(A2ARepairRequest {
                task_id: task.id,
                command: A2ARepairCommand::Requeue {
                    lease_id: Some(Uuid::new_v4()),
                    duplicate_risk: A2ADuplicateRisk::OperatorAccepted,
                },
                reason: "operator accepted duplicate risk".into(),
            })
            .await;
        assert!(matches!(result, Err(A2AError::LeaseMismatch { .. })));
    }

    #[tokio::test]
    async fn jsonl_requeue_replays_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let task = dummy_task();
        {
            let m = JsonlMailbox::open(path.clone()).await.unwrap();
            m.send_task(task.clone()).await.unwrap();
            let _ = m.try_recv_task_for(&task.recipient).await.unwrap().unwrap();
            let lease_id = m.task_queue(10).await.unwrap()[0].lease_id;
            m.repair_task(A2ARepairRequest {
                task_id: task.id,
                command: A2ARepairCommand::Requeue {
                    lease_id,
                    duplicate_risk: A2ADuplicateRisk::OperatorAccepted,
                },
                reason: "lease exceeded operator threshold".into(),
            })
            .await
            .unwrap();
        }

        let reopened = JsonlMailbox::open(path).await.unwrap();
        let queued = reopened.task_queue(10).await.unwrap();
        assert_eq!(queued[0].state, A2ATaskQueueState::Queued);
        assert_eq!(queued[0].attempt, 1);
        let _ = reopened
            .try_recv_task_for(&task.recipient)
            .await
            .unwrap()
            .unwrap();
        let leased_again = reopened.task_queue(10).await.unwrap();
        assert_eq!(leased_again[0].attempt, 2);
    }

    #[tokio::test]
    async fn jsonl_force_error_replays_pending_result() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let task = dummy_task();
        {
            let m = JsonlMailbox::open(path.clone()).await.unwrap();
            m.send_task(task.clone()).await.unwrap();
            let _ = m.try_recv_task_for(&task.recipient).await.unwrap().unwrap();
            let lease_id = m.task_queue(10).await.unwrap()[0].lease_id;
            let outcome = m
                .repair_task(A2ARepairRequest {
                    task_id: task.id,
                    command: A2ARepairCommand::ForceError {
                        lease_id,
                        message: "operator forced failure after stale lease".into(),
                    },
                    reason: "recipient process exited".into(),
                })
                .await
                .unwrap();
            assert_eq!(outcome.state, A2ARepairState::ResultPending);
        }

        let reopened = JsonlMailbox::open(path).await.unwrap();
        assert!(reopened.task_queue(10).await.unwrap().is_empty());
        let result = reopened
            .try_recv_result_for(&task.sender)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.status, A2ATaskStatus::Error);
        assert_eq!(
            result.error_message.as_deref(),
            Some("operator forced failure after stale lease")
        );
    }

    #[tokio::test]
    async fn jsonl_persists_senders_and_pending_results_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let t = dummy_task();
        let r = A2ATaskResult::ok(t.id, vec![Content::text("done")]);
        {
            let m = JsonlMailbox::open(path.clone()).await.unwrap();
            m.send_task(t.clone()).await.unwrap();
            // Drain the task — sender stays known so a result can still
            // be attributed to it.
            let _ = m.recv_task().await.unwrap();
            m.send_result(r.clone()).await.unwrap();
        }

        let m2 = JsonlMailbox::open(path.clone()).await.unwrap();
        assert_eq!(
            m2.lookup_task_sender(t.id).await.unwrap(),
            Some(t.sender.clone())
        );
        let got = m2.recv_result().await.unwrap();
        assert_eq!(got, r);
    }

    #[tokio::test]
    async fn jsonl_drained_results_do_not_reappear_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let t = dummy_task();
        let r = A2ATaskResult::ok(t.id, vec![]);
        {
            let m = JsonlMailbox::open(path.clone()).await.unwrap();
            m.send_task(t.clone()).await.unwrap();
            m.send_result(r.clone()).await.unwrap();
            assert!(m.try_recv_result_for(&t.sender).await.unwrap().is_some());
        }
        let m2 = JsonlMailbox::open(path).await.unwrap();
        assert!(m2.try_recv_result_for(&t.sender).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn try_recv_task_for_returns_only_recipient_match() {
        let m = InMemoryMailbox::new();
        let alice = dummy_agent("alice@local");
        let bob = dummy_agent("bob@local");
        let carol = dummy_agent("carol@local");
        // Alice -> Bob, Alice -> Carol, Alice -> Bob
        let to_bob_1 = A2ATask {
            id: Uuid::new_v4(),
            sender: alice.clone(),
            recipient: bob.clone(),
            intent_text: "1".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        let to_carol = A2ATask {
            id: Uuid::new_v4(),
            sender: alice.clone(),
            recipient: carol.clone(),
            intent_text: "2".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        let to_bob_2 = A2ATask {
            id: Uuid::new_v4(),
            sender: alice.clone(),
            recipient: bob.clone(),
            intent_text: "3".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        m.send_task(to_bob_1.clone()).await.unwrap();
        m.send_task(to_carol.clone()).await.unwrap();
        m.send_task(to_bob_2.clone()).await.unwrap();

        // Bob drains: gets the two addressed to him, in the order they were sent.
        let first = m.try_recv_task_for(&bob).await.unwrap().unwrap();
        assert_eq!(first.id, to_bob_1.id, "oldest match first");
        let second = m.try_recv_task_for(&bob).await.unwrap().unwrap();
        assert_eq!(second.id, to_bob_2.id);
        assert!(m.try_recv_task_for(&bob).await.unwrap().is_none());

        // Carol's task is still queued — Bob's drain didn't touch it.
        let carol_first = m.try_recv_task_for(&carol).await.unwrap().unwrap();
        assert_eq!(carol_first.id, to_carol.id);
    }

    #[tokio::test]
    async fn try_recv_task_for_skips_other_recipients() {
        let m = InMemoryMailbox::new();
        let bob = dummy_agent("bob@local");
        let stranger = dummy_agent("stranger@local");
        // Bob's task is queued; stranger tries to drain — gets nothing,
        // task is still there for Bob.
        let to_bob = A2ATask {
            id: Uuid::new_v4(),
            sender: dummy_agent("alice@local"),
            recipient: bob.clone(),
            intent_text: "for bob only".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        m.send_task(to_bob.clone()).await.unwrap();
        assert!(m.try_recv_task_for(&stranger).await.unwrap().is_none());
        let got = m.try_recv_task_for(&bob).await.unwrap().unwrap();
        assert_eq!(got.id, to_bob.id);
    }

    #[tokio::test]
    async fn try_recv_result_for_returns_only_results_for_peers_tasks() {
        let m = InMemoryMailbox::new();
        let alice = dummy_agent("alice@local");
        let dan = dummy_agent("dan@local");
        // Alice sends two tasks; Dan sends one.
        let alice_task_1 = A2ATask {
            id: Uuid::new_v4(),
            sender: alice.clone(),
            recipient: dummy_agent("research@local"),
            intent_text: "alice's".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        let alice_task_2 = A2ATask {
            id: Uuid::new_v4(),
            sender: alice.clone(),
            recipient: dummy_agent("research@local"),
            intent_text: "alice's other".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        let dan_task = A2ATask {
            id: Uuid::new_v4(),
            sender: dan.clone(),
            recipient: dummy_agent("research@local"),
            intent_text: "dan's".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        };
        m.send_task(alice_task_1.clone()).await.unwrap();
        m.send_task(alice_task_2.clone()).await.unwrap();
        m.send_task(dan_task.clone()).await.unwrap();

        // Results land in interleaved order — alice, dan, alice.
        m.send_result(A2ATaskResult::ok(alice_task_1.id, vec![]))
            .await
            .unwrap();
        m.send_result(A2ATaskResult::ok(dan_task.id, vec![]))
            .await
            .unwrap();
        m.send_result(A2ATaskResult::ok(alice_task_2.id, vec![]))
            .await
            .unwrap();

        // Alice drains hers first-then-second; Dan's stays.
        let first = m.try_recv_result_for(&alice).await.unwrap().unwrap();
        assert_eq!(first.task_id, alice_task_1.id);
        let second = m.try_recv_result_for(&alice).await.unwrap().unwrap();
        assert_eq!(second.task_id, alice_task_2.id);
        assert!(m.try_recv_result_for(&alice).await.unwrap().is_none());

        // Dan drains his.
        let dans = m.try_recv_result_for(&dan).await.unwrap().unwrap();
        assert_eq!(dans.task_id, dan_task.id);
    }

    #[tokio::test]
    async fn try_recv_result_for_skips_results_for_unknown_task_ids() {
        // Defence-in-depth: a result whose `task_id` is not in the senders
        // map (replay corruption / out-of-band write) is invisible to
        // every peer. The Sprint-43 audit-and-reject path covers `post`;
        // this covers `recv`.
        let m = InMemoryMailbox::new();
        let alice = dummy_agent("alice@local");
        m.send_result(A2ATaskResult::ok(Uuid::new_v4(), vec![]))
            .await
            .unwrap();
        assert!(m.try_recv_result_for(&alice).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn in_memory_compact_returns_zero() {
        let m = InMemoryMailbox::new();
        m.send_task(dummy_task()).await.unwrap();
        assert_eq!(m.compact().await.unwrap(), 0);
    }

    fn task_between(sender: AgentId, recipient: AgentId) -> A2ATask {
        A2ATask {
            id: Uuid::new_v4(),
            sender,
            recipient,
            intent_text: "x".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        }
    }

    async fn drive_round_trip(m: &JsonlMailbox, t: &A2ATask) {
        m.send_task(t.clone()).await.unwrap();
        let _ = m.try_recv_task_for(&t.recipient).await.unwrap().unwrap();
        m.send_result(A2ATaskResult::ok(t.id, vec![]))
            .await
            .unwrap();
        let _ = m.try_recv_result_for(&t.sender).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn jsonl_compact_drops_fully_resolved_task_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let m = JsonlMailbox::open(path.clone()).await.unwrap();

        let alice = dummy_agent("alice@local");
        let bob = dummy_agent("bob@local");
        let t = task_between(alice.clone(), bob.clone());
        drive_round_trip(&m, &t).await;

        // Pre-compact: 4 events on disk (TaskSent, TaskRecv, ResultPosted, ResultRecv).
        let raw = std::fs::read_to_string(&path).unwrap();
        let count = raw.lines().filter(|l| !l.is_empty()).count();
        assert_eq!(count, 4);

        let dropped = m.compact().await.unwrap();
        assert_eq!(dropped, 4);

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.lines().find(|l| !l.is_empty()).is_none());

        // Reopen — should yield empty in-memory state because every event
        // was for the now-resolved task.
        let m2 = JsonlMailbox::open(path).await.unwrap();
        assert!(m2.recent_tasks(10).await.unwrap().is_empty());
        assert!(m2.recent_results(10).await.unwrap().is_empty());
        assert!(m2.lookup_task_sender(t.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn jsonl_compact_keeps_in_flight_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let m = JsonlMailbox::open(path.clone()).await.unwrap();

        let alice = dummy_agent("alice@local");
        let bob = dummy_agent("bob@local");
        let carol = dummy_agent("carol@local");
        // Three recipients so each task drains independently when its
        // recipient asks for it. Otherwise `try_recv_task_for(bob)` on a
        // shared recipient queue would always drain the oldest match.
        let resolved = task_between(alice.clone(), bob.clone());
        let in_flight = task_between(alice.clone(), bob.clone());
        let no_result_yet = task_between(alice.clone(), carol.clone());

        drive_round_trip(&m, &resolved).await;
        // in_flight: send only — never drained.
        m.send_task(in_flight.clone()).await.unwrap();
        // no_result_yet: send + recv (different recipient so the drain
        // doesn't fight in_flight) but no result posted yet.
        m.send_task(no_result_yet.clone()).await.unwrap();
        let _ = m.try_recv_task_for(&carol).await.unwrap().unwrap();

        let dropped = m.compact().await.unwrap();
        assert_eq!(
            dropped, 4,
            "only the four events for the resolved task drop"
        );

        // Reopen — both in-flight tasks survive in their respective
        // states. `in_flight` is still a queued task; `no_result_yet`'s
        // task was drained, but its sender entry is still required so
        // a future `post_a2a_result` for it can be attributed.
        let m2 = JsonlMailbox::open(path).await.unwrap();
        let queued = m2.recent_tasks(10).await.unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].id, in_flight.id);
        assert_eq!(
            m2.lookup_task_sender(no_result_yet.id).await.unwrap(),
            Some(alice.clone()),
            "drained-but-no-result task keeps its sender entry"
        );
        assert!(m2.lookup_task_sender(resolved.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn jsonl_compact_no_op_when_nothing_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let m = JsonlMailbox::open(path.clone()).await.unwrap();

        // Two queued tasks, one drained-but-no-result task. None are
        // fully resolved, so the compact call must be a no-op.
        m.send_task(task_between(
            dummy_agent("alice@local"),
            dummy_agent("bob@local"),
        ))
        .await
        .unwrap();
        let drained = task_between(dummy_agent("alice@local"), dummy_agent("bob@local"));
        m.send_task(drained.clone()).await.unwrap();
        let _ = m
            .try_recv_task_for(&drained.recipient)
            .await
            .unwrap()
            .unwrap();

        let raw_before = std::fs::read_to_string(&path).unwrap();
        let dropped = m.compact().await.unwrap();
        assert_eq!(dropped, 0);
        let raw_after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            raw_before, raw_after,
            "no-op compact must not touch the file"
        );
        assert!(!path.with_extension("jsonl.tmp").exists());
    }

    #[tokio::test]
    async fn jsonl_compact_replay_yields_same_state_as_before() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");

        let alice = dummy_agent("alice@local");
        let bob = dummy_agent("bob@local");
        let resolved = task_between(alice.clone(), bob.clone());
        let queued = task_between(alice.clone(), bob.clone());

        let snapshot_pre = {
            let m = JsonlMailbox::open(path.clone()).await.unwrap();
            drive_round_trip(&m, &resolved).await;
            m.send_task(queued.clone()).await.unwrap();
            (
                m.recent_tasks(10).await.unwrap(),
                m.recent_results(10).await.unwrap(),
                m.lookup_task_sender(queued.id).await.unwrap(),
            )
        };

        // Compact in a fresh handle so the test is realistic — the
        // operator triggers compaction on a running daemon.
        let m_compact = JsonlMailbox::open(path.clone()).await.unwrap();
        m_compact.compact().await.unwrap();

        // Reopen and confirm the in-memory state matches.
        let m_post = JsonlMailbox::open(path).await.unwrap();
        let snapshot_post = (
            m_post.recent_tasks(10).await.unwrap(),
            m_post.recent_results(10).await.unwrap(),
            m_post.lookup_task_sender(queued.id).await.unwrap(),
        );
        assert_eq!(snapshot_pre, snapshot_post);
    }

    #[tokio::test]
    async fn jsonl_compact_does_not_drop_results_with_unposted_tasks() {
        // Defence-in-depth: a `ResultPosted` whose `task_id` was never
        // `TaskSent` is a corruption signal. The compact logic uses
        // `seen` (TaskSent membership) as the gate, so an orphaned
        // result is not droppable — it survives and can be inspected.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let m = JsonlMailbox::open(path.clone()).await.unwrap();

        let orphan_task_id = Uuid::new_v4();
        m.send_result(A2ATaskResult::ok(orphan_task_id, vec![]))
            .await
            .unwrap();

        let dropped = m.compact().await.unwrap();
        assert_eq!(dropped, 0);

        // Surviving event still on disk and still replays.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains(&orphan_task_id.to_string()));
    }
}
