//! Outbound long-poll client contract against the coordinator
//! (design-02 §5: the operator never accepts an inbound connection —
//! it dials out and hangs waiting for the next job). This is the client
//! side of that contract: the trait the real HTTP client
//! (`HttpCoordinatorClient` in `http_client`) implements, plus an
//! in-memory mock for tests.

use async_trait::async_trait;
use covenant_compute_protocol::{
    HeartbeatRequest, HeartbeatResponse, JobAccept, JobOffer, JobResultAck, JobResultMessage,
    RegisterRequest, RegisterResponse, ResultSettlement, StreamPush,
};
use covenant_types::AgentId;
use parking_lot::Mutex;
use std::collections::VecDeque;

#[derive(Debug, thiserror::Error)]
pub enum CoordinatorError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("protocol: {0}")]
    Protocol(String),
}

/// The client side of the node-to-coordinator wire contract. The real
/// implementation (`HttpCoordinatorClient`) dials out over HTTP
/// long-poll to the coordinator service; see
/// `build-notes-phase1-foundation.md` for the exact endpoints this trait
/// stands in for.
#[async_trait]
pub trait Coordinator: Send + Sync {
    async fn register(&self, req: RegisterRequest) -> Result<RegisterResponse, CoordinatorError>;
    async fn heartbeat(&self, req: HeartbeatRequest)
        -> Result<HeartbeatResponse, CoordinatorError>;
    /// Long-polls for the next dispatched job. `Ok(None)` means no job
    /// was available before the poll's own timeout — not an error.
    async fn poll_next_job(&self, operator: &AgentId)
        -> Result<Option<JobOffer>, CoordinatorError>;
    async fn accept_job(&self, decision: JobAccept) -> Result<(), CoordinatorError>;
    /// Delivers the signed receipt and output. The ack carries the
    /// settlement verdict — `Released` pays, `Refunded` does not — and
    /// only that verdict may drive the operator's earnings books: a
    /// clean 200 can still be a deadline refund.
    async fn submit_result(
        &self,
        result: JobResultMessage,
    ) -> Result<JobResultAck, CoordinatorError>;
    /// Relays a streaming job's chunk batch. Best-effort by protocol
    /// design: callers treat an error as "stop pushing for this job",
    /// never as a reason to fail the job itself.
    async fn push_stream(&self, push: StreamPush) -> Result<(), CoordinatorError>;
}

/// In-memory [`Coordinator`] for tests: `poll_next_job` hands out
/// pre-seeded offers FIFO instead of talking to a real service. Also
/// verifies signed messages exactly as a real coordinator would, so a
/// test that signs a malformed register/heartbeat message fails the
/// same way it would against the real thing.
#[derive(Default)]
pub struct MockCoordinator {
    offers: Mutex<VecDeque<JobOffer>>,
    registrations: Mutex<Vec<RegisterRequest>>,
    heartbeats: Mutex<Vec<HeartbeatRequest>>,
    accepts: Mutex<Vec<JobAccept>>,
    results: Mutex<Vec<JobResultMessage>>,
    stream_pushes: Mutex<Vec<StreamPush>>,
    submit_failures: Mutex<u32>,
    /// Scripts the next `accept_job`: `Some(true)` a landed rejection
    /// (the job was reassigned or cancelled), `Some(false)` a transport
    /// failure whose ack may or may not have landed. Set by a test
    /// proving the accepted-book is durable before the accept ack.
    fail_next_accept: Mutex<Option<bool>>,
}

impl MockCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues an offer to be handed out by the next `poll_next_job` call.
    pub fn push_offer(&self, offer: JobOffer) {
        self.offers.lock().push_back(offer);
    }

    /// How many queued offers no poll has taken yet — what a drained
    /// or gated node deliberately leaves behind.
    pub fn pending_offers(&self) -> usize {
        self.offers.lock().len()
    }

    pub fn registrations(&self) -> Vec<RegisterRequest> {
        self.registrations.lock().clone()
    }

    pub fn heartbeats(&self) -> Vec<HeartbeatRequest> {
        self.heartbeats.lock().clone()
    }

    pub fn accepts(&self) -> Vec<JobAccept> {
        self.accepts.lock().clone()
    }

    pub fn results(&self) -> Vec<JobResultMessage> {
        self.results.lock().clone()
    }

    pub fn stream_pushes(&self) -> Vec<StreamPush> {
        self.stream_pushes.lock().clone()
    }

    /// Makes the next `n` `submit_result` calls fail at transport
    /// without the coordinator seeing anything — the
    /// restarting-coordinator window the node's outbox exists for.
    pub fn fail_submits(&self, n: u32) {
        *self.submit_failures.lock() = n;
    }

    /// Fails the next `accept_job` at transport — the ack may or may not
    /// have reached the coordinator, the uncertain window that must keep
    /// the job booked for recovery.
    pub fn fail_next_accept_transport(&self) {
        *self.fail_next_accept.lock() = Some(false);
    }

    /// Fails the next `accept_job` with a landed rejection — the
    /// coordinator's final word that the job is gone, which must drop it
    /// from the accepted book.
    pub fn fail_next_accept_rejected(&self) {
        *self.fail_next_accept.lock() = Some(true);
    }
}

