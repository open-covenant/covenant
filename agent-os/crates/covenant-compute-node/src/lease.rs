//! Serving a lease session: hold a machine open for a buyer, publish
//! how to reach it, and stop the moment the buyer says stop.
//!
//! Every other job kind runs to completion on its own. A session ends
//! only when someone ends it, and the buyer is paying by the second the
//! whole time — so the two things that matter here are that the access
//! details reach the buyer as early as possible, and that a close takes
//! effect promptly. Both are latency the buyer is literally paying for.
//!
//! The close signal arrives out of band: the serve loop polls the
//! coordinator for it and flips a flag in [`LeaseControl`], which the
//! executor is waiting on. Routing it through shared state rather than
//! the executor's own arguments keeps [`crate::JobExecutor`] unchanged
//! for every other backend.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use covenant_compute_protocol::{lease_access_chunk, JobEnvelopePayload, JobKind, LeaseAccess};
use covenant_mcp::Content;
use parking_lot::Mutex;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::executor::{ChunkSink, ExecutionOutcome, ExecutorError, JobExecutor};

/// Shared close flags for the sessions this node is serving. The serve
/// loop writes, the executor reads.
#[derive(Default)]
pub struct LeaseControl {
    closed: Mutex<HashMap<Uuid, bool>>,
    changed: Notify,
}

impl LeaseControl {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Marks a session as closed and wakes whoever is serving it.
    pub fn close(&self, job_id: Uuid) {
        self.closed.lock().insert(job_id, true);
        self.changed.notify_waiters();
    }

    pub fn is_closed(&self, job_id: Uuid) -> bool {
        self.closed.lock().get(&job_id).copied().unwrap_or(false)
    }

    /// Drops a finished session's flag — the map tracks live sessions,
    /// not history.
    pub fn forget(&self, job_id: Uuid) {
        self.closed.lock().remove(&job_id);
    }

    /// Resolves when `job_id` is closed or `timeout` elapses; returns
    /// whether it was closed. Waiting on the notify rather than polling
    /// is what makes a close take effect in milliseconds — seconds the
    /// buyer would otherwise be billed for.
    pub async fn closed_or_timeout(&self, job_id: Uuid, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.is_closed(job_id) {
                return true;
            }
            let waiter = self.changed.notified();
            // Re-check after arming the waiter: a close landing in the
            // gap would otherwise be missed until the next wakeup.
            if self.is_closed(job_id) {
                return true;
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return false;
            }
            if tokio::time::timeout(deadline - now, waiter).await.is_err() {
                return self.is_closed(job_id);
            }
        }
    }
}

/// What a backend must do to put a real machine behind a lease:
/// bring a session up for `job`, and tear it down when told. Keep
/// `open` quick. The market bills the operator for the box from the
/// moment it is created, and the buyer's meter runs alongside it: the
/// coordinator stamps that meter at accept, before `open` returns, so
/// the seconds a session spends coming up are billed to the buyer
/// today. A backend that cannot come up should fail `open` rather than
/// hold a box the buyer is paying for and cannot use; a failed lease
/// refunds them whole.
// async_trait adds #[must_use] to already-must-use boxed futures.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait SessionBackend: Send + Sync {
    /// Brings a session up and returns where it can be reached.
    async fn open(&self, job: &JobEnvelopePayload) -> Result<LeaseAccess, ExecutorError>;
    /// Optional lease-bound evidence after the access grant is queued. Backends
    /// must account for coordinator persistence lag; this is not proof of delivery.
    async fn after_access(
        &self,
        _job: &JobEnvelopePayload,
        _access: &LeaseAccess,
    ) -> Result<(), ExecutorError> {
        Ok(())
    }
    /// Releases the machine. Called exactly once per opened session,
    /// on close, on deadline, and on error paths — an operator that
    /// leaks a box here pays for it.
    async fn close(&self, job_id: Uuid);
}

