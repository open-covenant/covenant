//! Mocked end-to-end loop: mock coordinator offers a job -> node admits
//! it (with a mock escrow-hold attestation) -> executes it (echo
//! executor) -> signs a WorkReceipt -> the receipt verifies -> the
//! earnings ledger credits. No network, no real funds, no real GPU.

use std::sync::Arc;
use std::time::Duration;

use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency, A2ATaskStatus};
use covenant_audit::{AuditLog, InMemoryAuditLog};
use covenant_compute_node::{
    EarningsLedger, EchoExecutor, InMemoryEarningsLedger, MockCoordinator, Node, NodeConfig,
};
use covenant_compute_protocol::{
    CapabilityProfile, CapabilityRequirement, EscrowHoldAttestation, FundingSource, HardwareClass,
    JobEnvelopePayload, JobKind, JobOffer, PriceAsk, PriceUnit, SignedJobEnvelope,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use uuid::Uuid;

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[tokio::test]
async fn full_local_loop_admits_executes_signs_and_credits() {
    let operator_identity = LocalIdentity::generate("operator@local");
    let buyer_identity = LocalIdentity::generate("buyer@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let coordinator_pubkey_b58 = bs58::encode(coordinator_identity.pubkey_bytes()).into_string();

    let profile = CapabilityProfile {
        operator: operator_identity.agent_id(),
        hardware: HardwareClass::CpuOnly,
        vram_gb: 0,
        models_served: vec!["any".into()],
        job_kinds: vec![JobKind::BatchJob],
        price: PriceAsk {
            unit: PriceUnit::PerJob,
            micro_usdc: 5_000,
        },
        tee_capable: false,
        kind_prices: Vec::new(),
        kind_models: Vec::new(),
    };

    let job_id = Uuid::new_v4();
    let now_ms = epoch_ms();
    let envelope_payload = JobEnvelopePayload {
        job_id,
        buyer: buyer_identity.agent_id(),
        kind: JobKind::BatchJob,
        capability_requirement: CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id: None,
            kind: JobKind::BatchJob,
            max_duration_secs: 30,
            min_reputation_bps: None,
        },
        input: vec![Content::text("summarize: the quick brown fox")],
        price_micro_usdc: 5_000,
        deadline_ms: 30_000,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "integration-job-1"),
        issued_at_ms: now_ms,
        referral_code: None,
        stream: false,
    };
    let signed_envelope =
        SignedJobEnvelope::sign(envelope_payload, &buyer_identity).expect("buyer signs envelope");

    // The mock escrow proof: what a real FederationEscrow::hold() impl
    // would have returned, signed by the coordinator.
    let escrow_hold = EscrowHoldAttestation::sign(
        job_id,
        5_000,
        FundingSource::Organic,
        now_ms,
        &coordinator_identity,
    )
    .expect("coordinator signs escrow hold");

    let coordinator = Arc::new(MockCoordinator::new());
    coordinator.push_offer(JobOffer {
        envelope: signed_envelope,
        escrow_hold,
        rework: None,
        reproduction: None,
    });

    let audit = Arc::new(InMemoryAuditLog::new());
    let earnings = Arc::new(InMemoryEarningsLedger::new());
    let executor = Arc::new(EchoExecutor);

    let node = Node::new(
        operator_identity,
        profile,
        coordinator.clone(),
        executor,
        earnings.clone(),
        audit.clone(),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    );

    let outcome = node
        .run_once()
        .await
        .expect("run_once should succeed")
        .expect("a job was offered, so an outcome must come back");

    assert_eq!(outcome.job_id, job_id);
    assert!(outcome.error_message.is_none());
    assert_eq!(outcome.receipt.receipt.status, A2ATaskStatus::Ok);
    assert_eq!(outcome.receipt.receipt.price_micro_usdc, 5_000);

    // The receipt must independently verify — this is what the
    // coordinator (and, transitively, the buyer) checks.
    outcome
        .receipt
        .verify()
        .expect("signed receipt must verify");

    // The receipt actually reached the coordinator.
    let submitted = coordinator.results();
    assert_eq!(submitted.len(), 1);
    assert_eq!(submitted[0].receipt.receipt.job_id, job_id);
    submitted[0]
        .receipt
        .verify()
        .expect("coordinator-side copy must also verify");

    // The node told the coordinator it accepted the job.
    let accepts = coordinator.accepts();
    assert_eq!(accepts.len(), 1);
    assert!(matches!(
        accepts[0],
        covenant_compute_protocol::JobAccept::Accept { job_id: id } if id == job_id
    ));

    // Earnings ledger credited the job, unpaid, for the right amount.
    let unpaid = earnings.unpaid_total_micro_usdc().await;
    assert_eq!(unpaid, 5_000);
    let recent = earnings.recent(10).await;
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].job_id, job_id);
    assert_eq!(recent[0].funding_source, FundingSource::Organic);

    // Job lifecycle events landed in the node's own hash-chained audit
    // log, and the chain itself verifies.
    let events = audit.recent(10).await.expect("read audit log");
    assert_eq!(
        events.len(),
        2,
        "one admission event + one completion event"
    );
    let integrity = audit.verify_integrity().await.expect("verify integrity");
    assert!(integrity.valid);

    // No more jobs queued.
    assert!(node
        .run_once()
        .await
        .expect("run_once with an empty queue must not error")
        .is_none());
}

#[tokio::test]
async fn a_job_the_local_profile_cannot_satisfy_is_rejected_before_execution() {
    let operator_identity = LocalIdentity::generate("operator@local");
    let buyer_identity = LocalIdentity::generate("buyer@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let coordinator_pubkey_b58 = bs58::encode(coordinator_identity.pubkey_bytes()).into_string();

    // Node only serves InferenceCall; buyer asks for BatchJob.
    let profile = CapabilityProfile {
        operator: operator_identity.agent_id(),
        hardware: HardwareClass::CpuOnly,
        vram_gb: 0,
        models_served: vec!["any".into()],
        job_kinds: vec![JobKind::InferenceCall],
        price: PriceAsk {
            unit: PriceUnit::PerJob,
            micro_usdc: 5_000,
        },
        tee_capable: false,
        kind_prices: Vec::new(),
        kind_models: Vec::new(),
    };

    let job_id = Uuid::new_v4();
    let now_ms = epoch_ms();
    let envelope_payload = JobEnvelopePayload {
        job_id,
        buyer: buyer_identity.agent_id(),
        kind: JobKind::BatchJob,
        capability_requirement: CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id: None,
            kind: JobKind::BatchJob,
            max_duration_secs: 30,
            min_reputation_bps: None,
        },
        input: vec![Content::text("do batch work")],
        price_micro_usdc: 5_000,
        deadline_ms: 30_000,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "integration-job-2"),
        issued_at_ms: now_ms,
        referral_code: None,
        stream: false,
    };
    let signed_envelope =
        SignedJobEnvelope::sign(envelope_payload, &buyer_identity).expect("sign envelope");
    let escrow_hold = EscrowHoldAttestation::sign(
        job_id,
        5_000,
        FundingSource::Organic,
        now_ms,
        &coordinator_identity,
    )
    .expect("sign escrow hold");

    let coordinator = Arc::new(MockCoordinator::new());
    coordinator.push_offer(JobOffer {
        envelope: signed_envelope,
        escrow_hold,
        rework: None,
        reproduction: None,
    });

    let audit = Arc::new(InMemoryAuditLog::new());
    let earnings = Arc::new(InMemoryEarningsLedger::new());
    let executor = Arc::new(EchoExecutor);

    let node = Node::new(
        operator_identity,
        profile,
        coordinator.clone(),
        executor,
        earnings.clone(),
        audit,
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    );

    let result = node.run_once().await;
    assert!(result.is_err(), "capability mismatch must fail closed");

    // Rejected before execution: no receipt submitted, nothing credited.
    assert!(coordinator.results().is_empty());
    assert_eq!(earnings.unpaid_total_micro_usdc().await, 0);
    let accepts = coordinator.accepts();
    assert_eq!(accepts.len(), 1);
    assert!(matches!(
        accepts[0],
        covenant_compute_protocol::JobAccept::Reject { job_id: id, .. } if id == job_id
    ));
}

