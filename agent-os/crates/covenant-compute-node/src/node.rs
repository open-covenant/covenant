//! The operator node's per-job orchestration: poll the coordinator,
//! admit, execute, meter, sign a receipt, submit it, credit earnings,
//! and record both steps into the node's own hash-chained audit log
//! (design-01 §1's lifecycle, condensed to what Phase 1 needs).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use covenant_a2a::A2ATaskStatus;
use covenant_audit::{AuditEvent, AuditKind, AuditLog};
use covenant_compute_protocol::{
    output_hash_hex, CapabilityProfile, EscrowHoldAttestation, HeartbeatRequest, JobAccept,
    JobMeter, JobResultMessage, OperatorStatus, ProtocolError, ResultSettlement, SignedJobEnvelope,
    SignedWorkReceipt, StreamChunk, StreamPush, WorkReceiptPayload,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use covenant_types::AgentId;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::accepted::{AcceptedBook, AcceptedEntry};
use crate::admission::{admit_job, AdmissionContext, AdmissionError};
use crate::coordinator::{Coordinator, CoordinatorError};
use crate::earnings::{EarningsEntry, EarningsError, EarningsLedger, EarningsStatus};
use crate::executor::{ExecutorError, JobExecutor};
use crate::outbox::{OutboxEntry, ResultOutbox};

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("coordinator: {0}")]
    Coordinator(#[from] CoordinatorError),
    #[error("admission rejected: {0}")]
    Admission(#[from] AdmissionError),
    #[error("protocol: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("audit: {0}")]
    Audit(String),
    #[error("earnings: {0}")]
    Earnings(#[from] EarningsError),
    #[error("outbox: {0}")]
    Outbox(#[from] crate::outbox::OutboxError),
}

pub struct NodeConfig {
    /// The operator's pinned, out-of-band-known coordinator pubkey.
    pub coordinator_pubkey_b58: String,
    pub max_in_flight: usize,
    /// Grace window `preempt_subprocess_pg` waits between SIGTERM and
    /// SIGKILL when a job outlives its deadline.
    pub preempt_grace: Duration,
    /// The marketplace fee the coordinator disclosed at registration.
    /// Earnings credit net of it (protocol-shared floor math), so the
    /// operator's books match what the coordinator actually pays. If
    /// the coordinator lowers its fee mid-session the estimate here is
    /// conservative; a raise is refused at re-register (main.rs).
    pub fee_bps: u32,
}

/// One admitted-and-executed job's outcome, returned to the caller of
/// [`Node::run_once`] for logging/inspection.
pub struct JobOutcome {
    pub job_id: Uuid,
    pub receipt: SignedWorkReceipt,
    pub error_message: Option<String>,
}

/// A failed job's error carried as its output, so a buyer learns *why* it
/// failed instead of a bare `execution_failed`. The output is hashed into
/// the receipt, so this text is the operator's signed statement of the
/// cause — the buyer re-verifies it against the receipt before trusting
/// it. Bounded because the receipt hashes it and a buyer renders it; a
/// verbose backend error must not bloat the wire frame.
fn failure_output(detail: &str) -> Vec<Content> {
    const MAX_CHARS: usize = 1024;
    let trimmed = detail.trim();
    let text: String = if trimmed.is_empty() {
        "the operator's executor failed without a reported cause".into()
    } else {
        trimmed.chars().take(MAX_CHARS).collect()
    };
    vec![Content::text(text)]
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn hash_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

/// How the chunk forwarder batches: whichever comes first of this much
/// buffered text or one flush interval. Small enough that a buyer sees
/// tokens at conversational latency, large enough that a fast model
/// doesn't turn every token into an HTTP round-trip.
const CHUNK_FLUSH_BYTES: usize = 8 * 1024;
const CHUNK_FLUSH_INTERVAL: Duration = Duration::from_millis(150);

/// Relays a streaming job's deltas to the coordinator in seq-numbered
/// batches until the executor drops the sink, then flushes the tail
/// with `done`. A failed push disables the relay for the rest of the
/// job — the receiver keeps draining so the executor never blocks on a
/// dead relay — because chunks are a preview and the job's money path
/// must not inherit their failures.
fn spawn_chunk_forwarder<C: Coordinator + 'static>(
    coordinator: Arc<C>,
    job_id: Uuid,
    mut rx: tokio::sync::mpsc::Receiver<String>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut next_seq = 0u64;
        let mut batch: Vec<StreamChunk> = Vec::new();
        let mut batch_bytes = 0usize;
        let mut ticker = tokio::time::interval(CHUNK_FLUSH_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        let flush = |batch: &mut Vec<StreamChunk>, batch_bytes: &mut usize, done: bool| {
            let push = StreamPush {
                job_id,
                chunks: std::mem::take(batch),
                done,
            };
            *batch_bytes = 0;
            push
        };

        loop {
            tokio::select! {
                maybe = rx.recv() => match maybe {
                    Some(text) => {
                        batch_bytes += text.len();
                        batch.push(StreamChunk { seq: next_seq, text });
                        next_seq += 1;
                        if batch_bytes < CHUNK_FLUSH_BYTES {
                            continue;
                        }
                    }
                    None => break,
                },
                _ = ticker.tick() => {
                    if batch.is_empty() {
                        continue;
                    }
                }
            }
            let push = flush(&mut batch, &mut batch_bytes, false);
            if let Err(e) = coordinator.push_stream(push).await {
                tracing::warn!(%job_id, error = %e, "chunk relay failed; streaming disabled for this job");
                while rx.recv().await.is_some() {}
                return;
            }
        }

        let push = flush(&mut batch, &mut batch_bytes, true);
        if let Err(e) = coordinator.push_stream(push).await {
            tracing::warn!(%job_id, error = %e, "final chunk flush failed; the receipt still concludes the job");
        }
    })
}

pub struct Node<C, X, L> {
    pub identity: LocalIdentity,
    pub profile: CapabilityProfile,
    pub coordinator: Arc<C>,
    pub executor: Arc<X>,
    pub earnings: Arc<L>,
    pub audit: Arc<dyn AuditLog>,
    pub config: NodeConfig,
    outbox: Arc<ResultOutbox>,
    accepted: Arc<AcceptedBook>,
    in_flight: AtomicUsize,
    backend_up: AtomicBool,
    draining: AtomicBool,
    drain_notify: tokio::sync::Notify,
}

/// How many process lives may attempt one job's execution before boot
/// recovery gives up on it — the crash-loop guard for a job whose
/// execution is what kills the node. Two means the accepting life plus
/// one recovery; the deadline sweep owns the refund either way.
const MAX_EXECUTION_LIVES: u32 = 2;

impl<C, X, L> Node<C, X, L>
where
    // 'static because the chunk forwarder outlives run_once's borrow:
    // it runs as a spawned task holding its own Arc<C>.
    C: Coordinator + 'static,
    X: JobExecutor,
    L: EarningsLedger,
{
    pub fn new(
        identity: LocalIdentity,
        profile: CapabilityProfile,
        coordinator: Arc<C>,
        executor: Arc<X>,
        earnings: Arc<L>,
        audit: Arc<dyn AuditLog>,
        config: NodeConfig,
    ) -> Self {
        Self {
            identity,
            profile,
            coordinator,
            executor,
            earnings,
            audit,
            config,
            outbox: Arc::new(ResultOutbox::in_memory()),
            accepted: Arc::new(AcceptedBook::in_memory()),
            in_flight: AtomicUsize::new(0),
            backend_up: AtomicBool::new(true),
            draining: AtomicBool::new(false),
            drain_notify: tokio::sync::Notify::new(),
        }
    }

    /// Swaps in a durable outbox — the real binary's form, so queued
    /// results survive a node restart too. The default in-memory queue
    /// still redelivers within one process life.
    pub fn with_outbox(mut self, outbox: Arc<ResultOutbox>) -> Self {
        self.outbox = outbox;
        self
    }

    /// Swaps in a durable accepted-jobs book — the real binary's form,
    /// so a job the process died holding is re-served at boot by
    /// [`Node::recover_accepted`]. The default in-memory book forgets
    /// with the process, which is exactly the pre-book behavior.
    pub fn with_accepted_book(mut self, accepted: Arc<AcceptedBook>) -> Self {
        self.accepted = accepted;
        self
    }

    async fn record_admission(
        &self,
        operator: &AgentId,
        job_id: Uuid,
        passed: bool,
        reason: &str,
    ) -> Result<(), NodeError> {
        self.audit
            .record(AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: operator.clone(),
                kind: AuditKind::ComputeJobAdmitted {
                    job_id,
                    operator_pubkey_b58: operator.pubkey_base58(),
                    passed,
                    reason: reason.to_string(),
                },
            })
            .await
            .map_err(|e| NodeError::Audit(e.to_string()))
    }

    async fn record_completion(
        &self,
        operator: &AgentId,
        job_id: Uuid,
        receipt: &WorkReceiptPayload,
    ) -> Result<(), NodeError> {
        let status = match receipt.status {
            A2ATaskStatus::Ok => "ok",
            A2ATaskStatus::Error => "error",
            A2ATaskStatus::Partial => "partial",
        };
        self.audit
            .record(AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: operator.clone(),
                kind: AuditKind::ComputeJobCompleted {
                    job_id,
                    operator_pubkey_b58: receipt.operator.pubkey_base58(),
                    status: status.into(),
                    result_hash_hex: receipt.result_hash_hex.clone(),
                    price_micro_usdc: receipt.price_micro_usdc,
                },
            })
            .await
            .map_err(|e| NodeError::Audit(e.to_string()))
    }

    /// Jobs currently executing — what the capacity admission check
    /// reads, exposed so a heartbeat can report queue depth honestly.
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }

    /// Whether the executor's backend answered its last health probe
    /// ([`Node::wait_for_backend`] keeps this current).
    pub fn backend_up(&self) -> bool {
        self.backend_up.load(Ordering::SeqCst)
    }

    /// Whether [`Node::begin_drain`] has been called: the node is
    /// finishing what it holds and taking nothing new.
    pub fn draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    /// Puts the node into drain: no new work is taken (`run_once`
    /// answers `None` without polling) and the status turns `Offline`,
    /// announced with an immediate transition beat so the coordinator
    /// re-matches whatever still sits in this node's queue while
    /// in-flight work runs to completion here. Idempotent — the beat
    /// rides the first call only. There is no way back short of a
    /// restart: drain exists to end the process, not to pause it.
    pub async fn begin_drain(&self) {
        if !self.draining.swap(true, Ordering::SeqCst) {
            tracing::info!("draining: finishing in-flight work, taking nothing new");
            // notify_one stores a permit, so an idle long-poll wakes
            // even if it registers a moment after this — the drain
            // must not wait out a poll for nothing.
            self.drain_notify.notify_one();
            self.report_status().await;
        }
    }

    /// The status a heartbeat should declare right now. A dead backend
    /// or a drain wins over everything — `Offline` is the one status
    /// the matcher never routes to, and a node that cannot (or will
    /// not) serve must not be matched; capacity comes second, and only
    /// then `Online`.
    pub fn current_status(&self) -> OperatorStatus {
        if !self.backend_up() || self.draining() {
            OperatorStatus::Offline
        } else if self.in_flight() >= self.config.max_in_flight {
            OperatorStatus::Busy
        } else {
            OperatorStatus::Online
        }
    }

    /// Best-effort out-of-cycle heartbeat, sent on a backend-health
    /// transition so the matcher hears now, not up to a heartbeat
    /// interval late. A racing periodic beat may land stale around the
    /// flip; the next interval repeats the truth either way.
    async fn report_status(&self) {
        let req = match HeartbeatRequest::sign(
            self.identity.agent_id(),
            self.current_status(),
            self.in_flight() as u32,
            epoch_ms(),
            &self.identity,
        ) {
            Ok(req) => req,
            Err(e) => {
                tracing::error!(error = %e, "transition heartbeat signing failed");
                return;
            }
        };
        if let Err(e) = self.coordinator.heartbeat(req).await {
            tracing::warn!(error = %e, "transition heartbeat failed; the periodic beat repeats it");
        }
    }

    /// Blocks until the executor's backend answers its health probe —
    /// the serve loop's gate against taking work it cannot serve.
    /// While the backend is down the node reports itself offline (the
    /// matcher routes new jobs to live operators instead of into
    /// guaranteed faults), keeps draining the outbox (a finished
    /// result owes the backend nothing), and re-probes every `retry`.
    /// Offers already queued when the outage began stay unpolled; the
    /// coordinator's re-offer sweep re-routes them without faulting
    /// anyone. A drain releases the gate immediately: a node that is
    /// leaving has nothing to wait for.
    pub async fn wait_for_backend(&self, retry: Duration) {
        loop {
            if self.draining() {
                return;
            }
            match self.executor.health().await {
                Ok(()) => {
                    if !self.backend_up.swap(true, Ordering::SeqCst) {
                        tracing::info!("backend recovered; resuming job intake");
                        self.report_status().await;
                    }
                    return;
                }
                Err(e) => {
                    if self.backend_up.swap(false, Ordering::SeqCst) {
                        tracing::warn!(
                            error = %e,
                            "backend unhealthy; job intake paused and the node reports \
                             offline until it recovers"
                        );
                        self.report_status().await;
                    } else {
                        tracing::debug!(error = %e, "backend still unhealthy");
                    }
                    self.drain_outbox().await;
                    tokio::select! {
                        _ = tokio::time::sleep(retry) => {}
                        _ = self.drain_notify.notified() => {}
                    }
                }
            }
        }
    }

    /// Polls the coordinator once. Returns `Ok(None)` when no job was
    /// offered. Runs the full admit -> execute -> sign -> submit ->
    /// credit loop for exactly one job otherwise. A draining node
    /// answers `None` without polling: taking nothing new is the
    /// library's guarantee, not the serve loop's courtesy.
    pub async fn run_once(&self) -> Result<Option<JobOutcome>, NodeError> {
        if self.draining() {
            return Ok(None);
        }
        let operator = self.identity.agent_id();
        // A drain interrupts the long-poll — an idle node must not make
        // its operator wait out a poll window to exit — but never the
        // execution below it. If the coordinator popped an offer into
        // the response this cancel abandons, its queue no longer holds
        // the job and the stale sweep's same-winner/fresh re-match
        // heals it; nothing is owed by a node that never accepted.
        let polled = tokio::select! {
            polled = self.coordinator.poll_next_job(&operator) => polled?,
            _ = self.drain_notify.notified() => return Ok(None),
        };
        let Some(offer) = polled else {
            return Ok(None);
        };

        let job_id = offer.envelope.payload.job_id;
        let admit_result = self.admit(&offer.envelope, &offer.escrow_hold, &operator);
        if let Err(e) = admit_result {
            self.record_admission(&operator, job_id, false, &e.to_string())
                .await?;
            let _ = self
                .coordinator
                .accept_job(JobAccept::Reject {
                    job_id,
                    reason: e.to_string(),
                })
                .await;
            return Err(NodeError::Admission(e));
        }
        self.record_admission(&operator, job_id, true, "ok").await?;

        // Remember the job durably BEFORE telling the coordinator
        // "accepted". The accept ack is what binds the coordinator to
        // hold this operator to the job until its deadline, so a crash
        // in the gap between that ack and a later durable write used to
        // forget a job the coordinator still expected — a guaranteed
        // deadline fault and lost pay, the one outcome this book exists
        // to prevent. Booking first inverts the risk: a crash before the
        // ack lands re-serves a job the coordinator may never have
        // registered, which it then pays (the assigned operator's own
        // still-`Offered` job) or refuses cleanly — never a fault. The
        // book is the safety net, not the job: one that cannot persist
        // is loud but not fatal, since the node can still deliver.
        if let Err(e) = self.accepted.book(AcceptedEntry {
            job_id,
            envelope: offer.envelope.clone(),
            escrow_hold: offer.escrow_hold.clone(),
            accepted_at_ms: epoch_ms(),
            lives: 1,
            settled: false,
        }) {
            tracing::warn!(
                %job_id,
                error = %e,
                "accepted job not booked; a crash before the result lands would forget it"
            );
        }

        if let Err(e) = self
            .coordinator
            .accept_job(JobAccept::Accept { job_id })
            .await
        {
            // A landed rejection is the coordinator's final word — the
            // job was reassigned or cancelled in the offer→accept gap —
            // so drop the book: recovery must not re-serve a job already
            // gone. A transport failure is uncertain (the ack may have
            // landed), so leave it booked and let recovery re-serve it,
            // the same split `execute_accepted` makes on the result.
            if !matches!(e, CoordinatorError::Transport(_)) {
                self.settle_accepted(job_id);
            }
            return Err(e.into());
        }

        self.execute_accepted(&offer.envelope, &offer.escrow_hold)
            .await
            .map(Some)
    }

    /// The post-accept half of the lifecycle: execute, meter, sign a
    /// receipt, submit, credit, and settle the accepted-jobs book —
    /// shared by [`Node::run_once`] and [`Node::recover_accepted`], so
    /// a re-served job walks exactly the path a first-try one does.
    async fn execute_accepted(
        &self,
        envelope: &SignedJobEnvelope,
        escrow_hold: &EscrowHoldAttestation,
    ) -> Result<JobOutcome, NodeError> {
        let operator = self.identity.agent_id();
        let job_id = envelope.payload.job_id;

        self.in_flight.fetch_add(1, Ordering::SeqCst);
        // The buyer's deadline is absolute — `issued_at_ms + deadline_ms`,
        // the same instant the coordinator's sweep and release check
        // enforce — so the executor's budget is the time still left until
        // it, not a fresh `deadline_ms` measured from now. A job that
        // spent time in the queue gets only the remainder; without this a
        // slow-dequeued job would run well past the point the coordinator
        // will pay for, burning the operator's compute for nothing.
        let remaining = envelope
            .payload
            .issued_at_ms
            .saturating_add(envelope.payload.deadline_ms)
            .saturating_sub(epoch_ms());
        let exec_result = if remaining == 0 {
            // Already past its deadline before execution could begin: the
            // coordinator refunds it regardless, so don't spend the
            // operator's compute on output that can never be paid.
            Err(ExecutorError::Timeout(Duration::ZERO))
        } else {
            let deadline = Duration::from_millis(remaining);
            if envelope.payload.stream {
                // The forwarder owns the sink's receiving end; when the
                // executor returns, the channel closes and the forwarder
                // flushes its tail with `done`. Awaiting it (briefly) puts
                // that final push ahead of the receipt in the common case,
                // and the coordinator drops late chunks either way.
                let (tx, rx) = tokio::sync::mpsc::channel::<String>(256);
                let forwarder = spawn_chunk_forwarder(self.coordinator.clone(), job_id, rx);
                let result = self
                    .executor
                    .execute_streaming(&envelope.payload, deadline, tx)
                    .await;
                if tokio::time::timeout(Duration::from_secs(2), forwarder)
                    .await
                    .is_err()
                {
                    tracing::warn!(%job_id, "chunk forwarder still flushing; submitting the result without it");
                }
                result
            } else {
                self.executor.execute(&envelope.payload, deadline).await
            }
        };
        self.in_flight.fetch_sub(1, Ordering::SeqCst);

        let (status, output, error_message, wall_ms, tokens_in, tokens_out, finish_reason) =
            match &exec_result {
                Ok(outcome) => (
                    A2ATaskStatus::Ok,
                    outcome.output.clone(),
                    None,
                    outcome.wall_ms,
                    outcome.tokens_in,
                    outcome.tokens_out,
                    outcome.finish_reason,
                ),
                Err(e) => {
                    // The buyer-facing output carries the bare cause; the
                    // `execution failed:` framing `ExecutorError::Failed`'s
                    // Display adds is redundant once it reads "not served:
                    // failed: …", though the node's own `error_message` keeps
                    // the full rendering for its logs.
                    let cause = match e {
                        ExecutorError::Failed(msg) => msg.clone(),
                        other => other.to_string(),
                    };
                    (
                        A2ATaskStatus::Error,
                        failure_output(&cause),
                        Some(e.to_string()),
                        0,
                        None,
                        None,
                        None,
                    )
                }
            };

        let job_hash_hex = hash_hex(envelope.payload_json.as_bytes());
        let result_hash_hex = output_hash_hex(&output);
        let audit_root_hex = self
            .audit
            .verify_integrity()
            .await
            .map_err(|e| NodeError::Audit(e.to_string()))?
            .root_hash_hex;

        let receipt_payload = WorkReceiptPayload {
            job_id,
            operator: operator.clone(),
            job_hash_hex,
            result_hash_hex,
            meter: JobMeter {
                wall_ms,
                tokens_in,
                tokens_out,
                gpu_seconds: None,
                finish_reason,
            },
            price_micro_usdc: envelope.payload.price_micro_usdc,
            status,
            executed_at_ms: epoch_ms(),
            node_audit_root_hex: audit_root_hex,
        };
        let signed_receipt = SignedWorkReceipt::sign(receipt_payload, &self.identity)?;

        self.record_completion(&operator, job_id, &signed_receipt.receipt)
            .await?;

        let message = JobResultMessage {
            receipt: signed_receipt.clone(),
            output,
        };
        match self.coordinator.submit_result(message.clone()).await {
            // Earnings follow the coordinator's settlement verdict, not
            // the delivery: a result that landed after the deadline acks
            // 200 but settles as a refund, and booking it would show the
            // operator owed money no payout will ever push.
            Ok(ack) => {
                match (&signed_receipt.receipt.status, ack.settled) {
                    (A2ATaskStatus::Ok, ResultSettlement::Released) => {
                        // `AlreadyCredited` is benign, exactly as in the
                        // outbox drain: a prior life booked this job's
                        // earnings but crashed before tombstoning it, so the
                        // credit is already durable — fall through to the
                        // tombstone rather than re-serving an already-paid
                        // job until the crash-loop guard drops it. A persist
                        // failure still propagates, leaving the book un-
                        // tombstoned so the next life retries the credit.
                        match self
                            .credit_released(
                                &signed_receipt,
                                ack.released_gross_micro_usdc,
                                escrow_hold.funding_source,
                            )
                            .await
                        {
                            Ok(()) | Err(EarningsError::AlreadyCredited(_)) => {}
                            Err(e) => return Err(e.into()),
                        }
                    }
                    (A2ATaskStatus::Ok, ResultSettlement::Refunded) => {
                        tracing::warn!(
                            %job_id,
                            "result delivered but the job settled as a refund (deadline passed \
                             first); no earnings booked"
                        );
                    }
                    (A2ATaskStatus::Ok, ResultSettlement::AwaitingCheck) => {
                        tracing::info!(
                            %job_id,
                            "result delivered; payment waits on another operator's check"
                        );
                    }
                    _ => {}
                }
                // Credit before tombstone: a crash between the two
                // re-serves the job into a refusal, never a lost book.
                self.settle_accepted(job_id);
            }
            // The coordinator never answered — a restart, a deploy —
            // and the transport layer's own retries are already spent.
            // The work is done and the receipt is signed truth: queue
            // the exact message durably and let the serve loop's drain
            // redeliver it. An entry the outbox cannot persist keeps
            // the failure loud instead of dropping the receipt — and
            // keeps the accepted book holding the job, so even that
            // crash re-serves it.
            Err(CoordinatorError::Transport(e)) => {
                self.outbox.enqueue(OutboxEntry {
                    job_id,
                    message,
                    funding_source: escrow_hold.funding_source,
                    queued_at_ms: epoch_ms(),
                    settled: false,
                })?;
                self.settle_accepted(job_id);
                tracing::warn!(
                    %job_id,
                    error = %e,
                    "result undeliverable; queued for redelivery"
                );
            }
            // Any landed answer is the coordinator's final word on this
            // job; there is nothing left for a restart to re-serve.
            Err(e) => {
                self.settle_accepted(job_id);
                return Err(e.into());
            }
        }

        Ok(JobOutcome {
            job_id,
            receipt: signed_receipt,
            error_message,
        })
    }

    fn settle_accepted(&self, job_id: Uuid) {
        if let Err(e) = self.accepted.settle(job_id) {
            tracing::error!(%job_id, error = %e, "accepted book settle failed");
        }
    }

    /// Books an Unpaid earnings entry for a receipt the coordinator
    /// settled as Released — the one shared credit path, whether the
    /// delivery landed first try or through the outbox. The credit is
    /// sized from the coordinator's disclosed released gross, not the
    /// receipt's envelope price: a lease escrows its window's ceiling but
    /// the coordinator releases only the metered seconds, so booking the
    /// price would over-report the balance and later trip the operator's
    /// own on-chain payout audit against the smaller metered transfer. A
    /// non-lease job releases its whole price, so its credit is unchanged.
    async fn credit_released(
        &self,
        receipt: &SignedWorkReceipt,
        released_gross_micro_usdc: u64,
        funding_source: covenant_compute_protocol::FundingSource,
    ) -> Result<(), EarningsError> {
        let fee = covenant_compute_protocol::fee_take_micro_usdc(
            released_gross_micro_usdc,
            self.config.fee_bps,
        );
        // Registration refuses a fee_bps at or above 100%, so fee <= gross
        // here; saturate anyway rather than risk an underflow panic or a
        // wrapped garbage amount fsynced into the durable ledger.
        self.earnings
            .credit(EarningsEntry {
                job_id: receipt.receipt.job_id,
                amount_micro_usdc: released_gross_micro_usdc.saturating_sub(fee),
                fee_micro_usdc: fee,
                funding_source,
                status: EarningsStatus::Unpaid,
                earned_at_ms: epoch_ms(),
                paid_tx_signature: None,
                paid_at_ms: None,
                receipt_signature_b58: Some(receipt.signature_b58.clone()),
            })
            .await
    }

    /// Re-pushes every queued result until the coordinator answers.
    /// A transport failure ends the pass — the coordinator is still
    /// gone, and the queue holds. A landed answer settles its entry once
    /// its credit is booked: a released verdict credits earnings exactly
    /// as a first-try delivery would (crediting before the tombstone, so
    /// a crash between the two re-delivers into the `AlreadyCredited`
    /// guard instead of losing the books, and a credit that fails to
    /// persist holds the entry for the next drain), a refunded one books
    /// nothing, and a refusal is the coordinator's final word — re-sending
    /// it is noise, not recovery. Returns how many entries settled.
    pub async fn drain_outbox(&self) -> usize {
        let mut settled = 0;
        for entry in self.outbox.pending() {
            let job_id = entry.job_id;
            match self.coordinator.submit_result(entry.message.clone()).await {
                Ok(ack) => {
                    // A Released verdict must be booked before the entry
                    // leaves the queue. If the credit fails to persist — a
                    // transient disk error, not `AlreadyCredited` — hold the
                    // entry so the next drain retries it (the coordinator
                    // answers the replay with the same verdict idempotently)
                    // rather than settling away the only durable record of
                    // the owed credit.
                    let keep_for_retry = match (&entry.message.receipt.receipt.status, ack.settled)
                    {
                        (A2ATaskStatus::Ok, ResultSettlement::Released) => {
                            match self
                                .credit_released(
                                    &entry.message.receipt,
                                    ack.released_gross_micro_usdc,
                                    entry.funding_source,
                                )
                                .await
                            {
                                Ok(()) => {
                                    tracing::info!(%job_id, "queued result delivered and credited");
                                    false
                                }
                                Err(EarningsError::AlreadyCredited(_)) => false,
                                Err(e) => {
                                    tracing::error!(
                                        %job_id,
                                        error = %e,
                                        "credit after redelivery failed; holding the entry for retry"
                                    );
                                    true
                                }
                            }
                        }
                        (A2ATaskStatus::Ok, ResultSettlement::Refunded) => {
                            tracing::warn!(
                                %job_id,
                                "queued result outlived its deadline; refunded, no earnings"
                            );
                            false
                        }
                        _ => false,
                    };
                    if keep_for_retry {
                        continue;
                    }
                    if let Err(e) = self.outbox.settle(job_id) {
                        tracing::error!(%job_id, error = %e, "outbox settle failed after redelivery");
                    }
                    settled += 1;
                }
                Err(CoordinatorError::Transport(e)) => {
                    tracing::debug!(error = %e, "coordinator still unreachable; outbox holds");
                    break;
                }
                Err(e) => {
                    tracing::warn!(
                        %job_id,
                        error = %e,
                        "queued result refused; dropped — the coordinator already answered it"
                    );
                    if let Err(e) = self.outbox.settle(job_id) {
                        tracing::error!(%job_id, error = %e, "outbox settle failed after refusal");
                    }
                    settled += 1;
                }
            }
        }
        settled
    }

    /// Re-serves jobs a previous process life accepted and never
    /// finished — the node-side twin of the coordinator's boot
    /// reconciliation. Boot-only, before the serve loop starts: run
    /// alongside live polling it would execute in-flight jobs a second
    /// time.
    ///
    /// Per booked job: one whose result already sits in the outbox is
    /// done (the crash fell between the outbox write and the book's
    /// tombstone — the drain owns delivery); one past its absolute
    /// deadline is dropped without burning compute (the coordinator's
    /// sweep refunds it, and the operator was genuinely dark);
    /// one that has already used [`MAX_EXECUTION_LIVES`] is dropped as
    /// poison. Everything else runs the normal execute-and-submit
    /// path with the time still left until the buyer's deadline — a
    /// streaming job re-streams, and any preview chunks the previous
    /// life already pushed drop server-side as duplicate seqs; the
    /// verified result is unaffected. Returns how many jobs re-served
    /// to a settlement.
    pub async fn recover_accepted(&self) -> usize {
        let mut served = 0;
        for entry in self.accepted.pending() {
            if self.draining() {
                tracing::info!(
                    "drain during recovery; remaining booked jobs wait for the next boot"
                );
                break;
            }
            let job_id = entry.job_id;
            if self.outbox.pending().iter().any(|e| e.job_id == job_id) {
                tracing::info!(
                    %job_id,
                    "interrupted job's result is already queued for redelivery; nothing to re-run"
                );
                self.settle_accepted(job_id);
                continue;
            }
            // The boot drain runs before recovery and dequeues every
            // result it delivers, so a job it just credited no longer
            // matches the outbox check above — but its credit is durable.
            // Re-executing it only burns compute for a result the
            // coordinator refuses as a duplicate; the credit already
            // survives a crash between it and the book's tombstone.
            if self.earnings.is_credited(job_id).await {
                tracing::info!(
                    %job_id,
                    "interrupted job was already delivered and credited; nothing to re-run"
                );
                self.settle_accepted(job_id);
                continue;
            }
            let deadline_ms = entry
                .envelope
                .payload
                .issued_at_ms
                .saturating_add(entry.envelope.payload.deadline_ms);
            if epoch_ms() >= deadline_ms {
                tracing::warn!(
                    %job_id,
                    "interrupted job's deadline passed while the node was down; \
                     the coordinator refunds it"
                );
                self.settle_accepted(job_id);
                continue;
            }
            if entry.lives >= MAX_EXECUTION_LIVES {
                tracing::error!(
                    %job_id,
                    lives = entry.lives,
                    "interrupted job has killed this node before; giving up on it"
                );
                self.settle_accepted(job_id);
                continue;
            }
            // The life is on the books before the execution that might
            // not survive it — otherwise a job that kills the node
            // would crash-loop it at every boot.
            if let Err(e) = self.accepted.record_life(job_id) {
                tracing::error!(
                    %job_id,
                    error = %e,
                    "cannot record the recovery attempt; skipping re-execution"
                );
                continue;
            }
            match self
                .execute_accepted(&entry.envelope, &entry.escrow_hold)
                .await
            {
                Ok(outcome) => {
                    served += 1;
                    tracing::info!(
                        %job_id,
                        status = ?outcome.receipt.receipt.status,
                        "interrupted job re-served"
                    );
                }
                // Transport failures never surface here (the outbox
                // absorbs them), so a coordinator error is an answered
                // refusal — it already settled the book.
                Err(NodeError::Coordinator(e)) => tracing::warn!(
                    %job_id,
                    error = %e,
                    "interrupted job's result refused; the coordinator had already answered it"
                ),
                Err(e) => tracing::warn!(
                    %job_id,
                    error = %e,
                    "interrupted job recovery failed; it stays booked for the next boot"
                ),
            }
        }
        served
    }

    fn admit(
        &self,
        envelope: &SignedJobEnvelope,
        escrow_hold: &EscrowHoldAttestation,
        _operator: &AgentId,
    ) -> Result<(), AdmissionError> {
        let ctx = AdmissionContext {
            local_profile: &self.profile,
            coordinator_pubkey_b58: &self.config.coordinator_pubkey_b58,
            max_in_flight: self.config.max_in_flight,
            in_flight: self.in_flight.load(Ordering::SeqCst),
            now_ms: epoch_ms(),
        };
        admit_job(envelope, escrow_hold, &ctx)
    }
}