/// A session backend that opens nothing: it reports a fixed endpoint
/// and holds the lease open. What the hermetic tests exercise, and the
/// honest stand-in for a node whose access path is not yet wired.
pub struct StubSessionBackend {
    endpoint: String,
}

impl StubSessionBackend {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }
}

#[async_trait]
impl SessionBackend for StubSessionBackend {
    async fn open(&self, job: &JobEnvelopePayload) -> Result<LeaseAccess, ExecutorError> {
        Ok(LeaseAccess {
            ssh: None,
            source_price: None,
            job_id: job.job_id,
            endpoint: self.endpoint.clone(),
            ready_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            note: Some("stub session: no machine is behind this endpoint".into()),
        })
    }

    async fn close(&self, _job_id: Uuid) {}
}

/// Where a node learns that a buyer closed a session. The coordinator
/// records the close; the serving node has to go and see it, because
/// nothing pushes to a node that only ever dials out.
// async_trait adds #[must_use] to already-must-use boxed futures.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait LeaseCloseSource: Send + Sync {
    async fn is_closed(&self, job_id: Uuid) -> bool;
}

/// How often a running session asks the coordinator whether it has
/// been closed. Every second of lag here is a second the buyer is
/// billed for a machine they already let go, so this is deliberately
/// tighter than any other poll in the node.
pub const CLOSE_POLL_INTERVAL: Duration = Duration::from_millis(750);

/// Runs a lease: open the session, publish the access grant as the
/// first chunk, hold until the buyer closes or the window runs out,
/// then release the machine.
pub struct LeaseExecutor {
    backend: Arc<dyn SessionBackend>,
    control: Arc<LeaseControl>,
    close_source: Option<Arc<dyn LeaseCloseSource>>,
    poll_interval: Duration,
}

struct CloseWatch(Option<tokio::task::JoinHandle<()>>);
impl Drop for CloseWatch {
    fn drop(&mut self) {
        if let Some(watch) = self.0.take() {
            watch.abort();
        }
    }
}

impl LeaseExecutor {
    pub fn new(backend: Arc<dyn SessionBackend>, control: Arc<LeaseControl>) -> Self {
        Self {
            backend,
            control,
            close_source: None,
            poll_interval: CLOSE_POLL_INTERVAL,
        }
    }

    /// Watches `source` for the buyer's close while a session runs.
    /// Without one the session only ends at its window — correct, but
    /// it bills every buyer the whole ceiling.
    pub fn watching(mut self, source: Arc<dyn LeaseCloseSource>) -> Self {
        self.close_source = Some(source);
        self
    }

    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Polls the close source into the shared flag until the session
    /// ends. Returns when a close is seen; the caller drops it
    /// otherwise.
    fn spawn_close_watch(&self, job_id: Uuid) -> Option<tokio::task::JoinHandle<()>> {
        let source = self.close_source.clone()?;
        let control = self.control.clone();
        let interval = self.poll_interval;
        Some(tokio::spawn(async move {
            loop {
                if control.is_closed(job_id) {
                    return;
                }
                if source.is_closed(job_id).await {
                    control.close(job_id);
                    return;
                }
                tokio::time::sleep(interval).await;
            }
        }))
    }
}

#[async_trait]
impl JobExecutor for LeaseExecutor {
    async fn execute(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        // Without a sink the buyer has no way to learn the endpoint
        // before the session ends, which makes the lease useless to
        // them. Refuse rather than burn their window.
        let _ = (job, deadline);
        Err(ExecutorError::Failed(
            "a lease session must be run as a streaming job so its access grant can \
             reach the buyer while the session is live"
                .into(),
        ))
    }

