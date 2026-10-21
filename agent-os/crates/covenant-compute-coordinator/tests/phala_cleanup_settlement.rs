//! Settlement regressions for the ephemeral Phala result-delivery grace.
//! Backdated accepted records model already-running leases without wall-clock
//! sleeps. The real result/view HTTP routes run on loopback; the recording
//! meter and payout backend never contact a provider, chain, or wallet.

use std::sync::Arc;

use base64::Engine as _;
use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency, A2ATaskStatus};
use covenant_audit::InMemoryAuditLog;
use covenant_compute_coordinator::{
    open_lease_onchain, router, tick_live_leases, CoordinatorConfig, CoordinatorState, JobPhase,
    JobRecord, MockPayout, NoopLeaseMeter,
};
use covenant_compute_protocol::phala::{
    phala_ephemeral_lease_input, phala_ephemeral_workload, reviewed_phala_ephemeral_wrapper,
    PhalaRelayConfig, PHALA_EPHEMERAL_CPU_MODEL, PHALA_EPHEMERAL_RESULT_SLACK_MS,
};
use covenant_compute_protocol::{
    lease_input, CapabilityRequirement, EscrowStatus, FederationEscrow, FundingSource,
    JobEnvelopePayload, JobKind, JobMeter, JobResultAck, JobResultMessage, LeaseTerms, LeaseView,
    MarketplaceFee, RefundReason, ResultSettlement, SignedJobEnvelope, SignedWorkReceipt,
    WorkReceiptPayload,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use uuid::Uuid;

const RATE: u64 = 34;
const DURATION_SECS: u64 = 600;
const CEILING: u64 = RATE * DURATION_SECS;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[derive(Clone, Copy)]
enum Delivery {
    EarlyClose,
    GuestExpiry,
    ResultDeadline,
}

struct Fixture {
    state: CoordinatorState,
    payout: Arc<MockPayout>,
    meter: Arc<NoopLeaseMeter>,
    base_url: String,
    server: tokio::task::JoinHandle<()>,
    job_id: Uuid,
    buyer_key: String,
    result: JobResultMessage,
    billable_ms: u64,
    billing_cutoff_ms: u64,
    result_deadline_ms: u64,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn fixture(delivery: Delivery) -> Fixture {
    let meter = Arc::new(NoopLeaseMeter::new());
    let payout = Arc::new(MockPayout::new());
    let state = CoordinatorState::new(
        LocalIdentity::generate("coordinator@phala-cleanup-test"),
        CoordinatorConfig {
            default_funding_source: FundingSource::Organic,
            // The on-chain lease exemption must survive delayed delivery.
            fee: MarketplaceFee::new(1_000).unwrap(),
            lease_meter: Some(meter.clone()),
            ..CoordinatorConfig::default()
        },
        Arc::new(covenant_compute_coordinator::reputation::NoReputation),
        payout.clone(),
        Arc::new(InMemoryAuditLog::new()),
    );
    let buyer = LocalIdentity::generate("buyer@phala-cleanup-test");
    let operator = LocalIdentity::generate("operator@phala-cleanup-test");
    let job_id = Uuid::new_v4();
    let now = now_ms();
    let issued_at_ms = now
        - match delivery {
            Delivery::EarlyClose => 300_000,
            Delivery::GuestExpiry => 700_000,
            Delivery::ResultDeadline => 1_000_000,
        };
    let accepted_at_ms = issued_at_ms + 10_000;
    let guest_expiry_ms = issued_at_ms + DURATION_SECS * 1_000;
    let close_at_ms = match delivery {
        Delivery::EarlyClose => Some(accepted_at_ms + 20_000),
        _ => None,
    };
    let billing_cutoff_ms = close_at_ms.unwrap_or(guest_expiry_ms);
    let billable_ms = billing_cutoff_ms - accepted_at_ms;
    let mut wire_key = Vec::new();
    wire_key.extend_from_slice(&11u32.to_be_bytes());
    wire_key.extend_from_slice(b"ssh-ed25519");
    wire_key.extend_from_slice(&32u32.to_be_bytes());
    wire_key.extend_from_slice(&[7u8; 32]);
    let client_public_key = format!(
        "ssh-ed25519 {}",
        base64::engine::general_purpose::STANDARD.encode(wire_key)
    );
    let mut payload = JobEnvelopePayload {
        job_id,
        buyer: buyer.agent_id(),
        kind: JobKind::LeaseSession,
        capability_requirement: CapabilityRequirement {
            inference_tariff: None,
            gpu_class: Some("cpu".into()),
            min_vram_gb: None,
            model_id: Some(PHALA_EPHEMERAL_CPU_MODEL.into()),
            kind: JobKind::LeaseSession,
            max_duration_secs: DURATION_SECS as u32,
            min_reputation_bps: None,
        },
        input: vec![lease_input(LeaseTerms {
            max_duration_secs: DURATION_SECS,
            rate_micro_usdc_per_sec: RATE,
            client_public_key: Some(client_public_key),
        })
        .unwrap()],
        price_micro_usdc: CEILING,
        deadline_ms: DURATION_SECS * 1_000 + PHALA_EPHEMERAL_RESULT_SLACK_MS,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, job_id.to_string()),
        issued_at_ms,
        referral_code: None,
        stream: true,
    };
    let (_, spec) = phala_ephemeral_workload(
        &payload,
        &operator.agent_id().pubkey_base58(),
        &reviewed_phala_ephemeral_wrapper(),
        &format!("sha256:{}", "ab".repeat(32)),
        &PhalaRelayConfig {
            url: "https://relay.example.com".into(),
            session_id: job_id.to_string(),
        },
    )
    .unwrap();
    assert_eq!(spec.lease_expires_at_ms, guest_expiry_ms);
    payload
        .input
        .push(phala_ephemeral_lease_input(spec).unwrap());
    payload.validate_input().unwrap();
    let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
    let result_deadline_ms = issued_at_ms + envelope.payload.deadline_ms;
    assert_eq!(PHALA_EPHEMERAL_RESULT_SLACK_MS, 360_000);
    assert!(now - billing_cutoff_ms > 60_000);
    let hold = state
        .escrow()
        .hold(job_id, &buyer.agent_id(), CEILING)
        .await
        .unwrap();
    // Seed the durable accepted-state seam. Admission/attestation are covered
    // separately; no current-time admission can accept an already-expired
    // guest solely to exercise the result handler's historical clocks.
    let record: JobRecord = serde_json::from_value(serde_json::json!({
        "operator_pubkey_b58": operator.agent_id().pubkey_base58(),
        "payout_address": bs58::encode([21u8; 32]).into_string(),
        "envelope": envelope,
        "escrow_hold": hold,
        "phase": "accepted",
        "receipt": null,
        "output": null,
        "accepted_at_ms": accepted_at_ms,
        "close_requested_at_ms": close_at_ms,
    }))
    .unwrap();
    state.jobs().insert(job_id, record.clone()).unwrap();
    open_lease_onchain(&state, job_id).await;
    assert_eq!(meter.opened().len(), 1);
    assert_eq!(meter.opened()[0].funded_micro_usdc(), CEILING);
    assert_eq!(tick_live_leases(&state, now).await, 1);
    assert_eq!(meter.ticks()[0].elapsed_ms, billable_ms);
    let output = vec![Content::text(
        "original VM deletion confirmed; delivery follows cleanup",
    )];
    let receipt = SignedWorkReceipt::sign(
        WorkReceiptPayload {
            job_id,
            operator: operator.agent_id(),
            job_hash_hex: covenant_compute_protocol::output_hash_hex(&envelope.payload.input),
            result_hash_hex: covenant_compute_protocol::output_hash_hex(&output),
            // Deliberately includes the delivery delay: this signed claim
            // must not replace either coordinator-owned billing cutoff.
            meter: JobMeter {
                wall_ms: now - accepted_at_ms,
                tokens_in: None,
                tokens_out: None,
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc: CEILING,
            status: A2ATaskStatus::Ok,
            executed_at_ms: now,
            node_audit_root_hex: "cc".repeat(32),
        },
        &operator,
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = router(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Fixture {
        state,
        payout,
        meter,
        base_url: format!("http://{address}"),
        server,
        job_id,
        buyer_key: buyer.agent_id().pubkey_base58(),
        result: JobResultMessage { receipt, output },
        billable_ms,
        billing_cutoff_ms,
        result_deadline_ms,
    }
}

async fn deliver(fixture: &Fixture) -> JobResultAck {
    let response = reqwest::Client::new()
        .post(format!(
            "{}/federation/jobs/{}/result",
            fixture.base_url, fixture.job_id
        ))
        .json(&fixture.result)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    response.json().await.unwrap()
}

async fn view(fixture: &Fixture) -> LeaseView {
    reqwest::Client::new()
        .get(format!(
            "{}/federation/jobs/{}/lease",
            fixture.base_url, fixture.job_id
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn assert_delayed_success(delivery: Delivery) {
    let fixture = fixture(delivery).await;
    assert!(now_ms() > fixture.billing_cutoff_ms + 60_000);
    assert!(now_ms() < fixture.result_deadline_ms - 60_000);
    let expected = (RATE * fixture.billable_ms).div_ceil(1_000);
    let before = view(&fixture).await;
    assert_eq!(before.elapsed_ms, fixture.billable_ms);
    assert_eq!(before.charged_micro_usdc, expected);
    let ack = deliver(&fixture).await;
    assert_eq!(ack.settled, ResultSettlement::Released);
    assert_eq!(ack.released_gross_micro_usdc, expected);
    assert_eq!(ack.fee_micro_usdc, Some(0));
    assert_eq!(
        fixture.state.escrow().status(fixture.job_id).await.unwrap(),
        EscrowStatus::Released
    );
    assert_eq!(
        fixture.state.escrow().hold_info(fixture.job_id).unwrap().0,
        expected
    );
    assert_eq!(
        fixture.state.escrow().organic_charged(&fixture.buyer_key),
        expected
    );
    let record = fixture.state.jobs().get(fixture.job_id).unwrap();
    assert_eq!(record.phase, JobPhase::Completed);
    assert_eq!(record.metered_elapsed_ms, Some(fixture.billable_ms));
    assert_eq!(record.fee_micro_usdc, 0);
    let conclusions = fixture.meter.concluded();
    assert_eq!(conclusions.len(), 1);
    assert_eq!(conclusions[0].elapsed_ms, fixture.billable_ms);
    assert_eq!(conclusions[0].charged_micro_usdc, expected);
    assert!(fixture.meter.voided().is_empty());
    // A recording meter makes no transfer, so the ordinary mock payout
    // receives the exact same gross with the lease fee exemption intact.
    assert_eq!(fixture.payout.records().len(), 1);
    assert_eq!(fixture.payout.records()[0].amount_micro_usdc, expected);
    let final_view = view(&fixture).await;
    assert_eq!(final_view.status, "completed");
    assert_eq!(final_view.elapsed_ms, fixture.billable_ms);
    assert_eq!(final_view.charged_micro_usdc, expected);
    assert!(final_view.access.is_none());
    // Redelivery must retain the original meter and pay only once.
    let replay = deliver(&fixture).await;
    assert_eq!(replay.released_gross_micro_usdc, expected);
    assert_eq!(fixture.meter.concluded().len(), 1);
    assert_eq!(fixture.payout.records().len(), 1);
}

#[tokio::test]
async fn ephemeral_cleanup_grace_success_after_early_close_never_bills_grace() {
    assert_delayed_success(Delivery::EarlyClose).await;
}

#[tokio::test]
async fn ephemeral_cleanup_grace_success_after_guest_expiry_never_bills_grace() {
    assert_delayed_success(Delivery::GuestExpiry).await;
}

#[tokio::test]
async fn ephemeral_cleanup_grace_result_after_signed_deadline_refunds_and_voids() {
    let fixture = fixture(Delivery::ResultDeadline).await;
    assert!(now_ms() > fixture.result_deadline_ms);
    let ack = deliver(&fixture).await;
    assert_eq!(ack.settled, ResultSettlement::Refunded);
    assert_eq!(ack.released_gross_micro_usdc, 0);
    assert_eq!(
        fixture.state.escrow().status(fixture.job_id).await.unwrap(),
        EscrowStatus::Refunded
    );
    assert_eq!(
        fixture.state.escrow().organic_charged(&fixture.buyer_key),
        0
    );
    let record = fixture.state.jobs().get(fixture.job_id).unwrap();
    assert_eq!(record.phase, JobPhase::Refunded);
    assert_eq!(record.refund_reason, Some(RefundReason::DeadlineExpired));
    assert!(record.receipt.is_some());
    assert!(fixture.meter.concluded().is_empty());
    assert_eq!(fixture.meter.voided(), vec![fixture.job_id]);
    assert!(fixture.payout.records().is_empty());
    let final_view = view(&fixture).await;
    assert_eq!(final_view.charged_micro_usdc, 0);
    assert_eq!(final_view.elapsed_ms, 0);
    assert!(final_view.access.is_none());
}

#[tokio::test]
async fn ephemeral_cleanup_grace_rejects_new_legacy_order_but_replays_old_terminal_state() {
    let fixture = fixture(Delivery::EarlyClose).await;
    let buyer = LocalIdentity::generate("legacy-buyer@phala-cleanup-test");
    let operator = LocalIdentity::generate("legacy-operator@phala-cleanup-test");
    let job_id = Uuid::new_v4();
    let buyer_key = buyer.agent_id().pubkey_base58();
    let mut historical = fixture.state.jobs().get(fixture.job_id).unwrap();
    let mut payload = historical.envelope.payload.clone();
    payload.job_id = job_id;
    payload.buyer = buyer.agent_id();
    payload.issued_at_ms = now_ms();
    payload.deadline_ms =
        DURATION_SECS * 1_000 + covenant_compute_protocol::LEASE_DEADLINE_SLACK_MS;
    payload.idempotency = A2AIdempotency::new(A2ADuplicateSafety::Idempotent, job_id.to_string());
    payload.input.retain(|content| {
        !matches!(content, Content::Json { value } if value.get("phala_ephemeral_lease").is_some())
    });
    let (_, spec) = phala_ephemeral_workload(
        &payload,
        &operator.agent_id().pubkey_base58(),
        &reviewed_phala_ephemeral_wrapper(),
        &format!("sha256:{}", "ab".repeat(32)),
        &PhalaRelayConfig {
            url: "https://relay.example.com".into(),
            session_id: job_id.to_string(),
        },
    )
    .unwrap();
    payload
        .input
        .push(phala_ephemeral_lease_input(spec).unwrap());
    // Old signatures and workload validation remain valid. Only admission
    // of a new order demands the additional provider-cleanup reserve.
    payload.validate_input().unwrap();
    let legacy = SignedJobEnvelope::sign(payload, &buyer).unwrap();
    legacy.verify().unwrap();
    let http = reqwest::Client::new();
    let url = format!("{}/federation/jobs", fixture.base_url);
    let response = http.post(&url).json(&legacy).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let reason = response.text().await.unwrap();
    assert!(
        reason.contains("300s cleanup and 60s result-delivery slack"),
        "{reason}"
    );
    assert!(fixture.state.jobs().get(job_id).is_none());
    assert!(fixture.state.escrow().hold_info(job_id).is_none());
    assert_eq!(fixture.state.escrow().organic_charged(&buyer_key), 0);
    assert_eq!(
        fixture.meter.opened().len(),
        1,
        "only the unrelated fixture lease exists"
    );
    assert!(fixture.payout.records().is_empty());

    // Model the same envelope restored from the pre-upgrade journal as an
    // already failed/refunded order. A replay must return its actual history,
    // not reinterpret the old deadline or create another hold/allocation.
    historical.envelope = legacy.clone();
    historical.operator_pubkey_b58 = operator.agent_id().pubkey_base58();
    historical.escrow_hold = fixture
        .state
        .escrow()
        .hold(job_id, &buyer.agent_id(), CEILING)
        .await
        .unwrap();
    fixture
        .state
        .escrow()
        .refund(job_id, RefundReason::ExecutionFailed)
        .await
        .unwrap();
    historical.phase = JobPhase::Failed;
    historical.refund_reason = Some(RefundReason::ExecutionFailed);
    historical.accepted_at_ms = Some(legacy.payload.issued_at_ms);
    historical.close_requested_at_ms = Some(legacy.payload.issued_at_ms);
    let output = vec![Content::text("original legacy cleanup failed")];
    let mut receipt = fixture.result.receipt.receipt.clone();
    receipt.job_id = job_id;
    receipt.operator = operator.agent_id();
    receipt.job_hash_hex = covenant_compute_protocol::output_hash_hex(&legacy.payload.input);
    receipt.result_hash_hex = covenant_compute_protocol::output_hash_hex(&output);
    receipt.status = A2ATaskStatus::Error;
    historical.receipt = Some(SignedWorkReceipt::sign(receipt, &operator).unwrap());
    historical.output = Some(output);
    fixture
        .state
        .jobs()
        .insert(job_id, historical.clone())
        .unwrap();
    let before = serde_json::to_value(&historical).unwrap();
    let response = http.post(&url).json(&legacy).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let response: serde_json::Value = response.json().await.unwrap();
    assert_eq!(response["job_id"], job_id.to_string());
    assert_eq!(response["status"], "failed");
    assert_eq!(
        serde_json::to_value(fixture.state.jobs().get(job_id).unwrap()).unwrap(),
        before
    );
    assert_eq!(
        fixture.state.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Refunded
    );
    assert_eq!(fixture.state.escrow().organic_charged(&buyer_key), 0);
    assert_eq!(fixture.meter.opened().len(), 1);
    assert!(fixture.meter.concluded().is_empty());
    assert!(fixture.payout.records().is_empty());
}
