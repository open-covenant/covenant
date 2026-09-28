//! Boot-time reconciliation of the coordinator's two durable books.
//!
//! Every settlement writes two journal lines — the escrow ledger's fund
//! flip and the job book's lifecycle record — as separate appends, so a
//! crash between them leaves the books disagreeing about one job: a
//! hold with no record (died mid-admission), a still-Held hold behind a
//! terminal record (died mid-reject), or a settled hold behind a job
//! still marked in-flight (died mid-settlement). Nothing at runtime
//! heals these: the deadline sweep reads only the job book, escrow
//! settles exactly once, and `submit_result` 404s a job with no record
//! — so a restored coordinator would carry the disagreement forever as
//! stranded buyer funds, a phantom in-flight job pinning the buyer's
//! ceiling, or a release no payout push will ever honor.
//!
//! [`reconcile_books`] runs once inside
//! [`CoordinatorState::with_journal`], after replay and before the
//! listener binds, so no live request can race it. It settles each
//! disagreement the way the interrupted handler would have: a settled
//! hold's verdict is final and the record follows it; a terminal
//! record's verdict stands and its Held hold settles to match; a hold
//! nothing ever recorded can never conclude, so it refunds.

use covenant_audit::AuditKind;
use covenant_compute_protocol::{EscrowStatus, FederationEscrow, RefundReason};
use uuid::Uuid;

use crate::http::funding_source_str;
use crate::jobs::{JobPhase, JobRecord};
use crate::state::CoordinatorState;

/// Reason string audited for a refund whose original cause died with
/// the crash: the fund flip was journaled but the record write (and the
/// audit row that follows it) was not, so whether the sweep or a failed
/// execution minted the refund is unrecoverable. Attributed like every
/// in-flight refund — each path that could have minted it faults the
/// assigned operator.
pub const RECOVERED_REFUND_REASON: &str = "crash_recovered";

/// What one reconciliation pass settled. All zero on every boot that
/// follows a clean shutdown — a non-clean report is worth an operator's
/// glance, so `with_journal` logs it at warn.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Held holds with no job record, refunded: the coordinator died
    /// between the hold and the record, mid-admission.
    pub orphan_holds_refunded: usize,
    /// Held holds behind a terminal record, settled to the record's
    /// verdict: died between the verdict and its fund flip.
    pub stale_holds_settled: usize,
    /// In-flight records behind a settled hold, concluded to the fund
    /// verdict: died between the flip and the record write.
    pub records_concluded: usize,
}

impl ReconcileReport {
    pub fn is_clean(&self) -> bool {
        *self == Self::default()
    }
}