    async fn execute_streaming(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
        sink: ChunkSink,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        if job.kind != JobKind::LeaseSession {
            return Err(ExecutorError::Failed(format!(
                "a lease executor cannot serve a {:?} job",
                job.kind
            )));
        }
        let terms = covenant_compute_protocol::parse_lease_terms(&job.input)
            .map_err(|e| ExecutorError::Failed(format!("lease terms unreadable: {e}")))?
            .ok_or_else(|| ExecutorError::Failed("lease carries no terms".into()))?;
        let phala_expiry = covenant_compute_protocol::phala::phala_lease_window(job)
            .map_err(|e| ExecutorError::Failed(e.to_string()))?
            .map(|(_, expiry)| expiry);
        let signed_remaining = |expiry: u64| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(u64::MAX);
            Duration::from_millis(expiry.saturating_sub(now))
        };
        let window = Duration::from_secs(terms.max_duration_secs)
            .min(deadline)
            .min(phala_expiry.map(&signed_remaining).unwrap_or(Duration::MAX));
        if window.is_zero() {
            return Err(ExecutorError::Failed("signed lease has expired".into()));
        }

        let started = std::time::Instant::now();
        let _watch = CloseWatch(self.spawn_close_watch(job.job_id));
        let opening = tokio::select! {
            biased;
            _ = self.control.closed_or_timeout(job.job_id, window) => {
                self.backend.close(job.job_id).await;
                self.control.forget(job.job_id);
                return Err(ExecutorError::Failed("lease closed or expired while opening".into()));
            }
            result = tokio::time::timeout(window, self.backend.open(job)) => result,
        };
        let access = match opening {
            Ok(Ok(access)) => access,
            Ok(Err(error)) => return Err(error),
            Err(_) => {
                self.backend.close(job.job_id).await;
                return Err(ExecutorError::Failed("lease expired while opening".into()));
            }
        };
        let grant = match lease_access_chunk(access.clone()) {
            Ok(grant) => grant,
            Err(e) => {
                // The box is up and billing the moment `open` returns. A
                // grant we cannot publish still has to release it, the same
                // as every other exit from a live session — the backend's
                // close contract holds on error paths too.
                self.backend.close(job.job_id).await;
                self.control.forget(job.job_id);
                return Err(ExecutorError::Failed(format!("access grant: {e}")));
            }
        };
        // Publish before waiting: the buyer is already being billed,
        // and until this lands they are paying for a machine they
        // cannot reach. The chunk stream is text, so the grant rides as
        // its JSON — the same block the final output carries, so a
        // buyer who missed the stream still finds it on the receipt.
        if let Content::Json { value } = &grant {
            if sink.send(value.to_string()).await.is_err() {
                self.backend.close(job.job_id).await;
                self.control.forget(job.job_id);
                return Err(ExecutorError::Failed("lease access receiver closed".into()));
            }
        }
        let evidence_window = window
            .saturating_sub(started.elapsed())
            .min(phala_expiry.map(&signed_remaining).unwrap_or(Duration::MAX));
        let evidence = tokio::select! {
            biased;
            _ = self.control.closed_or_timeout(job.job_id, evidence_window) => {
                self.backend.close(job.job_id).await;
                self.control.forget(job.job_id);
                return Err(ExecutorError::Failed("lease closed or expired collecting evidence".into()));
            }
            result = tokio::time::timeout(evidence_window, self.backend.after_access(job, &access)) => result,
        };
        if !matches!(evidence, Ok(Ok(()))) {
            self.backend.close(job.job_id).await;
            self.control.forget(job.job_id);
            return match evidence {
                Ok(Err(error)) => Err(error),
                _ => Err(ExecutorError::Failed(
                    "lease expired collecting evidence".into(),
                )),
            };
        }

        // The session runs until the buyer closes it or the window
        // ends, whichever is first — and never past the job's own
        // deadline, which is the coordinator's refund trigger.
        let remaining = window
            .saturating_sub(started.elapsed())
            .min(phala_expiry.map(&signed_remaining).unwrap_or(Duration::MAX));
        let closed = self.control.closed_or_timeout(job.job_id, remaining).await;
        // Ephemeral receipt runtime ends before asynchronous teardown. Other
        // backends retain their existing receipt semantics.
        let service_wall_ms =
            covenant_compute_protocol::phala::parse_phala_ephemeral_lease_spec(&job.input)
                .map_err(|error| ExecutorError::Failed(error.to_string()))?
                .map(|_| started.elapsed().as_millis() as u64);
        self.backend.close(job.job_id).await;
        let wall_ms = service_wall_ms.unwrap_or_else(|| started.elapsed().as_millis() as u64);
        self.control.forget(job.job_id);