#[async_trait]
impl Coordinator for MockCoordinator {
    async fn register(&self, req: RegisterRequest) -> Result<RegisterResponse, CoordinatorError> {
        req.verify()
            .map_err(|e| CoordinatorError::Protocol(e.to_string()))?;
        self.registrations.lock().push(req);
        Ok(RegisterResponse {
            accepted: true,
            operator_session: Some("mock-session".into()),
            reason: None,
            fee_bps: 0,
        })
    }

    async fn heartbeat(
        &self,
        req: HeartbeatRequest,
    ) -> Result<HeartbeatResponse, CoordinatorError> {
        req.verify()
            .map_err(|e| CoordinatorError::Protocol(e.to_string()))?;
        self.heartbeats.lock().push(req);
        Ok(HeartbeatResponse { ack: true })
    }

    async fn poll_next_job(
        &self,
        _operator: &AgentId,
    ) -> Result<Option<JobOffer>, CoordinatorError> {
        Ok(self.offers.lock().pop_front())
    }

    async fn accept_job(&self, decision: JobAccept) -> Result<(), CoordinatorError> {
        if let Some(landed) = self.fail_next_accept.lock().take() {
            return Err(if landed {
                CoordinatorError::Protocol("scripted job rejection".into())
            } else {
                CoordinatorError::Transport("scripted transport failure".into())
            });
        }
        self.accepts.lock().push(decision);
        Ok(())
    }

    async fn submit_result(
        &self,
        result: JobResultMessage,
    ) -> Result<JobResultAck, CoordinatorError> {
        {
            let mut failures = self.submit_failures.lock();
            if *failures > 0 {
                *failures -= 1;
                return Err(CoordinatorError::Transport(
                    "scripted transport failure".into(),
                ));
            }
        }
        result
            .receipt
            .verify()
            .map_err(|e| CoordinatorError::Protocol(e.to_string()))?;
        let job_id = result.receipt.receipt.job_id;
        // The mock always pays the whole receipt price; a test that needs a
        // metered lease draw, a refund verdict, or a transport failure wraps
        // its own `Coordinator`.
        let released_gross_micro_usdc = result.receipt.receipt.price_micro_usdc;
        self.results.lock().push(result);
        Ok(JobResultAck {
            job_id,
            settled: ResultSettlement::Released,
            released_gross_micro_usdc,
        })
    }

    async fn push_stream(&self, push: StreamPush) -> Result<(), CoordinatorError> {
        self.stream_pushes.lock().push(push);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_identity::LocalIdentity;

    #[tokio::test]
    async fn poll_next_job_returns_none_when_empty() {
        let coordinator = MockCoordinator::new();
        let operator = LocalIdentity::generate("operator@local").agent_id();
        assert!(coordinator
            .poll_next_job(&operator)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn register_rejects_a_badly_signed_request() {
        let coordinator = MockCoordinator::new();
        let identity = LocalIdentity::generate("operator@local");
        let profile = covenant_compute_protocol::CapabilityProfile {
            operator: identity.agent_id(),
            hardware: covenant_compute_protocol::HardwareClass::CpuOnly,
            vram_gb: 0,
            models_served: vec!["any".into()],
            job_kinds: vec![covenant_compute_protocol::JobKind::BatchJob],
            price: covenant_compute_protocol::PriceAsk {
                unit: covenant_compute_protocol::PriceUnit::PerJob,
                micro_usdc: 1,
            },
            tee_capable: false,
        };
        let mut req = RegisterRequest::sign(profile, "payout".into(), &identity).unwrap();
        req.payout_address = "tampered".into();
        assert!(coordinator.register(req).await.is_err());
        assert!(coordinator.registrations().is_empty());
    }
}