/// Settles every crash-window disagreement between the escrow ledger
/// and the job book. Boot-only by design: it assumes no request is in
/// flight, which is exactly the window between journal replay and the
/// listener binding. Each item is independent and best-effort — one
/// that cannot be made durable is logged and left for the next boot,
/// which will classify it identically.
pub async fn reconcile_books(state: &CoordinatorState) -> ReconcileReport {
    let mut report = ReconcileReport::default();
    for (job_id, hold) in state.escrow().holds_snapshot() {
        match (hold.status, state.jobs().get(job_id)) {
            (EscrowStatus::Held, None) => {
                // Died between the hold and the record. No offer exists
                // under a durable record, no result can ever settle it
                // (`submit_result` 404s), and the sweep reads only the
                // job book — refund, or the funds stay charged to the
                // buyer forever.
                if let Err(e) = state
                    .escrow()
                    .refund(job_id, RefundReason::AdmissionFailed)
                    .await
                {
                    tracing::error!(%job_id, error = %e, "boot reconcile: orphaned hold refund failed");
                    continue;
                }
                state
                    .record_audit(AuditKind::ComputeJobRefunded {
                        job_id,
                        reason: RefundReason::AdmissionFailed.as_str().into(),
                        operator_pubkey_b58: None,
                    })
                    .await;
                report.orphan_holds_refunded += 1;
            }
            (EscrowStatus::Held, Some(record)) => match record.phase {
                // A live in-flight job — the normal restored case; the
                // sweep and the node own it from here.
                JobPhase::Offered | JobPhase::Accepted => {}
                JobPhase::Completed => {
                    if settle_completed_hold(state, job_id, &hold, &record).await {
                        report.stale_holds_settled += 1;
                    }
                }
                JobPhase::Rejected | JobPhase::Refunded | JobPhase::Failed => {
                    // Died between the terminal verdict and its refund
                    // (the reject path writes the record first). The
                    // verdict stands; the money follows it. The record's
                    // own reason wins — the phase-derived fallback covers
                    // journal rows from before the field existed, and a
                    // non-fault refund (a buyer who cancelled, or a lease that
                    // metered zero) must not fault its operator here any more
                    // than it does on the live path.
                    let reason = record.refund_reason.unwrap_or(match record.phase {
                        JobPhase::Rejected => RefundReason::OperatorRejected,
                        JobPhase::Failed => RefundReason::ExecutionFailed,
                        _ => RefundReason::DeadlineExpired,
                    });
                    if let Err(e) = state.escrow().refund(job_id, reason).await {
                        tracing::error!(%job_id, error = %e, "boot reconcile: stale hold refund failed");
                        continue;
                    }
                    let operator_pubkey_b58 = match reason {
                        RefundReason::BuyerCancelled | RefundReason::NoMeteredUsage => None,
                        _ => assignee(&record),
                    };
                    state
                        .record_audit(AuditKind::ComputeJobRefunded {
                            job_id,
                            reason: reason.as_str().into(),
                            operator_pubkey_b58,
                        })
                        .await;
                    report.stale_holds_settled += 1;
                }
            },
            (settled, Some(record))
                if matches!(record.phase, JobPhase::Offered | JobPhase::Accepted) =>
            {
                // Died between the fund flip and the record's
                // conclusion. The settle is one-way and final, so the
                // record follows it: the sweep stops re-finding the
                // job, the buyer's in-flight ceiling frees, and the
                // receipt poll stops promising a conclusion that
                // already happened. A Released job concludes without
                // its receipt (that write died with the crash), so its
                // payout stays un-pushable — `sweep_unpaid` reports it
                // every tick — until the operator re-submits the
                // receipt, which `submit_result` accepts for exactly
                // this shape: it fills the record in and pushes the
                // payout the release already promised.
                let phase = match settled {
                    EscrowStatus::Released => JobPhase::Completed,
                    _ => JobPhase::Refunded,
                };
                if let Err(e) = state.jobs().set_phase(job_id, phase) {
                    tracing::error!(%job_id, error = %e, "boot reconcile: concluding the record failed");
                    continue;
                }
                match settled {
                    EscrowStatus::Released => {
                        state
                            .record_audit(AuditKind::ComputeJobReleased {
                                job_id,
                                operator_pubkey_b58: record.operator_pubkey_b58.clone(),
                                amount_micro_usdc: hold.amount_micro_usdc,
                                funding_source: funding_source_str(hold.funding_source).into(),
                            })
                            .await;
                    }
                    _ => {
                        state
                            .record_audit(AuditKind::ComputeJobRefunded {
                                job_id,
                                reason: RECOVERED_REFUND_REASON.into(),
                                operator_pubkey_b58: assignee(&record),
                            })
                            .await;
                    }
                }
                report.records_concluded += 1;
            }
            // Settled hold behind a terminal record — every cleanly
            // concluded job looks like this. A settled hold with no
            // record only follows a refunded mid-admission failure,
            // where the money story is already closed.
            _ => {}
        }
    }
    report
}

/// A completed record always carries the verified receipt its release
/// re-checks; honor the release the crash swallowed. Unreachable under
/// the current handler ordering (escrow flips before the record
/// concludes) — kept so the invariant this pass restores is total:
/// after reconciliation, a settled hold and a terminal record imply
/// each other.
async fn settle_completed_hold(
    state: &CoordinatorState,
    job_id: Uuid,
    hold: &crate::journal::EscrowHoldState,
    record: &JobRecord,
) -> bool {
    let Some(receipt) = record.receipt.as_ref() else {
        tracing::error!(%job_id, "boot reconcile: completed record has no receipt; hold left Held");
        return false;
    };
    if let Err(e) = state.escrow().release(job_id, receipt).await {
        tracing::error!(%job_id, error = %e, "boot reconcile: stale hold release failed");
        return false;
    }
    state
        .record_audit(AuditKind::ComputeJobReleased {
            job_id,
            operator_pubkey_b58: record.operator_pubkey_b58.clone(),
            amount_micro_usdc: hold.amount_micro_usdc,
            funding_source: funding_source_str(hold.funding_source).into(),
        })
        .await;
    true
}

