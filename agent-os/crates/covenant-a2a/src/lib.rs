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
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct A2ATask {
    pub id: Uuid,
    pub sender: AgentId,
    pub recipient: AgentId,
    pub intent_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
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

/// In-process FIFO mailbox. Useful for tests and for orchestrator agents
/// that fan tasks within the same daemon.
pub struct InMemoryMailbox {
    tasks: Mutex<VecDeque<A2ATask>>,
    results: Mutex<VecDeque<A2ATaskResult>>,
    /// Permanent record of who sent each task, populated on
    /// [`Mailbox::send_task`] and never pruned. The daemon uses this map
    /// to attribute `PostA2AResult` calls back to the original sender so
    /// the capability check can use the sender-scoped action.
    senders: Mutex<HashMap<Uuid, AgentId>>,
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
            senders: Mutex::new(HashMap::new()),
            task_notify: Notify::new(),
            result_notify: Notify::new(),
        }
    }
}

#[async_trait]
impl Mailbox for InMemoryMailbox {
    async fn send_task(&self, task: A2ATask) -> Result<(), A2AError> {
        self.senders
            .lock()
            .unwrap()
            .insert(task.id, task.sender.clone());
        self.tasks.lock().unwrap().push_back(task);
        self.task_notify.notify_one();
        Ok(())
    }

    async fn recv_task(&self) -> Result<A2ATask, A2AError> {
        loop {
            if let Some(t) = self.tasks.lock().unwrap().pop_front() {
                return Ok(t);
            }
            self.task_notify.notified().await;
        }
    }

    async fn try_recv_task_for(&self, recipient: &AgentId) -> Result<Option<A2ATask>, A2AError> {
        let mut tasks = self.tasks.lock().unwrap();
        let Some(pos) = tasks.iter().position(|t| t.recipient == *recipient) else {
            return Ok(None);
        };
        Ok(tasks.remove(pos))
    }

    async fn send_result(&self, result: A2ATaskResult) -> Result<(), A2AError> {
        self.results.lock().unwrap().push_back(result);
        self.result_notify.notify_one();
        Ok(())
    }

    async fn recv_result(&self) -> Result<A2ATaskResult, A2AError> {
        loop {
            if let Some(r) = self.results.lock().unwrap().pop_front() {
                return Ok(r);
            }
            self.result_notify.notified().await;
        }
    }

    async fn try_recv_result_for(&self, peer: &AgentId) -> Result<Option<A2ATaskResult>, A2AError> {
        let senders = self.senders.lock().unwrap();
        let mut results = self.results.lock().unwrap();
        let pos = results
            .iter()
            .position(|r| senders.get(&r.task_id).map(|s| s == peer).unwrap_or(false));
        Ok(pos.and_then(|p| results.remove(p)))
    }

    async fn recent_tasks(&self, limit: usize) -> Result<Vec<A2ATask>, A2AError> {
        Ok(self
            .tasks
            .lock()
            .unwrap()
            .iter()
            .take(limit)
            .cloned()
            .collect())
    }

    async fn recent_results(&self, limit: usize) -> Result<Vec<A2ATaskResult>, A2AError> {
        Ok(self
            .results
            .lock()
            .unwrap()
            .iter()
            .take(limit)
            .cloned()
            .collect())
    }

    async fn lookup_task_sender(&self, task_id: Uuid) -> Result<Option<AgentId>, A2AError> {
        Ok(self.senders.lock().unwrap().get(&task_id).cloned())
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
    TaskSent { task: A2ATask },
    TaskRecv { task_id: Uuid },
    ResultPosted { result: A2ATaskResult },
    ResultRecv { task_id: Uuid },
}

struct MailboxState {
    tasks: VecDeque<A2ATask>,
    results: VecDeque<A2ATaskResult>,
    senders: HashMap<Uuid, AgentId>,
}

impl MailboxState {
    fn empty() -> Self {
        Self {
            tasks: VecDeque::new(),
            results: VecDeque::new(),
            senders: HashMap::new(),
        }
    }

    fn apply(&mut self, ev: MailboxEvent) {
        match ev {
            MailboxEvent::TaskSent { task } => {
                self.senders.insert(task.id, task.sender.clone());
                self.tasks.push_back(task);
            }
            MailboxEvent::TaskRecv { task_id } => {
                if let Some(pos) = self.tasks.iter().position(|t| t.id == task_id) {
                    self.tasks.remove(pos);
                }
            }
            MailboxEvent::ResultPosted { result } => {
                self.results.push_back(result);
            }
            MailboxEvent::ResultRecv { task_id } => {
                if let Some(pos) = self.results.iter().position(|r| r.task_id == task_id) {
                    self.results.remove(pos);
                }
            }
        }
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
        let _g = self.file_lock.lock().await;
        self.append(&MailboxEvent::TaskSent { task: task.clone() })
            .await?;
        {
            let mut s = self.state.lock().unwrap();
            s.senders.insert(task.id, task.sender.clone());
            s.tasks.push_back(task);
        }
        self.task_notify.notify_one();
        Ok(())
    }