#[tokio::test]
async fn a_streaming_job_relays_chunks_and_still_settles_by_receipt() {
    let operator_identity = LocalIdentity::generate("operator@local");
    let buyer_identity = LocalIdentity::generate("buyer@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let coordinator_pubkey_b58 = bs58::encode(coordinator_identity.pubkey_bytes()).into_string();

    let profile = CapabilityProfile {
        operator: operator_identity.agent_id(),
        hardware: HardwareClass::CpuOnly,
        vram_gb: 0,
        models_served: vec!["any".into()],
        job_kinds: vec![JobKind::BatchJob],
        price: PriceAsk {
            unit: PriceUnit::PerJob,
            micro_usdc: 5_000,
        },
        tee_capable: false,
        kind_prices: Vec::new(),
        kind_models: Vec::new(),
    };

    let job_id = Uuid::new_v4();
    let now_ms = epoch_ms();
    let envelope_payload = JobEnvelopePayload {
        job_id,
        buyer: buyer_identity.agent_id(),
        kind: JobKind::BatchJob,
        capability_requirement: CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id: None,
            kind: JobKind::BatchJob,
            max_duration_secs: 30,
            min_reputation_bps: None,
        },
        input: vec![Content::text("fifty-"), Content::text("five")],
        price_micro_usdc: 5_000,
        deadline_ms: 30_000,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "integration-stream-1"),
        issued_at_ms: now_ms,
        referral_code: None,
        stream: true,
    };
    let signed_envelope =
        SignedJobEnvelope::sign(envelope_payload, &buyer_identity).expect("sign envelope");
    let escrow_hold = EscrowHoldAttestation::sign(
        job_id,
        5_000,
        FundingSource::Organic,
        now_ms,
        &coordinator_identity,
    )
    .expect("sign escrow hold");

    let coordinator = Arc::new(MockCoordinator::new());
    coordinator.push_offer(JobOffer {
        envelope: signed_envelope,
        escrow_hold,
        rework: None,
        reproduction: None,
    });

    let node = Node::new(
        operator_identity,
        profile,
        coordinator.clone(),
        Arc::new(EchoExecutor),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    );

    let outcome = node
        .run_once()
        .await
        .expect("run_once should succeed")
        .expect("an outcome comes back");
    assert_eq!(outcome.receipt.receipt.status, A2ATaskStatus::Ok);

    // However the flush timing batched them, the pushes carry every
    // delta exactly once, in seq order, and only the last says done.
    let pushes = coordinator.stream_pushes();
    assert!(!pushes.is_empty(), "a streaming job must push chunks");
    let chunks: Vec<_> = pushes.iter().flat_map(|p| p.chunks.clone()).collect();
    let seqs: Vec<u64> = chunks.iter().map(|c| c.seq).collect();
    assert_eq!(seqs, vec![0, 1]);
    let streamed: String = chunks.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(streamed, "fifty-five");
    assert!(
        pushes.last().unwrap().done,
        "the tail flush closes the feed"
    );
    assert!(
        pushes.iter().rev().skip(1).all(|p| !p.done),
        "done only on the final push"
    );
    assert!(pushes.iter().all(|p| p.job_id == job_id));

    // The receipt path is untouched: full output submitted and verified.
    let submitted = coordinator.results();
    assert_eq!(submitted.len(), 1);
    assert_eq!(
        submitted[0].output,
        vec![Content::text("fifty-"), Content::text("five")]
    );
}

/// A coordinator whose chunk relay is down but everything else works —
/// the failure the forwarder must absorb without touching the job.
struct DeadRelayCoordinator(MockCoordinator);

#[async_trait::async_trait]
impl covenant_compute_node::Coordinator for DeadRelayCoordinator {
    async fn register(
        &self,
        req: covenant_compute_protocol::RegisterRequest,
    ) -> Result<covenant_compute_protocol::RegisterResponse, covenant_compute_node::CoordinatorError>
    {
        self.0.register(req).await
    }
    async fn heartbeat(
        &self,
        req: covenant_compute_protocol::HeartbeatRequest,
    ) -> Result<covenant_compute_protocol::HeartbeatResponse, covenant_compute_node::CoordinatorError>
    {
        self.0.heartbeat(req).await
    }
    async fn poll_next_job(
        &self,
        operator: &covenant_types::AgentId,
    ) -> Result<Option<JobOffer>, covenant_compute_node::CoordinatorError> {
        self.0.poll_next_job(operator).await
    }
    async fn accept_job(
        &self,
        decision: covenant_compute_protocol::JobAccept,
    ) -> Result<(), covenant_compute_node::CoordinatorError> {
        self.0.accept_job(decision).await
    }
    async fn submit_result(
        &self,
        result: covenant_compute_protocol::JobResultMessage,
    ) -> Result<covenant_compute_protocol::JobResultAck, covenant_compute_node::CoordinatorError>
    {
        self.0.submit_result(result).await
    }
    async fn push_stream(
        &self,
        _push: covenant_compute_protocol::StreamPush,
    ) -> Result<(), covenant_compute_node::CoordinatorError> {
        Err(covenant_compute_node::CoordinatorError::Transport(
            "relay down".into(),
        ))
    }
}

#[tokio::test]
async fn a_dead_chunk_relay_never_fails_the_job_itself() {
    let operator_identity = LocalIdentity::generate("operator@local");
    let buyer_identity = LocalIdentity::generate("buyer@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let coordinator_pubkey_b58 = bs58::encode(coordinator_identity.pubkey_bytes()).into_string();

    let profile = CapabilityProfile {
        operator: operator_identity.agent_id(),
        hardware: HardwareClass::CpuOnly,
        vram_gb: 0,
        models_served: vec!["any".into()],
        job_kinds: vec![JobKind::BatchJob],
        price: PriceAsk {
            unit: PriceUnit::PerJob,
            micro_usdc: 5_000,
        },
        tee_capable: false,
        kind_prices: Vec::new(),
        kind_models: Vec::new(),
    };

    let job_id = Uuid::new_v4();
    let now_ms = epoch_ms();
    let envelope_payload = JobEnvelopePayload {
        job_id,
        buyer: buyer_identity.agent_id(),
        kind: JobKind::BatchJob,
        capability_requirement: CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id: None,
            kind: JobKind::BatchJob,
            max_duration_secs: 30,
            min_reputation_bps: None,
        },
        input: vec![Content::text("still served")],
        price_micro_usdc: 5_000,
        deadline_ms: 30_000,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "integration-stream-2"),
        issued_at_ms: now_ms,
        referral_code: None,
        stream: true,
    };
    let signed_envelope =
        SignedJobEnvelope::sign(envelope_payload, &buyer_identity).expect("sign envelope");
    let escrow_hold = EscrowHoldAttestation::sign(
        job_id,
        5_000,
        FundingSource::Organic,
        now_ms,
        &coordinator_identity,
    )
    .expect("sign escrow hold");

    let coordinator = Arc::new(DeadRelayCoordinator(MockCoordinator::new()));
    coordinator.0.push_offer(JobOffer {
        envelope: signed_envelope,
        escrow_hold,
        rework: None,
        reproduction: None,
    });

    let earnings = Arc::new(InMemoryEarningsLedger::new());
    let node = Node::new(
        operator_identity,
        profile,
        coordinator.clone(),
        Arc::new(EchoExecutor),
        earnings.clone(),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    );

    let outcome = node
        .run_once()
        .await
        .expect("run_once should succeed")
        .expect("an outcome comes back");
    assert_eq!(outcome.receipt.receipt.status, A2ATaskStatus::Ok);
    assert!(outcome.error_message.is_none());
    assert_eq!(coordinator.0.results().len(), 1, "the receipt still lands");
    assert_eq!(earnings.unpaid_total_micro_usdc().await, 5_000);
}

/// A coordinator that delivers fine but settles every result as a
/// refund — what a node sees when the buyer's deadline expired while
/// the result was in flight.
struct RefundAckCoordinator(MockCoordinator);

#[async_trait::async_trait]
impl covenant_compute_node::Coordinator for RefundAckCoordinator {
    async fn register(
        &self,
        req: covenant_compute_protocol::RegisterRequest,
    ) -> Result<covenant_compute_protocol::RegisterResponse, covenant_compute_node::CoordinatorError>
    {
        self.0.register(req).await
    }
    async fn heartbeat(
        &self,
        req: covenant_compute_protocol::HeartbeatRequest,
    ) -> Result<covenant_compute_protocol::HeartbeatResponse, covenant_compute_node::CoordinatorError>
    {
        self.0.heartbeat(req).await
    }
    async fn poll_next_job(
        &self,
        operator: &covenant_types::AgentId,
    ) -> Result<Option<JobOffer>, covenant_compute_node::CoordinatorError> {
        self.0.poll_next_job(operator).await
    }
    async fn accept_job(
        &self,
        decision: covenant_compute_protocol::JobAccept,
    ) -> Result<(), covenant_compute_node::CoordinatorError> {
        self.0.accept_job(decision).await
    }
    async fn submit_result(
        &self,
        result: covenant_compute_protocol::JobResultMessage,
    ) -> Result<covenant_compute_protocol::JobResultAck, covenant_compute_node::CoordinatorError>
    {
        let ack = self.0.submit_result(result).await?;
        Ok(covenant_compute_protocol::JobResultAck {
            settled: covenant_compute_protocol::ResultSettlement::Refunded,
            ..ack
        })
    }
    async fn push_stream(
        &self,
        push: covenant_compute_protocol::StreamPush,
    ) -> Result<(), covenant_compute_node::CoordinatorError> {
        self.0.push_stream(push).await
    }
}

/// A coordinator that delivers and pays, but releases a gross smaller than
/// the receipt's envelope price — what a node serving a lease sees when the
/// session bills only its metered seconds and the escrowed window ceiling
/// is refunded down to them.
struct MeteredReleaseCoordinator {
    inner: MockCoordinator,
    released_gross_micro_usdc: u64,
}