/// The record's assignee, when it ever had one — the empty string is
/// the never-matched placeholder and must not be attributed.
fn assignee(record: &JobRecord) -> Option<String> {
    Some(record.operator_pubkey_b58.clone()).filter(|op| !op.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::JobRecord;
    use crate::payout::MockPayout;
    use crate::reputation::NoReputation;
    use crate::state::{CoordinatorConfig, CoordinatorState};
    use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency, A2ATaskStatus};
    use covenant_audit::{AuditLog as _, InMemoryAuditLog};
    use covenant_compute_protocol::{
        CapabilityRequirement, EscrowHoldAttestation, JobEnvelopePayload, JobKind, JobMeter,
        SignedJobEnvelope, SignedWorkReceipt, WorkReceiptPayload,
    };
    use covenant_identity::LocalIdentity;
    use covenant_mcp::Content;
    use std::sync::Arc;
    use uuid::Uuid;

    fn test_state(config: CoordinatorConfig) -> (CoordinatorState, Arc<InMemoryAuditLog>) {
        let audit = Arc::new(InMemoryAuditLog::new());
        let state = CoordinatorState::new(
            LocalIdentity::generate("coordinator@recover"),
            config,
            Arc::new(NoReputation),
            Arc::new(MockPayout::new()),
            audit.clone(),
        );
        (state, audit)
    }

    fn record_for(job_id: Uuid, buyer: &LocalIdentity, phase: JobPhase) -> JobRecord {
        let payload = JobEnvelopePayload {
            job_id,
            buyer: buyer.agent_id(),
            kind: JobKind::BatchJob,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: None,
                kind: JobKind::BatchJob,
                max_duration_secs: 5,
                min_reputation_bps: None,
            },
            input: vec![Content::text("work")],
            price_micro_usdc: 100,
            deadline_ms: 60_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "recover-test"),
            issued_at_ms: crate::epoch_ms(),
            referral_code: None,
            stream: false,
        };
        let envelope = SignedJobEnvelope::sign(payload, buyer).unwrap();
        let escrow_hold = EscrowHoldAttestation::sign(
            job_id,
            100,
            covenant_compute_protocol::FundingSource::Organic,
            crate::epoch_ms(),
            &LocalIdentity::generate("attestor@recover"),
        )
        .unwrap();
        JobRecord {
            operator_pubkey_b58: "operator-pubkey".into(),
            payout_address: "operator-payout".into(),
            envelope,
            escrow_hold,
            phase,
            receipt: None,
            output: None,
            fee_micro_usdc: 0,
            referral_code: None,
            partner_share_micro_usdc: 0,
            buyer_referral_code: None,
            buyer_partner_share_micro_usdc: 0,
            payout: None,
            concluded_at_ms: None,
            refund_reason: None,
            dispute: None,
            offered_at_ms: 0,
            pinned: false,
            accepted_at_ms: None,
            metered_elapsed_ms: None,
            close_requested_at_ms: None,
            lease_access: None,
        }
    }

    fn ok_receipt(job_id: Uuid, operator: &LocalIdentity) -> SignedWorkReceipt {
        SignedWorkReceipt::sign(
            WorkReceiptPayload {
                job_id,
                operator: operator.agent_id(),
                job_hash_hex: "aa".repeat(32),
                result_hash_hex: "bb".repeat(32),
                meter: JobMeter {
                    wall_ms: 1,
                    tokens_in: None,
                    tokens_out: None,
                    gpu_seconds: None,
                    finish_reason: None,
                },
                price_micro_usdc: 100,
                status: A2ATaskStatus::Ok,
                executed_at_ms: 1,
                node_audit_root_hex: "cc".repeat(32),
            },
            operator,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn an_orphaned_held_hold_refunds_and_frees_the_buyer_balance() {
        let (state, audit) = test_state(CoordinatorConfig {
            require_prefunded_buyers: true,
            ..CoordinatorConfig::default()
        });
        let buyer = LocalIdentity::generate("buyer@recover").agent_id();
        state
            .accounts()
            .credit_deposit("sig-1", &buyer.pubkey_base58(), 5_000)
            .unwrap();
        let job_id = Uuid::new_v4();
        state.escrow().hold(job_id, &buyer, 5_000).await.unwrap();
        // The crash: no job record was ever written.
        assert_eq!(
            state
                .buyer_funds(&buyer.pubkey_base58())
                .available_micro_usdc,
            0
        );

        let report = reconcile_books(&state).await;
        assert_eq!(report.orphan_holds_refunded, 1);
        assert_eq!(
            state.escrow().status(job_id).await.unwrap(),
            EscrowStatus::Refunded
        );
        assert_eq!(
            state
                .buyer_funds(&buyer.pubkey_base58())
                .available_micro_usdc,
            5_000,
            "the orphaned hold must stop charging the buyer"
        );
        let events = audit.recent(10).await.unwrap();
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            AuditKind::ComputeJobRefunded { job_id: id, operator_pubkey_b58: None, reason }
                if *id == job_id && reason == "admission_failed"
        )));

        // The next boot has nothing left to settle.
        assert!(reconcile_books(&state).await.is_clean());
    }

    #[tokio::test]
    async fn a_rejected_record_with_a_held_hold_refunds_attributed() {
        let (state, audit) = test_state(CoordinatorConfig::default());
        let buyer = LocalIdentity::generate("buyer@recover");
        let job_id = Uuid::new_v4();
        state
            .escrow()
            .hold(job_id, &buyer.agent_id(), 100)
            .await
            .unwrap();
        state
            .jobs()
            .insert(job_id, record_for(job_id, &buyer, JobPhase::Rejected))
            .unwrap();

        let report = reconcile_books(&state).await;
        assert_eq!(report.stale_holds_settled, 1);
        assert_eq!(
            state.escrow().status(job_id).await.unwrap(),
            EscrowStatus::Refunded
        );
        let events = audit.recent(10).await.unwrap();
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            AuditKind::ComputeJobRefunded { job_id: id, operator_pubkey_b58: Some(op), reason }
                if *id == job_id && op == "operator-pubkey" && reason == "operator_rejected"
        )));
    }

    /// The cancel handler concludes the record before it refunds the
    /// hold; a crash between the two lands here. The record's own
    /// reason must win over the phase-derived fallback — deriving
    /// `deadline_expired` from the bare `Refunded` phase would fault
    /// an operator whose buyer merely walked away.
    #[tokio::test]
    async fn a_cancelled_record_with_a_held_hold_refunds_without_a_fault() {
        let (state, audit) = test_state(CoordinatorConfig::default());
        let buyer = LocalIdentity::generate("buyer@recover");
        let job_id = Uuid::new_v4();
        state
            .escrow()
            .hold(job_id, &buyer.agent_id(), 100)
            .await
            .unwrap();
        let mut record = record_for(job_id, &buyer, JobPhase::Refunded);
        record.refund_reason = Some(RefundReason::BuyerCancelled);
        state.jobs().insert(job_id, record).unwrap();

        let report = reconcile_books(&state).await;
        assert_eq!(report.stale_holds_settled, 1);
        assert_eq!(
            state.escrow().status(job_id).await.unwrap(),
            EscrowStatus::Refunded
        );
        let events = audit.recent(10).await.unwrap();
        assert!(
            events.iter().any(|e| matches!(
                &e.kind,
                AuditKind::ComputeJobRefunded { job_id: id, operator_pubkey_b58: None, reason }
                    if *id == job_id && reason == "buyer_cancelled"
            )),
            "the boot refund of a cancelled job stays unattributed"
        );
    }

    /// A lease that metered zero refunds whole under `NoMeteredUsage`, a
    /// no-fault reason like `buyer_cancelled`: the coordinator never saw the
    /// session run, so the operator that served it must not be faulted. The
    /// live path concludes the record before it refunds the hold, so a crash
    /// lands here with the reason intact, and the boot refund must stay
    /// unattributed despite the record naming an assignee.
    #[tokio::test]
    async fn a_zero_metered_record_with_a_held_hold_refunds_without_a_fault() {
        let (state, audit) = test_state(CoordinatorConfig::default());
        let buyer = LocalIdentity::generate("buyer@recover");
        let job_id = Uuid::new_v4();
        state
            .escrow()
            .hold(job_id, &buyer.agent_id(), 100)
            .await
            .unwrap();
        let mut record = record_for(job_id, &buyer, JobPhase::Refunded);
        record.refund_reason = Some(RefundReason::NoMeteredUsage);
        state.jobs().insert(job_id, record).unwrap();

        let report = reconcile_books(&state).await;
        assert_eq!(report.stale_holds_settled, 1);
        assert_eq!(
            state.escrow().status(job_id).await.unwrap(),
            EscrowStatus::Refunded
        );
        let events = audit.recent(10).await.unwrap();
        assert!(
            events.iter().any(|e| matches!(
                &e.kind,
                AuditKind::ComputeJobRefunded { job_id: id, operator_pubkey_b58: None, reason }
                    if *id == job_id && reason == "no_metered_usage"
            )),
            "the boot refund of a zero-metered lease stays unattributed"
        );
    }

    #[tokio::test]
    async fn a_completed_record_with_a_held_hold_releases_on_its_receipt() {
        let (state, audit) = test_state(CoordinatorConfig::default());
        let buyer = LocalIdentity::generate("buyer@recover");
        let operator = LocalIdentity::generate("operator@recover");
        let job_id = Uuid::new_v4();
        state
            .escrow()
            .hold(job_id, &buyer.agent_id(), 100)
            .await
            .unwrap();
        let mut record = record_for(job_id, &buyer, JobPhase::Completed);
        record.receipt = Some(ok_receipt(job_id, &operator));
        state.jobs().insert(job_id, record).unwrap();

        let report = reconcile_books(&state).await;
        assert_eq!(report.stale_holds_settled, 1);
        assert_eq!(
            state.escrow().status(job_id).await.unwrap(),
            EscrowStatus::Released,
            "a completed record's verdict stands; its money follows"
        );
        let events = audit.recent(10).await.unwrap();
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            AuditKind::ComputeJobReleased { job_id: id, amount_micro_usdc: 100, .. } if *id == job_id
        )));
    }

    #[tokio::test]
    async fn a_settled_hold_with_an_in_flight_record_concludes_the_record() {
        let (state, audit) = test_state(CoordinatorConfig::default());
        let buyer = LocalIdentity::generate("buyer@recover");
        let operator = LocalIdentity::generate("operator@recover");

        // Released escrow, record still Offered: the completion write
        // died with the crash.
        let paid = Uuid::new_v4();
        state
            .escrow()
            .hold(paid, &buyer.agent_id(), 100)
            .await
            .unwrap();
        state
            .escrow()
            .release(paid, &ok_receipt(paid, &operator))
            .await
            .unwrap();
        state
            .jobs()
            .insert(paid, record_for(paid, &buyer, JobPhase::Offered))
            .unwrap();

        // Refunded escrow, record still Accepted: the sweep's phase
        // write died with the crash.
        let refunded = Uuid::new_v4();
        state
            .escrow()
            .hold(refunded, &buyer.agent_id(), 100)
            .await
            .unwrap();
        state
            .escrow()
            .refund(refunded, RefundReason::DeadlineExpired)
            .await
            .unwrap();
        state
            .jobs()
            .insert(refunded, record_for(refunded, &buyer, JobPhase::Accepted))
            .unwrap();

        let report = reconcile_books(&state).await;
        assert_eq!(report.records_concluded, 2);
        assert_eq!(state.jobs().get(paid).unwrap().phase, JobPhase::Completed);
        assert_eq!(
            state.jobs().get(refunded).unwrap().phase,
            JobPhase::Refunded
        );
        let events = audit.recent(10).await.unwrap();
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            AuditKind::ComputeJobReleased { job_id: id, .. } if *id == paid
        )));
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            AuditKind::ComputeJobRefunded { job_id: id, operator_pubkey_b58: Some(_), reason }
                if *id == refunded && reason == RECOVERED_REFUND_REASON
        )));
    }

    #[tokio::test]
    async fn a_live_job_and_a_cleanly_concluded_job_are_untouched() {
        let (state, audit) = test_state(CoordinatorConfig::default());
        let buyer = LocalIdentity::generate("buyer@recover");
        let operator = LocalIdentity::generate("operator@recover");

        let live = Uuid::new_v4();
        state
            .escrow()
            .hold(live, &buyer.agent_id(), 100)
            .await
            .unwrap();
        state
            .jobs()
            .insert(live, record_for(live, &buyer, JobPhase::Offered))
            .unwrap();

        let done = Uuid::new_v4();
        state
            .escrow()
            .hold(done, &buyer.agent_id(), 100)
            .await
            .unwrap();
        state
            .escrow()
            .release(done, &ok_receipt(done, &operator))
            .await
            .unwrap();
        let mut concluded = record_for(done, &buyer, JobPhase::Completed);
        concluded.receipt = Some(ok_receipt(done, &operator));
        state.jobs().insert(done, concluded).unwrap();

        let report = reconcile_books(&state).await;
        assert!(report.is_clean());
        assert_eq!(
            state.escrow().status(live).await.unwrap(),
            EscrowStatus::Held
        );
        assert_eq!(state.jobs().get(live).unwrap().phase, JobPhase::Offered);
        assert_eq!(state.jobs().get(done).unwrap().phase, JobPhase::Completed);
        assert!(audit.recent(10).await.unwrap().is_empty());
    }
}
