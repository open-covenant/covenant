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
use std::sync::Mutex;
use tokio::sync::Notify;
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
}

/// Per-agent inbox. Tasks land in `recv_task`; results for tasks the agent
/// itself dispatched land in `recv_result`. Both `recv_*` are async and
/// resolve when something is available; impl is responsible for fairness.
#[async_trait]
pub trait Mailbox: Send + Sync {
    async fn send_task(&self, task: A2ATask) -> Result<(), A2AError>;
    async fn recv_task(&self) -> Result<A2ATask, A2AError>;
    /// Non-blocking variant of `recv_task` for RPC-style callers (HTTP /
    /// IPC) that want a single round-trip.
    async fn try_recv_task(&self) -> Result<Option<A2ATask>, A2AError>;
    async fn send_result(&self, result: A2ATaskResult) -> Result<(), A2AError>;
    async fn recv_result(&self) -> Result<A2ATaskResult, A2AError>;
    async fn try_recv_result(&self) -> Result<Option<A2ATaskResult>, A2AError>;

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

    async fn try_recv_task(&self) -> Result<Option<A2ATask>, A2AError> {
        Ok(self.tasks.lock().unwrap().pop_front())
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

    async fn try_recv_result(&self) -> Result<Option<A2ATaskResult>, A2AError> {
        Ok(self.results.lock().unwrap().pop_front())
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
        assert!(m.try_recv_task().await.unwrap().is_none());
        m.send_task(dummy_task()).await.unwrap();
        let got = m.try_recv_task().await.unwrap();
        assert!(got.is_some());
        // After draining, empty again.
        assert!(m.try_recv_task().await.unwrap().is_none());
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
        assert!(m.try_recv_task().await.unwrap().is_some());
        assert!(m.try_recv_task().await.unwrap().is_some());
        assert!(m.try_recv_task().await.unwrap().is_some());
        assert!(m.try_recv_task().await.unwrap().is_none());
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
}