#[async_trait::async_trait]
impl covenant_compute_node::Coordinator for MeteredReleaseCoordinator {
    async fn register(
        &self,
        req: covenant_compute_protocol::RegisterRequest,
    ) -> Result<covenant_compute_protocol::RegisterResponse, covenant_compute_node::CoordinatorError>
    {
        self.inner.register(req).await
    }
    async fn heartbeat(
        &self,
        req: covenant_compute_protocol::HeartbeatRequest,
    ) -> Result<covenant_compute_protocol::HeartbeatResponse, covenant_compute_node::CoordinatorError>
    {
        self.inner.heartbeat(req).await
    }
    async fn poll_next_job(
        &self,
        operator: &covenant_types::AgentId,
    ) -> Result<Option<JobOffer>, covenant_compute_node::CoordinatorError> {
        self.inner.poll_next_job(operator).await
    }
    async fn accept_job(
        &self,
        decision: covenant_compute_protocol::JobAccept,
    ) -> Result<(), covenant_compute_node::CoordinatorError> {
        self.inner.accept_job(decision).await
    }
    async fn submit_result(
        &self,
        result: covenant_compute_protocol::JobResultMessage,
    ) -> Result<covenant_compute_protocol::JobResultAck, covenant_compute_node::CoordinatorError>
    {
        let ack = self.inner.submit_result(result).await?;
        Ok(covenant_compute_protocol::JobResultAck {
            released_gross_micro_usdc: self.released_gross_micro_usdc,
            ..ack
        })
    }
    async fn push_stream(
        &self,
        push: covenant_compute_protocol::StreamPush,
    ) -> Result<(), covenant_compute_node::CoordinatorError> {
        self.inner.push_stream(push).await
    }
}

#[tokio::test]
async fn a_result_settled_as_a_refund_books_no_earnings() {
    let operator_identity = LocalIdentity::generate("operator@local");
    let buyer_identity = LocalIdentity::generate("buyer@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let coordinator_pubkey_b58 = bs58::encode(coordinator_identity.pubkey_bytes()).into_string();

    let profile = CapabilityProfile {
        operator: operator_identity.agent_id(),
        hardware: HardwareClass::CpuOnly,
        vram_gb: 0,
        models_served: vec!["any".into()],
        job_kinds: vec![JobKind::BatchJob],
        price: PriceAsk {
            unit: PriceUnit::PerJob,
            micro_usdc: 5_000,
        },
        tee_capable: false,
        kind_prices: Vec::new(),
        kind_models: Vec::new(),
    };

    let job_id = Uuid::new_v4();
    let now_ms = epoch_ms();
    let envelope_payload = JobEnvelopePayload {
        job_id,
        buyer: buyer_identity.agent_id(),
        kind: JobKind::BatchJob,
        capability_requirement: CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id: None,
            kind: JobKind::BatchJob,
            max_duration_secs: 30,
            min_reputation_bps: None,
        },
        input: vec![Content::text("delivered too late to pay")],
        price_micro_usdc: 5_000,
        deadline_ms: 30_000,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "integration-refund-1"),
        issued_at_ms: now_ms,
        referral_code: None,
        stream: false,
    };
    let signed_envelope =
        SignedJobEnvelope::sign(envelope_payload, &buyer_identity).expect("sign envelope");
    let escrow_hold = EscrowHoldAttestation::sign(
        job_id,
        5_000,
        FundingSource::Organic,
        now_ms,
        &coordinator_identity,
    )
    .expect("sign escrow hold");

    let coordinator = Arc::new(RefundAckCoordinator(MockCoordinator::new()));
    coordinator.0.push_offer(JobOffer {
        envelope: signed_envelope,
        escrow_hold,
        rework: None,
        reproduction: None,
    });

    let earnings = Arc::new(InMemoryEarningsLedger::new());
    let node = Node::new(
        operator_identity,
        profile,
        coordinator.clone(),
        Arc::new(EchoExecutor),
        earnings.clone(),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    );

    let outcome = node
        .run_once()
        .await
        .expect("run_once should succeed")
        .expect("an outcome comes back");
    assert_eq!(outcome.receipt.receipt.status, A2ATaskStatus::Ok);
    assert_eq!(coordinator.0.results().len(), 1, "the result was delivered");
    assert_eq!(
        earnings.unpaid_total_micro_usdc().await,
        0,
        "a refunded settlement must not book owed money"
    );
    assert!(earnings.recent(10).await.is_empty());
}

#[tokio::test]
async fn a_metered_release_credits_the_coordinators_gross_not_the_receipt_price() {
    // A lease escrows its window's ceiling but is billed only the seconds
    // it ran: the coordinator releases the metered draw and refunds the
    // rest. The operator's credit must follow that released gross, not the
    // ceiling stamped on the receipt — otherwise the books overstate what
    // is owed and the operator's own on-chain payout audit trips against
    // the smaller transfer the coordinator actually made.
    let operator_identity = LocalIdentity::generate("operator@local");
    let buyer_identity = LocalIdentity::generate("buyer@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let coordinator_pubkey_b58 = bs58::encode(coordinator_identity.pubkey_bytes()).into_string();

    let profile = CapabilityProfile {
        operator: operator_identity.agent_id(),
        hardware: HardwareClass::CpuOnly,
        vram_gb: 0,
        models_served: vec!["any".into()],
        job_kinds: vec![JobKind::BatchJob],
        price: PriceAsk {
            unit: PriceUnit::PerJob,
            micro_usdc: 5_000,
        },
        tee_capable: false,
        kind_prices: Vec::new(),
        kind_models: Vec::new(),
    };

    let job_id = Uuid::new_v4();
    let now_ms = epoch_ms();
    let envelope_payload = JobEnvelopePayload {
        job_id,
        buyer: buyer_identity.agent_id(),
        kind: JobKind::BatchJob,
        capability_requirement: CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id: None,
            kind: JobKind::BatchJob,
            max_duration_secs: 30,
            min_reputation_bps: None,
        },
        input: vec![Content::text("a lease closed early")],
        // The escrowed ceiling the receipt carries.
        price_micro_usdc: 5_000,
        deadline_ms: 30_000,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "integration-metered-1"),
        issued_at_ms: now_ms,
        referral_code: None,
        stream: false,
    };
    let signed_envelope =
        SignedJobEnvelope::sign(envelope_payload, &buyer_identity).expect("sign envelope");
    let escrow_hold = EscrowHoldAttestation::sign(
        job_id,
        5_000,
        FundingSource::Organic,
        now_ms,
        &coordinator_identity,
    )
    .expect("sign escrow hold");

    // The coordinator releases 3_000 of the 5_000 ceiling: the metered draw.
    let coordinator = Arc::new(MeteredReleaseCoordinator {
        inner: MockCoordinator::new(),
        released_gross_micro_usdc: 3_000,
    });
    coordinator.inner.push_offer(JobOffer {
        envelope: signed_envelope,
        escrow_hold,
        rework: None,
        reproduction: None,
    });

    let earnings = Arc::new(InMemoryEarningsLedger::new());
    let node = Node::new(
        operator_identity,
        profile,
        coordinator.clone(),
        Arc::new(EchoExecutor),
        earnings.clone(),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            // A 10% fee, so the credit proves the fee is taken of the
            // released gross (3_000 → 300), not the ceiling (5_000 → 500).
            fee_bps: 1_000,
        },
    );

    let outcome = node
        .run_once()
        .await
        .expect("run_once should succeed")
        .expect("an outcome comes back");
    // The receipt still carries the escrowed ceiling; only the credit is
    // sized from the coordinator's smaller released gross.
    assert_eq!(outcome.receipt.receipt.price_micro_usdc, 5_000);

    assert_eq!(
        earnings.unpaid_total_micro_usdc().await,
        2_700,
        "credit is the metered 3_000 gross net of its 300 fee, not the 5_000 ceiling"
    );
    let recent = earnings.recent(10).await;
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].job_id, job_id);
    assert_eq!(recent[0].amount_micro_usdc, 2_700);
    assert_eq!(recent[0].fee_micro_usdc, 300);
}

fn offer_for(
    job_id: Uuid,
    tag: &str,
    buyer_identity: &LocalIdentity,
    coordinator_identity: &LocalIdentity,
) -> JobOffer {
    offer_for_at(
        job_id,
        tag,
        buyer_identity,
        coordinator_identity,
        epoch_ms(),
        30_000,
    )
}

fn offer_for_at(
    job_id: Uuid,
    tag: &str,
    buyer_identity: &LocalIdentity,
    coordinator_identity: &LocalIdentity,
    issued_at_ms: u64,
    deadline_ms: u64,
) -> JobOffer {
    let envelope_payload = JobEnvelopePayload {
        job_id,
        buyer: buyer_identity.agent_id(),
        kind: JobKind::BatchJob,
        capability_requirement: CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id: None,
            kind: JobKind::BatchJob,
            max_duration_secs: 30,
            min_reputation_bps: None,
        },
        input: vec![Content::text("work worth redelivering")],
        price_micro_usdc: 5_000,
        deadline_ms,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, tag),
        issued_at_ms,
        referral_code: None,
        stream: false,
    };
    let envelope =
        SignedJobEnvelope::sign(envelope_payload, buyer_identity).expect("sign envelope");
    let escrow_hold = EscrowHoldAttestation::sign(
        job_id,
        5_000,
        FundingSource::Organic,
        issued_at_ms,
        coordinator_identity,
    )
    .expect("sign escrow hold");
    JobOffer {
        envelope,
        escrow_hold,
        rework: None,
        reproduction: None,
    }
}