        let ended = if closed {
            "closed by the buyer"
        } else {
            "ended at the window"
        };
        Ok(ExecutionOutcome {
            output: vec![
                grant,
                Content::text(format!("lease session {ended} after {}ms", wall_ms)),
            ],
            wall_ms,
            tokens_in: None,
            tokens_out: None,
            // `FinishReason` describes how a generation stopped; a
            // session has no such notion, and mislabelling one as
            // `Stop` would put a generation's vocabulary on a receipt
            // for a rented machine.
            finish_reason: None,
        })
    }

    async fn health(&self) -> Result<(), ExecutorError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_close_resolves_the_wait_immediately() {
        let control = LeaseControl::new();
        let job_id = Uuid::new_v4();
        let waiter = {
            let control = control.clone();
            tokio::spawn(async move {
                control
                    .closed_or_timeout(job_id, Duration::from_secs(30))
                    .await
            })
        };
        // Give the waiter a moment to arm, then close.
        tokio::time::sleep(Duration::from_millis(50)).await;
        control.close(job_id);
        assert!(
            waiter.await.unwrap(),
            "the wait ends on close, not on timeout"
        );
    }

    #[tokio::test]
    async fn a_wait_with_no_close_ends_at_its_window() {
        let control = LeaseControl::new();
        let job_id = Uuid::new_v4();
        let started = std::time::Instant::now();
        let closed = control
            .closed_or_timeout(job_id, Duration::from_millis(120))
            .await;
        assert!(!closed);
        assert!(started.elapsed() >= Duration::from_millis(100));
    }

    #[tokio::test]
    async fn a_close_that_lands_before_the_wait_is_still_seen() {
        let control = LeaseControl::new();
        let job_id = Uuid::new_v4();
        control.close(job_id);
        assert!(
            control
                .closed_or_timeout(job_id, Duration::from_millis(50))
                .await,
            "a close is a latch, not an edge — arriving early must not lose it"
        );
    }

    #[tokio::test]
    async fn forget_drops_a_finished_sessions_flag() {
        let control = LeaseControl::new();
        let job_id = Uuid::new_v4();
        control.close(job_id);
        assert!(control.is_closed(job_id));
        control.forget(job_id);
        assert!(!control.is_closed(job_id));
    }

    /// A backend whose session comes up but reports an endpoint the access
    /// grant rejects, recording every `close` so a leaked box is visible.
    struct UnpublishableGrantBackend {
        closed: Arc<Mutex<Vec<Uuid>>>,
    }

    #[async_trait]
    impl SessionBackend for UnpublishableGrantBackend {
        async fn open(&self, job: &JobEnvelopePayload) -> Result<LeaseAccess, ExecutorError> {
            Ok(LeaseAccess {
                ssh: None,
                source_price: None,
                job_id: job.job_id,
                endpoint: String::new(),
                ready_at_ms: 0,
                note: None,
            })
        }
        async fn close(&self, job_id: Uuid) {
            self.closed.lock().push(job_id);
        }
    }

    fn lease_job() -> JobEnvelopePayload {
        use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
        use covenant_compute_protocol::{lease_input, CapabilityRequirement, LeaseTerms};
        use covenant_types::AgentId;
        let terms = LeaseTerms {
            max_duration_secs: 60,
            rate_micro_usdc_per_sec: 10,
            client_public_key: None,
        };
        JobEnvelopePayload {
            job_id: Uuid::new_v4(),
            buyer: AgentId::new("buyer@local", [1u8; 32]),
            kind: JobKind::LeaseSession,
            capability_requirement: CapabilityRequirement {
                inference_tariff: None,
                gpu_class: None,
                min_vram_gb: None,
                model_id: None,
                kind: JobKind::LeaseSession,
                max_duration_secs: 60,
                min_reputation_bps: None,
            },
            input: vec![lease_input(terms).unwrap()],
            price_micro_usdc: 600,
            deadline_ms: 120_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "lease-test"),
            issued_at_ms: 0,
            referral_code: None,
            stream: false,
        }
    }

    #[tokio::test]
    async fn an_unpublishable_grant_still_releases_the_box() {
        let closed = Arc::new(Mutex::new(Vec::new()));
        let backend = Arc::new(UnpublishableGrantBackend {
            closed: closed.clone(),
        });
        let executor = LeaseExecutor::new(backend, LeaseControl::new());
        let job = lease_job();
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let err = executor
            .execute_streaming(&job, Duration::from_secs(120), tx)
            .await
            .expect_err("an unpublishable grant fails the job");
        assert!(err.to_string().contains("access grant"), "got: {err}");
        // The box came up on `open`; failing before the grant is published
        // must still release it, or the operator pays for a machine no one
        // can reach.
        assert_eq!(closed.lock().as_slice(), &[job.job_id]);
    }

    /// A backend that comes up reachable and records every release.
    struct RunningBackend {
        closed: Arc<Mutex<Vec<Uuid>>>,
    }

    #[async_trait]
    impl SessionBackend for RunningBackend {
        async fn open(&self, job: &JobEnvelopePayload) -> Result<LeaseAccess, ExecutorError> {
            Ok(LeaseAccess {
                ssh: None,
                source_price: None,
                job_id: job.job_id,
                endpoint: "ssh renter@203.0.113.7 -p 2222".into(),
                ready_at_ms: 0,
                note: None,
            })
        }
        async fn close(&self, job_id: Uuid) {
            self.closed.lock().push(job_id);
        }
    }

    /// A close source that already reports the buyer let the session go —
    /// what a node's poll finds once the coordinator has recorded the close.
    struct BuyerHasClosed;

    #[async_trait]
    impl LeaseCloseSource for BuyerHasClosed {
        async fn is_closed(&self, _job_id: Uuid) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn a_buyer_close_releases_the_box_and_the_grant_reaches_the_buyer() {
        let closed = Arc::new(Mutex::new(Vec::new()));
        let backend = Arc::new(RunningBackend {
            closed: closed.clone(),
        });
        let executor = LeaseExecutor::new(backend, LeaseControl::new())
            .watching(Arc::new(BuyerHasClosed))
            .with_poll_interval(Duration::from_millis(1));
        let job = lease_job();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);

        let outcome = executor
            .execute_streaming(&job, Duration::from_secs(2), tx)
            .await
            .expect("a served lease session concludes cleanly");

        // The buyer's close ended the session, and the machine was
        // released — a node that never acts on the close bills the buyer
        // for a box they already let go.
        assert_eq!(closed.lock().as_slice(), &[job.job_id]);
        let summary = outcome
            .output
            .iter()
            .find_map(|c| match c {
                Content::Text { text } => Some(text.clone()),
                Content::Json { .. } => None,
            })
            .expect("a concluded session carries a text summary");
        assert!(summary.contains("closed by the buyer"), "got: {summary}");
        // A rented machine is not a generation: it stamps no finish_reason.
        assert!(outcome.finish_reason.is_none());

        // The access grant went out on the live stream before the wait, so
        // the buyer can reach what they are already paying for.
        let first = rx.recv().await.expect("the grant streams first");
        assert!(first.contains("lease_access"), "got: {first}");
    }

    #[tokio::test]
    async fn a_session_that_reaches_its_window_end_still_releases_the_box() {
        // No buyer close ever arrives, so the session runs to its window and
        // ends on its own. The box must still be released: one left running
        // past its window bills the operator with no one paying.
        let closed = Arc::new(Mutex::new(Vec::new()));
        let backend = Arc::new(RunningBackend {
            closed: closed.clone(),
        });
        let executor = LeaseExecutor::new(backend, LeaseControl::new());
        let job = lease_job();
        let (tx, _rx) = tokio::sync::mpsc::channel(8);

        // The short deadline caps the window; nothing ever closes the lease.
        let outcome = executor
            .execute_streaming(&job, Duration::from_millis(50), tx)
            .await
            .expect("a session that runs its full window concludes cleanly");

        assert_eq!(
            closed.lock().as_slice(),
            &[job.job_id],
            "the box is released when the window ends, not only on a buyer close"
        );
        let summary = outcome
            .output
            .iter()
            .find_map(|c| match c {
                Content::Text { text } => Some(text.clone()),
                Content::Json { .. } => None,
            })
            .expect("a concluded session carries a text summary");
        assert!(summary.contains("ended at the window"), "got: {summary}");
    }

    struct EvidenceBackend {
        closed: Arc<Mutex<Vec<Uuid>>>,
        reject: bool,
    }
    #[async_trait]
    impl SessionBackend for EvidenceBackend {
        async fn open(&self, job: &JobEnvelopePayload) -> Result<LeaseAccess, ExecutorError> {
            RunningBackend {
                closed: self.closed.clone(),
            }
            .open(job)
            .await
        }
        async fn after_access(
            &self,
            _: &JobEnvelopePayload,
            _: &LeaseAccess,
        ) -> Result<(), ExecutorError> {
            if self.reject {
                return Err(ExecutorError::Failed("evidence rejected".into()));
            }
            std::future::pending().await
        }
        async fn close(&self, id: Uuid) {
            self.closed.lock().push(id);
        }
    }

    #[tokio::test]
    async fn evidence_failure_and_timeout_both_release_the_machine() {
        for reject in [true, false] {
            let closed = Arc::new(Mutex::new(Vec::new()));
            let executor = LeaseExecutor::new(
                Arc::new(EvidenceBackend {
                    closed: closed.clone(),
                    reject,
                }),
                LeaseControl::new(),
            );
            let job = lease_job();
            let (tx, mut rx) = tokio::sync::mpsc::channel(2);
            assert!(executor
                .execute_streaming(&job, Duration::from_millis(30), tx)
                .await
                .is_err());
            assert!(rx.recv().await.unwrap().contains("lease_access"));
            assert_eq!(closed.lock().as_slice(), &[job.job_id]);
        }
    }

    #[tokio::test]
    async fn phala_window_ends_at_signed_expiry_including_queue_delay() {
        use base64::Engine as _;
        use covenant_compute_protocol::phala::{
            phala_lease_input, phala_workload, reviewed_phala_wrapper, PHALA_CPU_MODEL,
        };
        use covenant_compute_protocol::{lease_input, LeaseTerms};
        let mut job = lease_job();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        job.issued_at_ms = now - 59_700;
        job.capability_requirement.gpu_class = Some("cpu".into());
        job.capability_requirement.model_id = Some(PHALA_CPU_MODEL.into());
        let mut key = b"\0\0\0\x0bssh-ed25519\0\0\0\x20".to_vec();
        key.extend_from_slice(&[7; 32]);
        job.input = vec![lease_input(LeaseTerms {
            max_duration_secs: 60,
            rate_micro_usdc_per_sec: 10,
            client_public_key: Some(format!(
                "ssh-ed25519 {}",
                base64::engine::general_purpose::STANDARD.encode(key)
            )),
        })
        .unwrap()];
        let (_, spec) = phala_workload(
            &job,
            &bs58::encode([3; 32]).into_string(),
            &reviewed_phala_wrapper(),
            &format!("sha256:{}", "12".repeat(32)),
        )
        .unwrap();
        job.input.push(phala_lease_input(spec).unwrap());
        let closed = Arc::new(Mutex::new(Vec::new()));
        let executor = LeaseExecutor::new(
            Arc::new(RunningBackend {
                closed: closed.clone(),
            }),
            LeaseControl::new(),
        );
        let (tx, _rx) = tokio::sync::mpsc::channel(2);
        let started = std::time::Instant::now();
        let result = executor
            .execute_streaming(&job, Duration::from_secs(60), tx)
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(
            result.wall_ms < 2000,
            "must not bill the queued 59.7 seconds as extra runtime"
        );
        assert_eq!(closed.lock().as_slice(), &[job.job_id]);
    }

    #[tokio::test]
    async fn a_lease_run_as_a_non_streaming_job_is_refused() {
        // A lease's access grant only reaches the buyer on the live stream.
        // Run without one it would bill the buyer for a machine they could
        // never learn the address of, so a non-streaming lease is refused.
        let backend = Arc::new(RunningBackend {
            closed: Arc::new(Mutex::new(Vec::new())),
        });
        let executor = LeaseExecutor::new(backend, LeaseControl::new());
        let err = executor
            .execute(&lease_job(), Duration::from_secs(1))
            .await
            .expect_err("a lease cannot be served without a stream");
        assert!(
            matches!(&err, ExecutorError::Failed(msg) if msg.contains("streaming job")),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn a_lease_executor_refuses_a_job_of_another_kind() {
        // The executor serves lease sessions and nothing else; a job of
        // another kind that reached it is a routing mistake, refused rather
        // than run as if it were a lease.
        let backend = Arc::new(RunningBackend {
            closed: Arc::new(Mutex::new(Vec::new())),
        });
        let executor = LeaseExecutor::new(backend, LeaseControl::new());
        let mut job = lease_job();
        job.kind = JobKind::InferenceCall;
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let err = executor
            .execute_streaming(&job, Duration::from_secs(1), tx)
            .await
            .expect_err("a non-lease job is not the lease executor's to serve");
        assert!(
            matches!(&err, ExecutorError::Failed(msg) if msg.contains("cannot serve")),
            "got: {err}"
        );
    }
    struct PendingAdmission {
        stage_is_evidence: bool,
        entered: Arc<Notify>,
        closed: Arc<Mutex<Vec<Uuid>>>,
    }
    #[async_trait]
    impl SessionBackend for PendingAdmission {
        async fn open(&self, job: &JobEnvelopePayload) -> Result<LeaseAccess, ExecutorError> {
            if !self.stage_is_evidence {
                self.entered.notify_one();
                return std::future::pending().await;
            }
            Ok(LeaseAccess {
                ssh: None,
                source_price: None,
                job_id: job.job_id,
                endpoint: "ssh://covenant@example.com:22".into(),
                ready_at_ms: 0,
                note: None,
            })
        }
        async fn after_access(
            &self,
            _: &JobEnvelopePayload,
            _: &LeaseAccess,
        ) -> Result<(), ExecutorError> {
            self.entered.notify_one();
            std::future::pending().await
        }
        async fn close(&self, job_id: Uuid) {
            self.closed.lock().push(job_id);
        }
    }
    async fn assert_close_cancels_admission(evidence: bool) {
        let entered = Arc::new(Notify::new());
        let closed = Arc::new(Mutex::new(Vec::new()));
        let backend = Arc::new(PendingAdmission {
            stage_is_evidence: evidence,
            entered: entered.clone(),
            closed: closed.clone(),
        });
        let control = LeaseControl::new();
        let executor = LeaseExecutor::new(backend, control.clone());
        let job = lease_job();
        let job_id = job.job_id;
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let running = tokio::spawn(async move {
            executor
                .execute_streaming(&job, Duration::from_secs(60), tx)
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .unwrap();
        control.close(job_id);
        let error = tokio::time::timeout(Duration::from_secs(1), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("closed or expired"));
        assert_eq!(*closed.lock(), vec![job_id]);
        assert!(!control.is_closed(job_id));
    }
    #[tokio::test]
    async fn buyer_close_cancels_pending_open_and_releases_owned_resource() {
        assert_close_cancels_admission(false).await;
    }
    #[tokio::test]
    async fn buyer_close_cancels_pending_evidence_and_releases_owned_resource() {
        assert_close_cancels_admission(true).await;
    }
}