    async fn recv_task(&self) -> Result<A2ATask, A2AError> {
        loop {
            {
                let _g = self.file_lock.lock().await;
                let front_id = self.state.lock().unwrap().tasks.front().map(|t| t.id);
                if let Some(id) = front_id {
                    self.append(&MailboxEvent::TaskRecv { task_id: id }).await?;
                    if let Some(t) = self.state.lock().unwrap().tasks.pop_front() {
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
            let s = self.state.lock().unwrap();
            s.tasks
                .iter()
                .find(|t| t.recipient == *recipient)
                .map(|t| t.id)
        };
        let Some(id) = target_id else { return Ok(None) };
        self.append(&MailboxEvent::TaskRecv { task_id: id }).await?;
        let mut s = self.state.lock().unwrap();
        let pos = s.tasks.iter().position(|t| t.id == id);
        Ok(pos.and_then(|p| s.tasks.remove(p)))
    }

    async fn send_result(&self, result: A2ATaskResult) -> Result<(), A2AError> {
        let _g = self.file_lock.lock().await;
        self.append(&MailboxEvent::ResultPosted {
            result: result.clone(),
        })
        .await?;
        self.state.lock().unwrap().results.push_back(result);
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
                    .unwrap()
                    .results
                    .front()
                    .map(|r| r.task_id);
                if let Some(id) = front_id {
                    self.append(&MailboxEvent::ResultRecv { task_id: id })
                        .await?;
                    if let Some(r) = self.state.lock().unwrap().results.pop_front() {
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
            let s = self.state.lock().unwrap();
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
        let mut s = self.state.lock().unwrap();
        let pos = s.results.iter().position(|r| r.task_id == task_id);
        Ok(pos.and_then(|p| s.results.remove(p)))
    }

    async fn recent_tasks(&self, limit: usize) -> Result<Vec<A2ATask>, A2AError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .tasks
            .iter()
            .take(limit)
            .cloned()
            .collect())
    }

    async fn recent_results(&self, limit: usize) -> Result<Vec<A2ATaskResult>, A2AError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .results
            .iter()
            .take(limit)
            .cloned()
            .collect())
    }

    async fn lookup_task_sender(&self, task_id: Uuid) -> Result<Option<AgentId>, A2AError> {
        Ok(self.state.lock().unwrap().senders.get(&task_id).cloned())
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
        let mut s = self.state.lock().unwrap();
        for tid in &droppable {
            s.senders.remove(tid);
        }
        Ok(dropped)
    }
}

fn compute_droppable_task_ids(events: &[MailboxEvent]) -> std::collections::HashSet<Uuid> {
    use std::collections::{HashMap, HashSet};
    // Per task_id: count TaskRecv, ResultPosted, ResultRecv. TaskSent
    // is implicit from membership in `seen`.
    let mut seen: HashSet<Uuid> = HashSet::new();
    let mut recv: HashSet<Uuid> = HashSet::new();
    let mut posted: HashMap<Uuid, u64> = HashMap::new();
    let mut drained: HashMap<Uuid, u64> = HashMap::new();
    for ev in events {
        match ev {
            MailboxEvent::TaskSent { task } => {
                seen.insert(task.id);
            }
            MailboxEvent::TaskRecv { task_id } => {
                recv.insert(*task_id);
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
        .filter(|tid| recv.contains(tid))
        .filter(|tid| {
            let p = posted.get(tid).copied().unwrap_or(0);
            let d = drained.get(tid).copied().unwrap_or(0);
            p > 0 && p == d
        })
        .collect()
}

fn event_belongs_to_droppable(
    ev: &MailboxEvent,
    droppable: &std::collections::HashSet<Uuid>,
) -> bool {
    match ev {
        MailboxEvent::TaskSent { task } => droppable.contains(&task.id),
        MailboxEvent::TaskRecv { task_id } => droppable.contains(task_id),
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
            parent: None,
            deadline_ms: None,
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
    fn task_skips_optional_fields_when_none() {
        let t = dummy_task();
        let s = serde_json::to_string(&t).unwrap();
        assert!(!s.contains("parent"));
        assert!(!s.contains("deadline_ms"));
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
            parent: None,
            deadline_ms: None,
        };
        let to_carol = A2ATask {
            id: Uuid::new_v4(),
            sender: alice.clone(),
            recipient: carol.clone(),
            intent_text: "2".into(),
            parent: None,
            deadline_ms: None,
        };
        let to_bob_2 = A2ATask {
            id: Uuid::new_v4(),
            sender: alice.clone(),
            recipient: bob.clone(),
            intent_text: "3".into(),
            parent: None,
            deadline_ms: None,
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
            parent: None,
            deadline_ms: None,
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
            parent: None,
            deadline_ms: None,
        };
        let alice_task_2 = A2ATask {
            id: Uuid::new_v4(),
            sender: alice.clone(),
            recipient: dummy_agent("research@local"),
            intent_text: "alice's other".into(),
            parent: None,
            deadline_ms: None,
        };
        let dan_task = A2ATask {
            id: Uuid::new_v4(),
            sender: dan.clone(),
            recipient: dummy_agent("research@local"),
            intent_text: "dan's".into(),
            parent: None,
            deadline_ms: None,
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
            parent: None,
            deadline_ms: None,
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