fn cpu_profile_for(operator_identity: &LocalIdentity) -> CapabilityProfile {
    CapabilityProfile {
        operator: operator_identity.agent_id(),
        hardware: HardwareClass::CpuOnly,
        vram_gb: 0,
        models_served: vec!["any".into()],
        job_kinds: vec![JobKind::BatchJob],
        price: PriceAsk {
            unit: PriceUnit::PerJob,
            micro_usdc: 5_000,
        },
        tee_capable: false,
        kind_prices: Vec::new(),
        kind_models: Vec::new(),
    }
}

#[tokio::test]
async fn an_undeliverable_result_queues_durably_and_credits_once_on_redelivery() {
    use covenant_compute_node::ResultOutbox;

    let buyer_identity = LocalIdentity::generate("buyer@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let coordinator_pubkey_b58 = bs58::encode(coordinator_identity.pubkey_bytes()).into_string();
    let dir = tempfile::tempdir().unwrap();
    let identity_path = dir.path().join("identity.json");
    let outbox_path = dir.path().join("outbox.jsonl");

    // Life 1: the coordinator dies exactly when the result is pushed.
    // The job still concludes locally, nothing is credited, and the
    // signed message lands durably in the outbox.
    let operator1 = LocalIdentity::load_or_create(&identity_path, "operator@local").unwrap();
    let profile1 = cpu_profile_for(&operator1);
    let job_id = Uuid::new_v4();
    let coordinator1 = Arc::new(MockCoordinator::new());
    coordinator1.push_offer(offer_for(
        job_id,
        "integration-outbox-1",
        &buyer_identity,
        &coordinator_identity,
    ));
    coordinator1.fail_submits(1);
    let earnings1 = Arc::new(InMemoryEarningsLedger::new());
    let node1 = Node::new(
        operator1,
        profile1,
        coordinator1.clone(),
        Arc::new(EchoExecutor),
        earnings1.clone(),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58: coordinator_pubkey_b58.clone(),
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    )
    .with_outbox(Arc::new(ResultOutbox::open(&outbox_path).unwrap()));

    let outcome = node1
        .run_once()
        .await
        .expect("an undeliverable result must not fail the job")
        .expect("an outcome comes back");
    assert_eq!(outcome.receipt.receipt.status, A2ATaskStatus::Ok);
    assert!(
        coordinator1.results().is_empty(),
        "the transport failure means the coordinator saw nothing"
    );
    assert_eq!(
        earnings1.unpaid_total_micro_usdc().await,
        0,
        "no settlement verdict, no earnings"
    );

    // Life 2: the node restarts against a healthy coordinator. The
    // reopened outbox redelivers, the verdict credits — exactly once.
    let operator2 = LocalIdentity::load_or_create(&identity_path, "operator@local").unwrap();
    let profile2 = cpu_profile_for(&operator2);
    let coordinator2 = Arc::new(MockCoordinator::new());
    let earnings2 = Arc::new(InMemoryEarningsLedger::new());
    let outbox2 = Arc::new(ResultOutbox::open(&outbox_path).unwrap());
    assert_eq!(outbox2.pending().len(), 1, "the queued result survived");
    let node2 = Node::new(
        operator2,
        profile2,
        coordinator2.clone(),
        Arc::new(EchoExecutor),
        earnings2.clone(),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    )
    .with_outbox(outbox2.clone());

    assert_eq!(node2.drain_outbox().await, 1);
    let delivered = coordinator2.results();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].receipt.receipt.job_id, job_id);
    delivered[0].receipt.verify().expect("redelivered verbatim");
    assert_eq!(earnings2.unpaid_total_micro_usdc().await, 5_000);
    assert!(outbox2.pending().is_empty());

    // Nothing left: a second drain neither re-sends nor re-credits.
    assert_eq!(node2.drain_outbox().await, 0);
    assert_eq!(coordinator2.results().len(), 1);
    assert_eq!(earnings2.unpaid_total_micro_usdc().await, 5_000);
}

#[tokio::test]
async fn a_drained_result_follows_the_verdict_and_a_refused_one_is_dropped() {
    use covenant_compute_node::{OutboxEntry, ResultOutbox};

    let operator_identity = LocalIdentity::generate("operator@local");
    let buyer_identity = LocalIdentity::generate("buyer@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let coordinator_pubkey_b58 = bs58::encode(coordinator_identity.pubkey_bytes()).into_string();

    // A message the coordinator will refuse outright — a tampered
    // receipt that fails verification — prepared up front, enqueued
    // later.
    let refused_id = Uuid::new_v4();
    let mut tampered = covenant_compute_protocol::SignedWorkReceipt::sign(
        covenant_compute_protocol::WorkReceiptPayload {
            job_id: refused_id,
            operator: operator_identity.agent_id(),
            job_hash_hex: "aa".repeat(32),
            result_hash_hex: "bb".repeat(32),
            meter: covenant_compute_protocol::JobMeter {
                wall_ms: 1,
                tokens_in: None,
                tokens_out: None,
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc: 5_000,
            status: A2ATaskStatus::Ok,
            executed_at_ms: epoch_ms(),
            node_audit_root_hex: "cc".repeat(32),
        },
        &operator_identity,
    )
    .unwrap();
    tampered.receipt.price_micro_usdc = 9_999; // breaks the signature

    // The job queued during the outage, and its deadline expired before
    // the coordinator came back: the redelivery lands but settles as a
    // refund, so nothing may be credited.
    let profile = cpu_profile_for(&operator_identity);
    let job_id = Uuid::new_v4();
    let coordinator = Arc::new(RefundAckCoordinator(MockCoordinator::new()));
    coordinator.0.push_offer(offer_for(
        job_id,
        "integration-outbox-2",
        &buyer_identity,
        &coordinator_identity,
    ));
    coordinator.0.fail_submits(1);
    let earnings = Arc::new(InMemoryEarningsLedger::new());
    let outbox = Arc::new(ResultOutbox::in_memory());
    let node = Node::new(
        operator_identity,
        profile,
        coordinator.clone(),
        Arc::new(EchoExecutor),
        earnings.clone(),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    )
    .with_outbox(outbox.clone());

    node.run_once()
        .await
        .expect("queued, not failed")
        .expect("an outcome comes back");
    assert_eq!(outbox.pending().len(), 1);
    assert_eq!(node.drain_outbox().await, 1);
    assert!(outbox.pending().is_empty());
    assert_eq!(
        earnings.unpaid_total_micro_usdc().await,
        0,
        "a refunded redelivery books nothing"
    );

    // The refused message is the coordinator's final word — dropped
    // from the queue, never re-sent, never credited.
    outbox
        .enqueue(OutboxEntry {
            job_id: refused_id,
            message: covenant_compute_protocol::JobResultMessage {
                receipt: tampered,
                output: vec![Content::text("tampered")],
            },
            funding_source: FundingSource::Organic,
            queued_at_ms: epoch_ms(),
            settled: false,
        })
        .unwrap();

    assert_eq!(node.drain_outbox().await, 1, "refused still settles");
    assert!(outbox.pending().is_empty());
    assert_eq!(earnings.unpaid_total_micro_usdc().await, 0);
}

/// An earnings ledger that fails `credit` with a persistence error
/// while armed, then delegates once disarmed — the transient disk fault
/// the outbox drain must survive without dropping the owed credit.
struct FaultyEarningsLedger {
    inner: InMemoryEarningsLedger,
    fail_credit: std::sync::atomic::AtomicBool,
}

impl FaultyEarningsLedger {
    fn new() -> Self {
        Self {
            inner: InMemoryEarningsLedger::new(),
            fail_credit: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn set_failing(&self, failing: bool) {
        self.fail_credit
            .store(failing, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl EarningsLedger for FaultyEarningsLedger {
    async fn credit(
        &self,
        entry: covenant_compute_node::EarningsEntry,
    ) -> Result<(), covenant_compute_node::EarningsError> {
        if self.fail_credit.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(covenant_compute_node::EarningsError::Persist(
                "scripted disk failure".into(),
            ));
        }
        self.inner.credit(entry).await
    }

    async fn mark_paid(
        &self,
        job_id: Uuid,
        tx_signature: Option<String>,
        paid_at_ms: u64,
    ) -> Result<bool, covenant_compute_node::EarningsError> {
        self.inner.mark_paid(job_id, tx_signature, paid_at_ms).await
    }

    async fn unpaid_total_micro_usdc(&self) -> u64 {
        self.inner.unpaid_total_micro_usdc().await
    }

    async fn recent(&self, limit: usize) -> Vec<covenant_compute_node::EarningsEntry> {
        self.inner.recent(limit).await
    }

    async fn is_credited(&self, job_id: Uuid) -> bool {
        self.inner.is_credited(job_id).await
    }
}

#[tokio::test]
async fn a_credit_that_fails_to_persist_holds_the_outbox_entry_for_retry() {
    use covenant_compute_node::ResultOutbox;

    // A released result redelivers, but the earnings credit fails to
    // persist (a transient disk fault). The entry must stay queued so a
    // later drain retries the credit — the coordinator answers the
    // replay with the same verdict idempotently — instead of settling
    // away the only durable record of money the coordinator released.
    let operator_identity = LocalIdentity::generate("operator@local");
    let buyer_identity = LocalIdentity::generate("buyer@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let coordinator_pubkey_b58 = bs58::encode(coordinator_identity.pubkey_bytes()).into_string();

    let job_id = Uuid::new_v4();
    let coordinator = Arc::new(MockCoordinator::new());
    coordinator.push_offer(offer_for(
        job_id,
        "integration-persist-hold",
        &buyer_identity,
        &coordinator_identity,
    ));
    // The first submission fails at the transport so the result queues.
    coordinator.fail_submits(1);
    let earnings = Arc::new(FaultyEarningsLedger::new());
    let outbox = Arc::new(ResultOutbox::in_memory());
    let profile = cpu_profile_for(&operator_identity);
    let node = Node::new(
        operator_identity,
        profile,
        coordinator.clone(),
        Arc::new(EchoExecutor),
        earnings.clone(),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    )
    .with_outbox(outbox.clone());

    node.run_once()
        .await
        .expect("queued, not failed")
        .expect("an outcome comes back");
    assert_eq!(outbox.pending().len(), 1, "the undeliverable result queued");

    // Redelivery lands (Released), but the credit can't persist: the
    // entry must be held, not settled, and nothing booked.
    earnings.set_failing(true);
    assert_eq!(
        node.drain_outbox().await,
        0,
        "a credit that can't persist settles nothing"
    );
    assert_eq!(
        outbox.pending().len(),
        1,
        "the entry is held for retry, not dropped"
    );
    assert_eq!(earnings.unpaid_total_micro_usdc().await, 0);

    // The disk recovers; the next drain re-submits (idempotent verdict),
    // credits exactly once, and only now settles the entry.
    earnings.set_failing(false);
    assert_eq!(
        node.drain_outbox().await,
        1,
        "the retry credits and settles"
    );
    assert!(outbox.pending().is_empty());
    assert_eq!(
        earnings.unpaid_total_micro_usdc().await,
        5_000,
        "the held credit lands on retry, exactly once"
    );
}

fn accepted_entry_from(offer: JobOffer, lives: u32) -> covenant_compute_node::AcceptedEntry {
    covenant_compute_node::AcceptedEntry {
        job_id: offer.envelope.payload.job_id,
        accepted_at_ms: offer.envelope.payload.issued_at_ms,
        envelope: offer.envelope,
        escrow_hold: offer.escrow_hold,
        lives,
        settled: false,
    }
}

/// Counts executions and echoes — recovery tests assert compute was
/// (or was not) burned, not just what the coordinator saw.
struct CountingExecutor(Arc<std::sync::atomic::AtomicUsize>);

#[async_trait::async_trait]
impl covenant_compute_node::JobExecutor for CountingExecutor {
    async fn execute(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
    ) -> Result<covenant_compute_node::ExecutionOutcome, covenant_compute_node::ExecutorError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        EchoExecutor.execute(job, deadline).await
    }
}

#[tokio::test]
async fn a_job_interrupted_mid_execution_is_re_served_at_boot_and_credits_once() {
    use covenant_compute_node::{AcceptedBook, ExecutionOutcome, ExecutorError, JobExecutor};
    use tokio::sync::Notify;

    // Hangs forever once started — the executor of a process about to
    // die. `started` pins the abort to mid-execution.
    struct HangingExecutor {
        started: Arc<Notify>,
    }

    #[async_trait::async_trait]
    impl JobExecutor for HangingExecutor {
        async fn execute(
            &self,
            _job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<ExecutionOutcome, ExecutorError> {
            self.started.notify_one();
            std::future::pending::<()>().await;
            unreachable!()
        }
    }

    let buyer_identity = LocalIdentity::generate("buyer@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let coordinator_pubkey_b58 = bs58::encode(coordinator_identity.pubkey_bytes()).into_string();
    let dir = tempfile::tempdir().unwrap();
    let identity_path = dir.path().join("identity.json");
    let book_path = dir.path().join("accepted.jsonl");
    let job_id = Uuid::new_v4();

    // Life 1: accept lands, the book remembers, execution never ends —
    // the process dies holding the job.
    let operator1 = LocalIdentity::load_or_create(&identity_path, "operator@local").unwrap();
    let profile1 = cpu_profile_for(&operator1);
    let coordinator1 = Arc::new(MockCoordinator::new());
    coordinator1.push_offer(offer_for(
        job_id,
        "integration-recover-1",
        &buyer_identity,
        &coordinator_identity,
    ));
    let started = Arc::new(Notify::new());
    let node1 = Arc::new(
        Node::new(
            operator1,
            profile1,
            coordinator1.clone(),
            Arc::new(HangingExecutor {
                started: started.clone(),
            }),
            Arc::new(InMemoryEarningsLedger::new()),
            Arc::new(InMemoryAuditLog::new()),
            NodeConfig {
                coordinator_pubkey_b58: coordinator_pubkey_b58.clone(),
                max_in_flight: 4,
                preempt_grace: Duration::from_secs(2),
                fee_bps: 0,
            },
        )
        .with_accepted_book(Arc::new(AcceptedBook::open(&book_path).unwrap())),
    );
    let run = {
        let node = node1.clone();
        tokio::spawn(async move { node.run_once().await })
    };
    started.notified().await;
    run.abort();
    let _ = run.await;
    assert!(
        coordinator1.results().is_empty(),
        "the process died before any result"
    );

    // Life 2: the reopened book still holds the job; recovery re-runs
    // it through the normal path and the settlement credits.
    let operator2 = LocalIdentity::load_or_create(&identity_path, "operator@local").unwrap();
    let profile2 = cpu_profile_for(&operator2);
    let coordinator2 = Arc::new(MockCoordinator::new());
    let earnings2 = Arc::new(InMemoryEarningsLedger::new());
    let book2 = Arc::new(AcceptedBook::open(&book_path).unwrap());
    let restored = book2.pending();
    assert_eq!(restored.len(), 1, "the accepted job survived the death");
    assert_eq!(restored[0].job_id, job_id);
    assert_eq!(restored[0].lives, 1);
    let node2 = Node::new(
        operator2,
        profile2,
        coordinator2.clone(),
        Arc::new(EchoExecutor),
        earnings2.clone(),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    )
    .with_accepted_book(book2.clone());

    assert_eq!(node2.recover_accepted().await, 1);
    let delivered = coordinator2.results();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].receipt.receipt.job_id, job_id);
    delivered[0].receipt.verify().expect("re-served and signed");
    assert_eq!(earnings2.unpaid_total_micro_usdc().await, 5_000);
    assert!(book2.pending().is_empty(), "the re-served job settled");

    // Nothing left: recovery is idempotent once the book is clean.
    assert_eq!(node2.recover_accepted().await, 0);
    assert_eq!(coordinator2.results().len(), 1);
    assert_eq!(earnings2.unpaid_total_micro_usdc().await, 5_000);
}

#[tokio::test]
async fn a_transport_failed_accept_leaves_the_job_booked_for_recovery() {
    use covenant_compute_node::AcceptedBook;

    let buyer_identity = LocalIdentity::generate("buyer@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let coordinator_pubkey_b58 = bs58::encode(coordinator_identity.pubkey_bytes()).into_string();
    let operator = LocalIdentity::generate("operator@local");
    let profile = cpu_profile_for(&operator);
    let job_id = Uuid::new_v4();

    let coordinator = Arc::new(MockCoordinator::new());
    coordinator.push_offer(offer_for(
        job_id,
        "accept-transport",
        &buyer_identity,
        &coordinator_identity,
    ));
    // The accept ack fails at transport — it may or may not have reached
    // the coordinator, so the node must not forget the job.
    coordinator.fail_next_accept_transport();

    let book = Arc::new(AcceptedBook::in_memory());
    let node = Node::new(
        operator,
        profile,
        coordinator.clone(),
        Arc::new(EchoExecutor),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    )
    .with_accepted_book(book.clone());

    // The run errors because the accept ack never confirmed, but the job
    // was durably booked FIRST — so recovery re-serves it instead of
    // eating a deadline fault for a job the coordinator may still hold
    // this operator to.
    assert!(
        node.run_once().await.is_err(),
        "a failed accept ack is an error"
    );
    let booked = book.pending();
    assert_eq!(
        booked.len(),
        1,
        "the job survives a transport-failed accept"
    );
    assert_eq!(booked[0].job_id, job_id);
    assert!(
        coordinator.results().is_empty(),
        "nothing executed on this life"
    );
}

#[tokio::test]
async fn a_rejected_accept_drops_the_booked_job() {
    use covenant_compute_node::AcceptedBook;

    let buyer_identity = LocalIdentity::generate("buyer@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let coordinator_pubkey_b58 = bs58::encode(coordinator_identity.pubkey_bytes()).into_string();
    let operator = LocalIdentity::generate("operator@local");
    let profile = cpu_profile_for(&operator);
    let job_id = Uuid::new_v4();

    let coordinator = Arc::new(MockCoordinator::new());
    coordinator.push_offer(offer_for(
        job_id,
        "accept-reject",
        &buyer_identity,
        &coordinator_identity,
    ));
    // A landed rejection is the coordinator's final word — the job was
    // reassigned or cancelled in the offer->accept gap.
    coordinator.fail_next_accept_rejected();

    let book = Arc::new(AcceptedBook::in_memory());
    let node = Node::new(
        operator,
        profile,
        coordinator.clone(),
        Arc::new(EchoExecutor),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    )
    .with_accepted_book(book.clone());

    assert!(
        node.run_once().await.is_err(),
        "a rejected accept is an error"
    );
    assert!(
        book.pending().is_empty(),
        "a job the coordinator refused is dropped, not left for a wasted recovery"
    );
    assert!(coordinator.results().is_empty());
}

#[tokio::test]
async fn a_recovered_job_already_credited_settles_instead_of_re_serving() {
    use covenant_compute_node::{AcceptedBook, EarningsEntry, EarningsStatus};

    // A prior life credited this job's earnings but crashed before it
    // tombstoned the accepted book. Recovery reads the durable credit and
    // settles the book without re-executing: re-running an already-paid
    // job only burns compute for a result the coordinator refuses.
    let operator_identity = LocalIdentity::generate("operator@local");
    let buyer_identity = LocalIdentity::generate("buyer@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let coordinator_pubkey_b58 = bs58::encode(coordinator_identity.pubkey_bytes()).into_string();

    let job_id = Uuid::new_v4();
    let book = Arc::new(AcceptedBook::in_memory());
    book.book(accepted_entry_from(
        offer_for(
            job_id,
            "integration-recover-already-credited",
            &buyer_identity,
            &coordinator_identity,
        ),
        1,
    ))
    .unwrap();

    let earnings = Arc::new(InMemoryEarningsLedger::new());
    earnings
        .credit(EarningsEntry {
            job_id,
            amount_micro_usdc: 5_000,
            fee_micro_usdc: 0,
            funding_source: FundingSource::Organic,
            status: EarningsStatus::Unpaid,
            earned_at_ms: epoch_ms(),
            paid_tx_signature: None,
            paid_at_ms: None,
            receipt_signature_b58: None,
        })
        .await
        .unwrap();

    let profile = cpu_profile_for(&operator_identity);
    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let coordinator = Arc::new(MockCoordinator::new());
    let node = Node::new(
        operator_identity,
        profile,
        coordinator.clone(),
        Arc::new(CountingExecutor(executions.clone())),
        earnings.clone(),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    )
    .with_accepted_book(book.clone());

    assert_eq!(
        node.recover_accepted().await,
        0,
        "an already-credited job is skipped, not re-served"
    );
    assert_eq!(
        executions.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the credit is durable proof the work is done; never re-execute"
    );
    assert!(
        book.pending().is_empty(),
        "an already-paid job is tombstoned, not left to re-serve"
    );
    assert_eq!(
        earnings.recent(10).await.len(),
        1,
        "still exactly one credit; recovery never double-books"
    );
    assert_eq!(earnings.unpaid_total_micro_usdc().await, 5_000);
    assert!(coordinator.results().is_empty(), "nothing was re-submitted");
}

#[tokio::test]
async fn a_recovered_job_past_its_deadline_burns_no_compute() {
    use covenant_compute_node::AcceptedBook;

    let operator_identity = LocalIdentity::generate("operator@local");
    let buyer_identity = LocalIdentity::generate("buyer@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let coordinator_pubkey_b58 = bs58::encode(coordinator_identity.pubkey_bytes()).into_string();

    let book = Arc::new(AcceptedBook::in_memory());
    book.book(accepted_entry_from(
        offer_for_at(
            Uuid::new_v4(),
            "integration-recover-expired",
            &buyer_identity,
            &coordinator_identity,
            epoch_ms().saturating_sub(60_000),
            30_000,
        ),
        1,
    ))
    .unwrap();

    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let coordinator = Arc::new(MockCoordinator::new());
    let earnings = Arc::new(InMemoryEarningsLedger::new());
    let node = Node::new(
        operator_identity,
        cpu_profile_for(&LocalIdentity::generate("profile@local")),
        coordinator.clone(),
        Arc::new(CountingExecutor(executions.clone())),
        earnings.clone(),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    )
    .with_accepted_book(book.clone());

    assert_eq!(node.recover_accepted().await, 0);
    assert_eq!(
        executions.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "an expired job can never be paid; running it burns compute for nothing"
    );
    assert!(coordinator.results().is_empty());
    assert!(book.pending().is_empty(), "the expired job is dropped");
    assert_eq!(earnings.unpaid_total_micro_usdc().await, 0);
}

#[tokio::test]
async fn a_job_that_kills_the_node_twice_is_dropped_as_poison() {
    use covenant_compute_node::AcceptedBook;

    let operator_identity = LocalIdentity::generate("operator@local");
    let buyer_identity = LocalIdentity::generate("buyer@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let coordinator_pubkey_b58 = bs58::encode(coordinator_identity.pubkey_bytes()).into_string();

    // lives == 2: the accepting life and one recovery both died.
    let book = Arc::new(AcceptedBook::in_memory());
    book.book(accepted_entry_from(
        offer_for(
            Uuid::new_v4(),
            "integration-recover-poison",
            &buyer_identity,
            &coordinator_identity,
        ),
        2,
    ))
    .unwrap();

    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let coordinator = Arc::new(MockCoordinator::new());
    let node = Node::new(
        operator_identity,
        cpu_profile_for(&LocalIdentity::generate("profile@local")),
        coordinator.clone(),
        Arc::new(CountingExecutor(executions.clone())),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    )
    .with_accepted_book(book.clone());

    assert_eq!(node.recover_accepted().await, 0);
    assert_eq!(
        executions.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a third life must not re-run the job that killed the first two"
    );
    assert!(book.pending().is_empty(), "the poison job is dropped");
}

#[tokio::test]
async fn a_recovered_job_whose_result_is_already_outboxed_is_not_re_run() {
    use covenant_compute_node::{AcceptedBook, OutboxEntry, ResultOutbox};

    let operator_identity = LocalIdentity::generate("operator@local");
    let buyer_identity = LocalIdentity::generate("buyer@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let coordinator_pubkey_b58 = bs58::encode(coordinator_identity.pubkey_bytes()).into_string();
    let job_id = Uuid::new_v4();

    // The crash fell between the outbox write and the book's tombstone:
    // both remember the job, and the outbox's signed result is truth.
    let receipt = covenant_compute_protocol::SignedWorkReceipt::sign(
        covenant_compute_protocol::WorkReceiptPayload {
            job_id,
            operator: operator_identity.agent_id(),
            job_hash_hex: "aa".repeat(32),
            result_hash_hex: "bb".repeat(32),
            meter: covenant_compute_protocol::JobMeter {
                wall_ms: 1,
                tokens_in: None,
                tokens_out: None,
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc: 5_000,
            status: A2ATaskStatus::Ok,
            executed_at_ms: 1,
            node_audit_root_hex: "cc".repeat(32),
        },
        &operator_identity,
    )
    .unwrap();
    let outbox = Arc::new(ResultOutbox::in_memory());
    outbox
        .enqueue(OutboxEntry {
            job_id,
            message: covenant_compute_protocol::JobResultMessage {
                receipt,
                output: vec![Content::text("already executed")],
            },
            funding_source: FundingSource::Organic,
            queued_at_ms: epoch_ms(),
            settled: false,
        })
        .unwrap();
    let book = Arc::new(AcceptedBook::in_memory());
    book.book(accepted_entry_from(
        offer_for(
            job_id,
            "integration-recover-outboxed",
            &buyer_identity,
            &coordinator_identity,
        ),
        1,
    ))
    .unwrap();

    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let coordinator = Arc::new(MockCoordinator::new());
    let node = Node::new(
        operator_identity,
        cpu_profile_for(&LocalIdentity::generate("profile@local")),
        coordinator.clone(),
        Arc::new(CountingExecutor(executions.clone())),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    )
    .with_outbox(outbox.clone())
    .with_accepted_book(book.clone());

    assert_eq!(node.recover_accepted().await, 0);
    assert_eq!(
        executions.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the work is already done; redelivery owns it"
    );
    assert!(book.pending().is_empty());
    assert_eq!(
        outbox.pending().len(),
        1,
        "the queued result still awaits the drain"
    );
}

#[tokio::test]
async fn a_job_drained_and_credited_this_boot_is_not_re_executed_by_recovery() {
    use covenant_compute_node::{AcceptedBook, OutboxEntry, ResultOutbox};

    // The exact boot ordering: a job crashed with its signed result in the
    // outbox AND still in the accepted book. The serve loop drains the
    // outbox first — delivering, crediting, and dequeuing the result — then
    // runs recovery. Recovery must read the durable credit and skip the job,
    // not re-execute it just because the outbox entry it would have matched
    // is gone.
    let operator_identity = LocalIdentity::generate("operator@local");
    let buyer_identity = LocalIdentity::generate("buyer@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let coordinator_pubkey_b58 = bs58::encode(coordinator_identity.pubkey_bytes()).into_string();
    let job_id = Uuid::new_v4();

    let receipt = covenant_compute_protocol::SignedWorkReceipt::sign(
        covenant_compute_protocol::WorkReceiptPayload {
            job_id,
            operator: operator_identity.agent_id(),
            job_hash_hex: "aa".repeat(32),
            result_hash_hex: "bb".repeat(32),
            meter: covenant_compute_protocol::JobMeter {
                wall_ms: 1,
                tokens_in: None,
                tokens_out: None,
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc: 5_000,
            status: A2ATaskStatus::Ok,
            executed_at_ms: 1,
            node_audit_root_hex: "cc".repeat(32),
        },
        &operator_identity,
    )
    .unwrap();
    let outbox = Arc::new(ResultOutbox::in_memory());
    outbox
        .enqueue(OutboxEntry {
            job_id,
            message: covenant_compute_protocol::JobResultMessage {
                receipt,
                output: vec![Content::text("already executed")],
            },
            funding_source: FundingSource::Organic,
            queued_at_ms: epoch_ms(),
            settled: false,
        })
        .unwrap();
    let book = Arc::new(AcceptedBook::in_memory());
    book.book(accepted_entry_from(
        offer_for(
            job_id,
            "integration-drain-then-recover",
            &buyer_identity,
            &coordinator_identity,
        ),
        1,
    ))
    .unwrap();

    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let earnings = Arc::new(InMemoryEarningsLedger::new());
    let coordinator = Arc::new(MockCoordinator::new());
    let node = Node::new(
        operator_identity,
        cpu_profile_for(&LocalIdentity::generate("profile@local")),
        coordinator.clone(),
        Arc::new(CountingExecutor(executions.clone())),
        earnings.clone(),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    )
    .with_outbox(outbox.clone())
    .with_accepted_book(book.clone());

    assert_eq!(node.drain_outbox().await, 1, "the queued result delivers");
    assert!(outbox.pending().is_empty(), "the drain dequeued it");
    assert_eq!(earnings.recent(10).await.len(), 1, "the drain credited it");

    assert_eq!(
        node.recover_accepted().await,
        0,
        "the credited job is skipped, not re-executed"
    );
    assert_eq!(
        executions.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "recovery must not re-run a job the drain already delivered"
    );
    assert!(book.pending().is_empty(), "the booked job is tombstoned");
    assert_eq!(
        earnings.recent(10).await.len(),
        1,
        "still exactly one credit; no double-book"
    );
}

/// Forwards to the mock but refuses every submitted result — the
/// coordinator that already answered this job in a previous life.
struct RefusingCoordinator(MockCoordinator);

#[async_trait::async_trait]
impl covenant_compute_node::Coordinator for RefusingCoordinator {
    async fn register(
        &self,
        req: covenant_compute_protocol::RegisterRequest,
    ) -> Result<covenant_compute_protocol::RegisterResponse, covenant_compute_node::CoordinatorError>
    {
        self.0.register(req).await
    }
    async fn heartbeat(
        &self,
        req: covenant_compute_protocol::HeartbeatRequest,
    ) -> Result<covenant_compute_protocol::HeartbeatResponse, covenant_compute_node::CoordinatorError>
    {
        self.0.heartbeat(req).await
    }
    async fn poll_next_job(
        &self,
        operator: &covenant_types::AgentId,
    ) -> Result<Option<JobOffer>, covenant_compute_node::CoordinatorError> {
        self.0.poll_next_job(operator).await
    }
    async fn accept_job(
        &self,
        decision: covenant_compute_protocol::JobAccept,
    ) -> Result<(), covenant_compute_node::CoordinatorError> {
        self.0.accept_job(decision).await
    }
    async fn submit_result(
        &self,
        _result: covenant_compute_protocol::JobResultMessage,
    ) -> Result<covenant_compute_protocol::JobResultAck, covenant_compute_node::CoordinatorError>
    {
        Err(covenant_compute_node::CoordinatorError::Protocol(
            "job already settled".into(),
        ))
    }
    async fn push_stream(
        &self,
        push: covenant_compute_protocol::StreamPush,
    ) -> Result<(), covenant_compute_node::CoordinatorError> {
        self.0.push_stream(push).await
    }
}

#[tokio::test]
async fn a_re_served_job_the_coordinator_already_answered_settles_clean() {
    use covenant_compute_node::AcceptedBook;

    let operator_identity = LocalIdentity::generate("operator@local");
    let buyer_identity = LocalIdentity::generate("buyer@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let coordinator_pubkey_b58 = bs58::encode(coordinator_identity.pubkey_bytes()).into_string();

    // The first life's ack landed but the tombstone didn't: recovery
    // re-runs the job, the coordinator refuses the duplicate, and the
    // book must settle on that answer with nothing credited twice.
    let book = Arc::new(AcceptedBook::in_memory());
    book.book(accepted_entry_from(
        offer_for(
            Uuid::new_v4(),
            "integration-recover-answered",
            &buyer_identity,
            &coordinator_identity,
        ),
        1,
    ))
    .unwrap();

    let earnings = Arc::new(InMemoryEarningsLedger::new());
    let node = Node::new(
        operator_identity,
        cpu_profile_for(&LocalIdentity::generate("profile@local")),
        Arc::new(RefusingCoordinator(MockCoordinator::new())),
        Arc::new(EchoExecutor),
        earnings.clone(),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    )
    .with_accepted_book(book.clone());

    assert_eq!(node.recover_accepted().await, 0);
    assert!(
        book.pending().is_empty(),
        "an answered job has nothing left to re-serve"
    );
    assert_eq!(
        earnings.unpaid_total_micro_usdc().await,
        0,
        "a refused duplicate must not credit"
    );
}

#[tokio::test]
async fn a_dead_backend_pauses_intake_reports_offline_and_recovers() {
    use covenant_compute_node::{ExecutionOutcome, ExecutorError, JobExecutor};
    use covenant_compute_protocol::OperatorStatus;
    use std::sync::atomic::{AtomicBool, Ordering};

    // A model server as the serve loop sees it: down until the operator
    // brings it back.
    struct FlakyBackend {
        healthy: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl JobExecutor for FlakyBackend {
        async fn execute(
            &self,
            job: &JobEnvelopePayload,
            deadline: Duration,
        ) -> Result<ExecutionOutcome, ExecutorError> {
            EchoExecutor.execute(job, deadline).await
        }

        async fn health(&self) -> Result<(), ExecutorError> {
            if self.healthy.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err(ExecutorError::Failed("model server unreachable".into()))
            }
        }
    }

    let operator_identity = LocalIdentity::generate("operator@local");
    let buyer_identity = LocalIdentity::generate("buyer@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let coordinator_pubkey_b58 = bs58::encode(coordinator_identity.pubkey_bytes()).into_string();
    let profile = cpu_profile_for(&operator_identity);

    let healthy = Arc::new(AtomicBool::new(false));
    let coordinator = Arc::new(MockCoordinator::new());
    let job_id = Uuid::new_v4();
    coordinator.push_offer(offer_for(
        job_id,
        "integration-backend-gate",
        &buyer_identity,
        &coordinator_identity,
    ));

    let node = Arc::new(Node::new(
        operator_identity,
        profile,
        coordinator.clone(),
        Arc::new(FlakyBackend {
            healthy: healthy.clone(),
        }),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 1,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    ));

    // While the backend is down the gate holds: the queued offer stays
    // untaken (no accept, no fault-bound execution), the node declares
    // Offline, and the transition is announced ONCE, not per probe.
    let gate = {
        let node = node.clone();
        tokio::spawn(async move { node.wait_for_backend(Duration::from_millis(20)).await })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        !gate.is_finished(),
        "the gate must hold while the backend is down"
    );
    assert!(
        coordinator.accepts().is_empty(),
        "no job may be taken during the outage"
    );
    assert!(!node.backend_up());
    assert_eq!(node.current_status(), OperatorStatus::Offline);
    let beats = coordinator.heartbeats();
    assert_eq!(beats.len(), 1, "one transition beat, not one per probe");
    assert_eq!(beats[0].status, OperatorStatus::Offline);

    // Recovery: the gate returns, announces Online, and the offer that
    // waited out the outage serves through the normal paid path.
    healthy.store(true, Ordering::SeqCst);
    gate.await.expect("gate task completes");
    assert!(node.backend_up());
    let beats = coordinator.heartbeats();
    assert_eq!(beats.len(), 2, "down + up transitions, nothing else");
    assert_eq!(beats[1].status, OperatorStatus::Online);
    assert_eq!(node.current_status(), OperatorStatus::Online);

    let outcome = node
        .run_once()
        .await
        .expect("serves after recovery")
        .expect("the offer waited for the backend");
    assert_eq!(outcome.job_id, job_id);
    assert_eq!(coordinator.results().len(), 1);
}

#[tokio::test]
async fn a_dead_backend_reports_offline_even_at_capacity() {
    use covenant_compute_node::{ExecutionOutcome, ExecutorError, JobExecutor};
    use covenant_compute_protocol::OperatorStatus;

    struct DeadBackend;

    #[async_trait::async_trait]
    impl JobExecutor for DeadBackend {
        async fn execute(
            &self,
            _job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<ExecutionOutcome, ExecutorError> {
            unreachable!("never taken")
        }

        async fn health(&self) -> Result<(), ExecutorError> {
            Err(ExecutorError::Failed("down".into()))
        }
    }

    let operator_identity = LocalIdentity::generate("operator@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let profile = cpu_profile_for(&operator_identity);
    let node = Arc::new(Node::new(
        operator_identity,
        profile,
        Arc::new(MockCoordinator::new()),
        Arc::new(DeadBackend),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58: bs58::encode(coordinator_identity.pubkey_bytes()).into_string(),
            // Zero capacity: a healthy node would say Busy. The dead
            // backend must still win — Busy nodes are merely skipped by
            // the matcher today, but only Offline states the truth.
            max_in_flight: 0,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    ));

    assert_eq!(
        node.current_status(),
        OperatorStatus::Busy,
        "healthy + full = busy"
    );
    let gate = {
        let node = node.clone();
        tokio::spawn(async move { node.wait_for_backend(Duration::from_millis(20)).await })
    };
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        node.current_status(),
        OperatorStatus::Offline,
        "a dead backend outranks capacity"
    );
    gate.abort();
    let _ = gate.await;
}

#[tokio::test]
async fn a_draining_node_finishes_its_job_and_takes_nothing_new() {
    use covenant_compute_node::{ExecutionOutcome, ExecutorError, JobExecutor};
    use covenant_compute_protocol::OperatorStatus;

    // Slow enough that the drain lands mid-execution.
    struct SlowEcho;

    #[async_trait::async_trait]
    impl JobExecutor for SlowEcho {
        async fn execute(
            &self,
            job: &JobEnvelopePayload,
            deadline: Duration,
        ) -> Result<ExecutionOutcome, ExecutorError> {
            tokio::time::sleep(Duration::from_millis(200)).await;
            EchoExecutor.execute(job, deadline).await
        }
    }

    let operator_identity = LocalIdentity::generate("operator@local");
    let buyer_identity = LocalIdentity::generate("buyer@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let profile = cpu_profile_for(&operator_identity);

    let coordinator = Arc::new(MockCoordinator::new());
    let in_flight_job = Uuid::new_v4();
    coordinator.push_offer(offer_for(
        in_flight_job,
        "integration-drain-1",
        &buyer_identity,
        &coordinator_identity,
    ));
    coordinator.push_offer(offer_for(
        Uuid::new_v4(),
        "integration-drain-2",
        &buyer_identity,
        &coordinator_identity,
    ));

    let node = Arc::new(Node::new(
        operator_identity,
        profile,
        coordinator.clone(),
        Arc::new(SlowEcho),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58: bs58::encode(coordinator_identity.pubkey_bytes()).into_string(),
            max_in_flight: 1,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    ));

    // The first job is mid-execution when the drain lands: it must run
    // to completion and submit — a drain abandons nothing it holds.
    let serving = {
        let node = node.clone();
        tokio::spawn(async move { node.run_once().await })
    };
    tokio::time::sleep(Duration::from_millis(60)).await;
    node.begin_drain().await;
    assert_eq!(node.current_status(), OperatorStatus::Offline);

    let outcome = serving
        .await
        .expect("serve task")
        .expect("run_once")
        .expect("the in-flight job finishes through the drain");
    assert_eq!(outcome.job_id, in_flight_job);
    assert_eq!(coordinator.results().len(), 1, "the result was submitted");

    // Nothing new: the second offer stays with the coordinator for the
    // re-match the Offline beat triggers there.
    assert!(node.run_once().await.expect("run_once").is_none());
    assert_eq!(coordinator.pending_offers(), 1);
    assert_eq!(coordinator.accepts().len(), 1);

    // Idempotent: one drain, one transition beat.
    node.begin_drain().await;
    let beats = coordinator.heartbeats();
    assert_eq!(
        beats.len(),
        1,
        "one Offline beat, however often drain is asked"
    );
    assert_eq!(beats[0].status, OperatorStatus::Offline);
}

#[tokio::test]
async fn a_drain_releases_the_backend_outage_gate() {
    use covenant_compute_node::{ExecutionOutcome, ExecutorError, JobExecutor};

    struct DeadBackend;

    #[async_trait::async_trait]
    impl JobExecutor for DeadBackend {
        async fn execute(
            &self,
            _job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<ExecutionOutcome, ExecutorError> {
            unreachable!("never taken")
        }

        async fn health(&self) -> Result<(), ExecutorError> {
            Err(ExecutorError::Failed("down".into()))
        }
    }

    let operator_identity = LocalIdentity::generate("operator@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let profile = cpu_profile_for(&operator_identity);
    let node = Arc::new(Node::new(
        operator_identity,
        profile,
        Arc::new(MockCoordinator::new()),
        Arc::new(DeadBackend),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58: bs58::encode(coordinator_identity.pubkey_bytes()).into_string(),
            max_in_flight: 1,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    ));

    let gate = {
        let node = node.clone();
        tokio::spawn(async move { node.wait_for_backend(Duration::from_millis(20)).await })
    };
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(
        !gate.is_finished(),
        "the gate holds while the backend is down"
    );

    // A node that is leaving has nothing to wait for: the gate opens
    // without the backend ever recovering, so the serve loop can reach
    // its drain check and exit.
    node.begin_drain().await;
    gate.await.expect("the drain releases the gate");
    assert!(!node.backend_up(), "the backend never recovered");
}

#[tokio::test]
async fn a_drain_interrupts_an_idle_long_poll_instead_of_waiting_it_out() {
    use covenant_compute_protocol::OperatorStatus;

    // A coordinator whose long-poll never answers — the state an idle
    // node is in when its operator hits ctrl-c.
    struct HangingPoll(MockCoordinator);

    #[async_trait::async_trait]
    impl covenant_compute_node::Coordinator for HangingPoll {
        async fn register(
            &self,
            req: covenant_compute_protocol::RegisterRequest,
        ) -> Result<
            covenant_compute_protocol::RegisterResponse,
            covenant_compute_node::CoordinatorError,
        > {
            self.0.register(req).await
        }
        async fn heartbeat(
            &self,
            req: covenant_compute_protocol::HeartbeatRequest,
        ) -> Result<
            covenant_compute_protocol::HeartbeatResponse,
            covenant_compute_node::CoordinatorError,
        > {
            self.0.heartbeat(req).await
        }
        async fn poll_next_job(
            &self,
            _operator: &covenant_types::AgentId,
        ) -> Result<Option<JobOffer>, covenant_compute_node::CoordinatorError> {
            std::future::pending().await
        }
        async fn accept_job(
            &self,
            decision: covenant_compute_protocol::JobAccept,
        ) -> Result<(), covenant_compute_node::CoordinatorError> {
            self.0.accept_job(decision).await
        }
        async fn submit_result(
            &self,
            result: covenant_compute_protocol::JobResultMessage,
        ) -> Result<covenant_compute_protocol::JobResultAck, covenant_compute_node::CoordinatorError>
        {
            self.0.submit_result(result).await
        }
        async fn push_stream(
            &self,
            push: covenant_compute_protocol::StreamPush,
        ) -> Result<(), covenant_compute_node::CoordinatorError> {
            self.0.push_stream(push).await
        }
    }

    let operator_identity = LocalIdentity::generate("operator@local");
    let coordinator_identity = LocalIdentity::generate("coordinator@local");
    let profile = cpu_profile_for(&operator_identity);
    let coordinator = Arc::new(HangingPoll(MockCoordinator::new()));
    let node = Arc::new(Node::new(
        operator_identity,
        profile,
        coordinator.clone(),
        Arc::new(EchoExecutor),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58: bs58::encode(coordinator_identity.pubkey_bytes()).into_string(),
            max_in_flight: 1,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    ));

    // Parked in the poll with no work in sight...
    let polling = {
        let node = node.clone();
        tokio::spawn(async move { node.run_once().await })
    };
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(!polling.is_finished(), "the long-poll is holding");

    // ...the drain wakes it instead of waiting out the poll window.
    node.begin_drain().await;
    let polled = tokio::time::timeout(Duration::from_secs(2), polling)
        .await
        .expect("the drain interrupts the poll promptly")
        .expect("poll task")
        .expect("run_once");
    assert!(polled.is_none(), "an interrupted poll takes no work");
    let beats = coordinator.0.heartbeats();
    assert_eq!(beats.len(), 1);
    assert_eq!(beats[0].status, OperatorStatus::Offline);
}
