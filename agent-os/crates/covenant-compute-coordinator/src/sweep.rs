//! Deadline-refund sweep: the coordinator's mechanical refund path for
//! a job whose buyer-stated `issued_at_ms + deadline_ms` has passed
//! with no verified receipt (build-notes-phase1-foundation.md §1.6 —
//! refund fires on deadline expiry, never a unilateral discretionary
//! call). Not a full lease-reclaim state machine (design-01 §3's
//! `Leased -> Executing -> Reporting` machinery is still condensed, see
//! build-notes-phase1-coordinator.md) — just the one mechanical check
//! this slice needs proven: still-offered/accepted jobs past their
//! deadline get refunded, not left held forever.

use covenant_audit::AuditKind;
use covenant_compute_protocol::{FederationEscrow, JobOffer, RefundReason};
use uuid::Uuid;

use crate::jobs::JobPhase;
use crate::matcher::select_operator;
use crate::state::CoordinatorState;

/// Refunds every `Offered`/`Accepted` job whose deadline has passed as
/// of `now_ms`. Returns the job ids actually refunded (a job that lost
/// a race — e.g. a result landed a moment earlier — is skipped, not
/// an error: `refund` on an already-settled hold is expected here, not
/// exceptional).
pub async fn sweep_expired(state: &CoordinatorState, now_ms: u64) -> Vec<Uuid> {
    let mut refunded = Vec::new();
    for job_id in state.jobs().expired(now_ms) {
        if state
            .escrow()
            .refund(job_id, RefundReason::DeadlineExpired)
            .await
            .is_err()
        {
            continue;
        }
        // The refund returned the buyer's escrowed window; match it
        // on-chain before the phase write below. An accepted lease that
        // expired has a funded vault the operator could otherwise settle
        // for the seconds it ran, and the void returns it whole to the
        // renter. It runs ahead of `conclude_unpaid` deliberately: a
        // journal error there would `continue` past a void sequenced
        // after it, and the next tick — finding the hold already
        // refunded — skips the job before reaching that void, so a
        // deferred void is a void never fired. Voiding here is idempotent
        // and a no-op for a non-lease job, a lease that never accepted,
        // or a deployment with no chain meter.
        //
        // Expired jobs were Offered/Accepted, so an assignee exists; the
        // filter guards the empty-string placeholder of a record that
        // never matched.
        let record = state.jobs().get(job_id);
        if let Some(record) = &record {
            crate::onchain_meter::void_lease_onchain(state, record).await;
        }
        if state
            .jobs()
            .conclude_unpaid(job_id, JobPhase::Refunded, RefundReason::DeadlineExpired)
            .is_err()
        {
            continue;
        }
        let operator_pubkey_b58 = record
            .as_ref()
            .map(|r| r.operator_pubkey_b58.clone())
            .filter(|op| !op.is_empty());
        state
            .record_audit(AuditKind::ComputeJobRefunded {
                job_id,
                reason: RefundReason::DeadlineExpired.as_str().into(),
                operator_pubkey_b58,
            })
            .await;
        refunded.push(job_id);
    }
    refunded
}

/// Re-matches every job whose offer has sat unaccepted past the
/// deployment's `reoffer_after` window (zero disables) — the routing
/// heal for an assignee that died holding an offer, and for the
/// in-memory delivery queues a coordinator restart drops. Money is
/// untouched: the hold stays held, only the assignment moves.
///
/// Per stale job, the matcher picks whoever it would pick now. A new
/// winner takes over the record — under the same guard the accept
/// path writes through, so a racing accept wins cleanly — its queue
/// gets the offer, the old queue gives its copy back, and a
/// `ComputeJobReoffered` row records the move (no reputation fault:
/// the stall can be the coordinator's own restart, and the deadline
/// sweep still faults whoever holds a job that dies unserved). The
/// same winner means the assignee is still the best fit — redeliver
/// (idempotent) to heal a lost queue and restart the clock. No winner
/// means no live capacity right now: leave the assignment alone, the
/// assignee may yet come back, and the deadline sweep is the backstop.
/// Returns the job ids actually re-pointed at a new operator.
pub async fn sweep_stale_offers(state: &CoordinatorState, now_ms: u64) -> Vec<Uuid> {
    let config = state.config();
    let reoffer_after_ms = config.reoffer_after.as_millis() as u64;
    if reoffer_after_ms == 0 {
        return Vec::new();
    }
    let mut reoffered = Vec::new();
    for (job_id, record) in state.jobs().stale_offered(now_ms, reoffer_after_ms) {
        // A pinned job is a coordinator probe (a redundancy mirror or a
        // canary): it measures one named operator, so it can never be
        // re-pointed at another the way an ordinary stale offer can. The
        // only heal it takes is redelivering to its own operator a queue
        // the coordinator dropped on restart — the same second chance an
        // ordinary job gets, so the deadline sweep faults the probed
        // operator only when it held the offer and let it die, not when
        // the coordinator lost the delivery. A queue that still holds the
        // offer needs nothing; a lost or polled-out one is redelivered,
        // idempotently, to the same operator.
        if record.pinned {
            if !state
                .registry()
                .queue_holds(&record.operator_pubkey_b58, job_id)
            {
                let offer = JobOffer {
                    envelope: record.envelope.clone(),
                    escrow_hold: record.escrow_hold.clone(),
                };
                match state
                    .jobs()
                    .touch_offered(job_id, &record.operator_pubkey_b58, now_ms)
                {
                    Ok(true) => {
                        state.registry().deliver(&record.operator_pubkey_b58, offer);
                    }
                    Ok(false) => {}
                    Err(e) => {
                        tracing::error!(%job_id, error = %e, "stale pinned-offer redeliver failed")
                    }
                }
            }
            continue;
        }
        // The tell that decides the re-match's shape: an assignee whose
        // queue still holds the offer after the whole window looks
        // alive to every matcher gate (registered, recent, cheap) while
        // demonstrably not polling — exclude it, or the same broken
        // winner bounces the job in place until the deadline eats it.
        // A queue without the offer means it was polled out (a decision
        // is in flight, the accept guard's domain) or the queue died
        // with a coordinator restart — there the assignee stays a fair
        // candidate and same-winner redelivery is the heal.
        let assignee_ignoring_it = state
            .registry()
            .queue_holds(&record.operator_pubkey_b58, job_id);
        let Some(winner) = select_operator(
            state.registry(),
            state.reputation(),
            state.bonds(),
            &record.envelope.payload.capability_requirement,
            record.envelope.payload.price_micro_usdc,
            now_ms,
            config.operator_liveness_timeout,
            config.min_operator_score_bps,
            config.bond_floor(),
            assignee_ignoring_it.then_some(record.operator_pubkey_b58.as_str()),
        )
        .await
        else {
            continue;
        };
        let offer = JobOffer {
            envelope: record.envelope.clone(),
            escrow_hold: record.escrow_hold.clone(),
        };
        if winner == record.operator_pubkey_b58 {
            match state.jobs().touch_offered(job_id, &winner, now_ms) {
                Ok(true) => {
                    state.registry().deliver(&winner, offer);
                }
                Ok(false) => {}
                Err(e) => tracing::error!(%job_id, error = %e, "stale-offer touch failed"),
            }
            continue;
        }
        if move_offer(state, job_id, &record, offer, &winner, now_ms).await {
            reoffered.push(job_id);
        }
    }
    reoffered
}

/// Re-points one still-`Offered` job at `winner` and hands over its
/// queued offer — the shared tail of every re-match, whatever decided
/// the move (a stale window or an `Offline` declaration). A racing
/// accept wins cleanly (the reassign guard refuses), the old queue
/// gives its copy back, and a `ComputeJobReoffered` row records the
/// move with no reputation fault. Returns whether the job moved.
async fn move_offer(
    state: &CoordinatorState,
    job_id: Uuid,
    record: &crate::jobs::JobRecord,
    offer: JobOffer,
    winner: &str,
    now_ms: u64,
) -> bool {
    // Same capture the original match did: the terms the winner
    // registered under are the terms its release pays.
    let (payout_address, referral_code) = state
        .registry()
        .record(winner)
        .map(|r| (r.payout_address, r.referral_code))
        .unwrap_or_default();
    match state.jobs().reassign(
        job_id,
        &record.operator_pubkey_b58,
        winner,
        payout_address,
        referral_code,
        now_ms,
    ) {
        Ok(true) => {}
        // The job moved on mid-sweep (an accept or reject landed);
        // nothing to heal.
        Ok(false) => return false,
        Err(e) => {
            tracing::error!(%job_id, error = %e, "re-offer reassign failed");
            return false;
        }
    }
    state.registry().revoke(&record.operator_pubkey_b58, job_id);
    if !state.registry().deliver(winner, offer) {
        // The winner deregistered between the match and this
        // delivery. The record already names it, so the next tick
        // re-matches from there — same posture as a lost queue.
        tracing::warn!(%job_id, "re-offer winner vanished before delivery");
    }
    state
        .record_audit(AuditKind::ComputeJobReoffered {
            job_id,
            from_operator_pubkey_b58: record.operator_pubkey_b58.clone(),
            to_operator_pubkey_b58: winner.to_string(),
        })
        .await;
    tracing::info!(
        %job_id,
        from = %record.operator_pubkey_b58,
        to = %winner,
        "offer re-matched"
    );
    true
}

/// Re-matches every offer still queued to an operator that just
/// declared itself `Offline` — the event-driven twin of
/// [`sweep_stale_offers`], fired from the heartbeat handler so an
/// honest "my backend is down" heals its queued jobs now instead of
/// one stale window later. Only offers the operator's queue still
/// holds move (a polled-out offer has a decision in flight — the
/// accept guard's domain), the declarer is excluded from the re-match
/// (it just said it cannot serve), and nothing faults: honesty about
/// an outage must never cost reputation, or nodes learn to go silent
/// instead. No winner leaves the assignment alone — the node may
/// recover inside the deadline, and the deadline sweep backstops.
/// Disabled alongside the sweep by a zero `reoffer_after`: one switch
/// means "the coordinator never moves assignments on its own".
pub async fn reoffer_offline(
    state: &CoordinatorState,
    operator_pubkey_b58: &str,
    now_ms: u64,
) -> Vec<Uuid> {
    let config = state.config();
    if config.reoffer_after.as_millis() == 0 {
        return Vec::new();
    }
    let mut reoffered = Vec::new();
    for (job_id, record) in state.jobs().offered_to(operator_pubkey_b58, now_ms) {
        if !state.registry().queue_holds(operator_pubkey_b58, job_id) {
            continue;
        }
        let Some(winner) = select_operator(
            state.registry(),
            state.reputation(),
            state.bonds(),
            &record.envelope.payload.capability_requirement,
            record.envelope.payload.price_micro_usdc,
            now_ms,
            config.operator_liveness_timeout,
            config.min_operator_score_bps,
            config.bond_floor(),
            Some(operator_pubkey_b58),
        )
        .await
        else {
            continue;
        };
        let offer = JobOffer {
            envelope: record.envelope.clone(),
            escrow_hold: record.escrow_hold.clone(),
        };
        if move_offer(state, job_id, &record, offer, &winner, now_ms).await {
            reoffered.push(job_id);
        }
    }
    reoffered
}

/// Spawns a background task that calls [`sweep_expired`] every
/// `interval` until the returned handle is aborted or dropped. Not
/// wired into the hermetic e2e test (which drives `sweep_expired`
/// directly for a deterministic assertion) — this is what `main.rs`
/// runs so a real deployment doesn't hold expired funds indefinitely.
pub fn spawn_periodic_sweep(
    state: CoordinatorState,
    interval: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            let refunded = sweep_expired(&state, crate::epoch_ms()).await;
            if !refunded.is_empty() {
                tracing::info!(count = refunded.len(), "deadline-refund sweep");
            }
            // After the refund pass so a job that is both expired and
            // stale refunds instead of bouncing to a new operator the
            // same tick would take it from.
            let reoffered = sweep_stale_offers(&state, crate::epoch_ms()).await;
            if !reoffered.is_empty() {
                tracing::info!(count = reoffered.len(), "stale-offer re-match sweep");
            }
            // Stream buffers are in-memory previews (see `stream.rs`);
            // this tick is their only reclamation, so it rides the
            // sweep every deployment already runs.
            let evicted = state
                .streams()
                .evict_idle(crate::epoch_ms(), crate::stream::STREAM_LINGER_MS);
            if evicted > 0 {
                tracing::debug!(count = evicted, "idle job streams evicted");
            }
        }
    })
}

/// Re-pushes the payout for every `Completed` job whose release no
/// recorded push ever honored — the self-healing path for "escrow
/// released, then the push failed or the coordinator died first",
/// which otherwise stays money owed forever with only a log line to
/// show for it. Safe to re-run: the payout backend is idempotent per
/// job id (a completed payout returns its cached record, a concurrent
/// duplicate is refused), and a success lands on the journaled record
/// through the same [`CoordinatorState::record_payout_pushed`] as the
/// first-chance push. Returns the job ids whose payout landed.
pub async fn sweep_unpaid(state: &CoordinatorState) -> Vec<Uuid> {
    let mut pushed = Vec::new();
    for (job_id, record) in state.jobs().completed_unpaid() {
        // A completed job always carries its verified receipt; a
        // record without one is not retryable, only reportable.
        let Some(receipt) = &record.receipt else {
            tracing::error!(%job_id, "completed job has no receipt; cannot retry its payout");
            continue;
        };
        // The operator is owed exactly what escrow released for the job,
        // minus the pinned fee — the same figure the first-chance push
        // split its net from, so the retry can never pay a different
        // amount than the release intended. For a metered lease that is
        // the seconds it ran, written down onto the hold at settlement,
        // not the window ceiling the buyer escrowed: reading the envelope
        // price would re-push the whole ceiling and overpay every
        // early-closed lease whose first push missed, out of the
        // remainder already refunded to the buyer. The hold is the
        // authoritative, immutable record of what was released, so it is
        // preferred over the job record's own account — equal for a clean
        // settlement, but the hold cannot be re-stamped by a later
        // receipt redelivery the way the record's metered elapsed can.
        let released_gross = state
            .escrow()
            .hold_info(job_id)
            .map(|(amount, _)| amount)
            .unwrap_or_else(|| record.released_gross_micro_usdc());
        let net = released_gross.saturating_sub(record.fee_micro_usdc);
        if net == 0 {
            // Fully fee-consumed: nothing is owed, and the zero-amount
            // guard in the backend would refuse it every tick forever.
            continue;
        }
        // An open transfer bracket means a push is in flight or its
        // outcome is unknown; either way this sweep must not touch it
        // — the suspended case is exactly the cross-restart
        // double-spend window.
        if state.attempts().is_open(job_id) {
            continue;
        }
        match state
            .push_job_payout(
                job_id,
                &record.operator_pubkey_b58,
                &record.payout_address,
                net,
                receipt,
            )
            .await
        {
            Ok(paid) => {
                tracing::info!(
                    %job_id,
                    amount_micro_usdc = paid.amount_micro_usdc,
                    "payout retry landed"
                );
                pushed.push(job_id);
            }
            Err(e) => {
                tracing::warn!(%job_id, error = %e, "payout retry failed; will retry next sweep");
            }
        }
    }
    pushed
}

/// Re-pushes every withdrawal debit no backend transfer ever honored —
/// the same self-healing [`sweep_unpaid`] gives job payouts, for the
/// crash window between a withdrawal's debit and its push. Safe to
/// re-run: the backend is idempotent per withdrawal id, and a success
/// lands through the same [`CoordinatorState::record_withdrawal_pushed`]
/// as the first-chance push. Returns the withdrawal ids whose transfer
/// landed.
pub async fn sweep_unpushed_withdrawals(state: &CoordinatorState) -> Vec<Uuid> {
    let mut pushed = Vec::new();
    for withdrawal in state.withdrawals().unpushed() {
        if state.attempts().is_open(withdrawal.withdrawal_id) {
            continue;
        }
        match state.push_withdrawal(&withdrawal).await {
            Ok(transfer) => {
                tracing::info!(
                    withdrawal_id = %withdrawal.withdrawal_id,
                    amount_micro_usdc = transfer.amount_micro_usdc,
                    "withdrawal retry landed"
                );
                pushed.push(withdrawal.withdrawal_id);
            }
            Err(e) => {
                tracing::warn!(
                    withdrawal_id = %withdrawal.withdrawal_id,
                    error = %e,
                    "withdrawal retry failed; will retry next sweep"
                );
            }
        }
    }
    pushed
}

/// Pushes every matured unbond refund no transfer ever honored — the
/// bond-side [`sweep_unpushed_withdrawals`], and the ONLY pusher: an
/// unbond deliberately gets no first-chance push, the unbonding window
/// is the point. What each refund pays is re-read at push time
/// ([`crate::bond::OperatorBonds::payable`]), so a slash that landed
/// during maturation shrinks the transfer; a request slashed to
/// nothing closes with a zero-payment push and no transfer at all.
/// Safe to re-run: the backend is idempotent per unbond id. Returns
/// the unbond ids that concluded.
pub async fn sweep_matured_unbonds(state: &CoordinatorState) -> Vec<Uuid> {
    let mut concluded = Vec::new();
    for unbond in state.bonds().matured_unpushed(crate::epoch_ms()) {
        match state.push_unbond_refund(&unbond).await {
            Ok(Some(transfer)) => {
                tracing::info!(
                    unbond_id = %unbond.unbond_id,
                    paid_micro_usdc = transfer.amount_micro_usdc,
                    "matured unbond refund landed"
                );
                concluded.push(unbond.unbond_id);
            }
            // Nothing payable (a slashed-to-zero request closed clean),
            // already reserved, or an open bracket — either way this
            // request needed no transfer this tick.
            Ok(None) => {
                if state
                    .bonds()
                    .get_unbond(unbond.unbond_id)
                    .is_some_and(|u| u.pushed.is_some())
                {
                    concluded.push(unbond.unbond_id);
                }
            }
            Err(e) => {
                tracing::warn!(
                    unbond_id = %unbond.unbond_id,
                    error = %e,
                    "unbond refund failed; will retry next sweep"
                );
            }
        }
    }
    concluded
}

/// The [`sweep_unpaid`] + [`sweep_unpushed_withdrawals`] +
/// [`sweep_matured_unbonds`] periodic runner, separate from the refund
/// sweep so payout pushes (which can spawn a signing sidecar) run on
/// their own, slower cadence.
pub fn spawn_periodic_payout_retry(
    state: CoordinatorState,
    interval: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            sweep_unpaid(&state).await;
            sweep_unpushed_withdrawals(&state).await;
            sweep_matured_unbonds(&state).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::JobRecord;
    use crate::payout::MockPayout;
    use crate::reputation::NoReputation;
    use crate::state::CoordinatorConfig;
    use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
    use covenant_audit::InMemoryAuditLog;
    use covenant_compute_protocol::{
        CapabilityProfile, CapabilityRequirement, EscrowHoldAttestation, EscrowStatus,
        HardwareClass, JobEnvelopePayload, JobKind, PriceAsk, PriceUnit, RegisterRequest,
        SignedJobEnvelope,
    };
    use covenant_identity::LocalIdentity;
    use covenant_mcp::Content;
    use std::sync::Arc;

    fn cpu_profile(identity: &LocalIdentity, ask_micro_usdc: u64) -> CapabilityProfile {
        CapabilityProfile {
            operator: identity.agent_id(),
            hardware: HardwareClass::CpuOnly,
            vram_gb: 0,
            models_served: vec!["any".into()],
            job_kinds: vec![JobKind::BatchJob],
            price: PriceAsk {
                unit: PriceUnit::PerJob,
                micro_usdc: ask_micro_usdc,
            },
            tee_capable: false,
        }
    }

    fn payout(seed: u8) -> String {
        bs58::encode([seed; 32]).into_string()
    }

    fn test_state() -> CoordinatorState {
        let identity = LocalIdentity::generate("coordinator@test");
        let audit: Arc<dyn covenant_audit::AuditLog> = Arc::new(InMemoryAuditLog::new());
        CoordinatorState::new(
            identity,
            CoordinatorConfig::default(),
            Arc::new(NoReputation),
            Arc::new(MockPayout::new()),
            audit,
        )
    }

    #[tokio::test]
    async fn matured_unbonds_push_clamped_refunds_and_slashed_out_ones_close_clean() {
        use covenant_audit::AuditLog as _;

        let payout = Arc::new(MockPayout::new());
        let audit = Arc::new(InMemoryAuditLog::new());
        let state = CoordinatorState::new(
            LocalIdentity::generate("coordinator@test"),
            CoordinatorConfig::default(),
            Arc::new(NoReputation),
            payout.clone(),
            audit.clone(),
        );
        state.bonds().credit_post("sig-1", "op-a", 1_000).unwrap();
        let full_exit = crate::bond::UnbondState {
            unbond_id: Uuid::new_v4(),
            operator_pubkey_b58: "op-a".into(),
            recipient_address_b58: "op-wallet".into(),
            amount_micro_usdc: 1_000,
            requested_at_ms: 1,
            matures_at_ms: 0,
            pushed: None,
        };
        state.bonds().request_unbond(full_exit.clone()).unwrap();
        // The fault lands while the request matures.
        state
            .bonds()
            .slash(
                "canary:j1",
                "op-a",
                600,
                Uuid::new_v4(),
                "canary wrong-answer",
                5,
            )
            .unwrap();

        let concluded = sweep_matured_unbonds(&state).await;
        assert_eq!(concluded, vec![full_exit.unbond_id]);
        let pushed = state.bonds().get_unbond(full_exit.unbond_id).unwrap();
        assert_eq!(pushed.pushed.as_ref().unwrap().paid_micro_usdc, 400);
        let transfers = payout.transfers();
        assert_eq!(transfers.len(), 1);
        assert_eq!(transfers[0].amount_micro_usdc, 400);
        assert_eq!(transfers[0].recipient_address, "op-wallet");
        let events = audit.recent(10).await.unwrap();
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            covenant_audit::AuditKind::ComputeBondRefunded {
                requested_micro_usdc: 1_000,
                paid_micro_usdc: 400,
                ..
            }
        )));
        // Nothing owed on the next tick.
        assert!(sweep_matured_unbonds(&state).await.is_empty());

        // A second operator slashed to nothing closes with a
        // zero-payment push and no transfer.
        state.bonds().credit_post("sig-2", "op-b", 500).unwrap();
        let exit = crate::bond::UnbondState {
            unbond_id: Uuid::new_v4(),
            operator_pubkey_b58: "op-b".into(),
            recipient_address_b58: "op-b-wallet".into(),
            amount_micro_usdc: 500,
            requested_at_ms: 1,
            matures_at_ms: 0,
            pushed: None,
        };
        state.bonds().request_unbond(exit.clone()).unwrap();
        state
            .bonds()
            .slash("redundancy:j9", "op-b", 500, Uuid::new_v4(), "minority", 6)
            .unwrap();
        let concluded = sweep_matured_unbonds(&state).await;
        assert_eq!(concluded, vec![exit.unbond_id]);
        let closed = state.bonds().get_unbond(exit.unbond_id).unwrap();
        assert_eq!(closed.pushed.as_ref().unwrap().paid_micro_usdc, 0);
        assert!(closed.pushed.as_ref().unwrap().tx_signature.is_none());
        assert_eq!(
            payout.transfers().len(),
            1,
            "no transfer for a zeroed refund"
        );

        // An immature request is untouched by the sweep.
        state.bonds().credit_post("sig-3", "op-c", 100).unwrap();
        let waiting = crate::bond::UnbondState {
            unbond_id: Uuid::new_v4(),
            operator_pubkey_b58: "op-c".into(),
            recipient_address_b58: "op-c-wallet".into(),
            amount_micro_usdc: 100,
            requested_at_ms: 1,
            matures_at_ms: u64::MAX,
            pushed: None,
        };
        state.bonds().request_unbond(waiting.clone()).unwrap();
        assert!(sweep_matured_unbonds(&state).await.is_empty());
        assert!(state
            .bonds()
            .get_unbond(waiting.unbond_id)
            .unwrap()
            .pushed
            .is_none());
    }

    fn job_record(
        job_id: Uuid,
        issued_at_ms: u64,
        deadline_ms: u64,
        escrow_hold: EscrowHoldAttestation,
    ) -> JobRecord {
        let buyer = LocalIdentity::generate("buyer@test");
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
            deadline_ms,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "sweep-test"),
            issued_at_ms,
            referral_code: None,
            stream: false,
        };
        let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
        JobRecord {
            operator_pubkey_b58: "operator-pubkey".into(),
            payout_address: "operator-payout".into(),
            envelope,
            escrow_hold,
            phase: JobPhase::Offered,
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
            offered_at_ms: issued_at_ms,
            pinned: false,
            accepted_at_ms: None,
            metered_elapsed_ms: None,
            close_requested_at_ms: None,
            lease_access: None,
        }
    }

    #[tokio::test]
    async fn sweep_refunds_only_past_deadline_offered_jobs() {
        let state = test_state();
        let buyer = LocalIdentity::generate("buyer@test").agent_id();

        let past_id = Uuid::new_v4();
        let hold = state.escrow().hold(past_id, &buyer, 100).await.unwrap();
        state
            .jobs()
            .insert(past_id, job_record(past_id, 0, 1_000, hold))
            .unwrap();

        let future_id = Uuid::new_v4();
        let hold = state.escrow().hold(future_id, &buyer, 100).await.unwrap();
        state
            .jobs()
            .insert(future_id, job_record(future_id, 0, 1_000_000, hold))
            .unwrap();

        let refunded = sweep_expired(&state, 2_000).await;
        assert_eq!(refunded, vec![past_id]);
        assert_eq!(
            state.escrow().status(past_id).await.unwrap(),
            EscrowStatus::Refunded
        );
        assert_eq!(
            state.escrow().status(future_id).await.unwrap(),
            EscrowStatus::Held
        );
        assert_eq!(state.jobs().get(past_id).unwrap().phase, JobPhase::Refunded);

        let events = state.audit().recent(10).await.unwrap();
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            AuditKind::ComputeJobRefunded { job_id, .. } if *job_id == past_id
        )));
    }

    #[tokio::test]
    async fn a_stale_pinned_probe_is_redelivered_to_its_own_operator_never_reassigned() {
        // A redundancy mirror (or canary) rides an operator's in-memory
        // queue, which a coordinator restart drops while the durable
        // record keeps naming that operator. The stale-offer sweep must
        // heal the lost queue by redelivering to the same operator, not
        // leave the probe to expire and fault an operator that never
        // received it, and never re-point the measurement at a cheaper one.
        let state = test_state();
        let buyer = LocalIdentity::generate("buyer@test").agent_id();

        let probed = LocalIdentity::generate("probed@op");
        let probed_key = probed.agent_id().pubkey_base58();
        let req = RegisterRequest::sign(cpu_profile(&probed, 100), payout(1), &probed).unwrap();
        state.registry().register(&req, 1_000, None).unwrap();

        // A strictly cheaper operator the matcher would prefer if the
        // sweep ever tried to re-point the probe.
        let cheaper = LocalIdentity::generate("cheaper@op");
        let cheaper_key = cheaper.agent_id().pubkey_base58();
        let req = RegisterRequest::sign(cpu_profile(&cheaper, 1), payout(2), &cheaper).unwrap();
        state.registry().register(&req, 1_000, None).unwrap();

        let job_id = Uuid::new_v4();
        let hold = state.escrow().hold(job_id, &buyer, 100).await.unwrap();
        let mut record = job_record(job_id, 0, 1_000_000, hold);
        record.operator_pubkey_b58 = probed_key.clone();
        record.payout_address = payout(1);
        record.pinned = true;
        state.jobs().insert(job_id, record).unwrap();

        // The restart lost the queue: nothing was delivered afterwards.
        assert!(!state.registry().queue_holds(&probed_key, job_id));

        // Well past the 60s re-offer window, still inside the deadline.
        let reoffered = sweep_stale_offers(&state, 120_000).await;

        assert!(
            reoffered.is_empty(),
            "a same-operator redelivery is not a re-point"
        );
        assert!(
            state.registry().queue_holds(&probed_key, job_id),
            "the lost probe is redelivered to its own operator"
        );
        assert!(
            !state.registry().queue_holds(&cheaper_key, job_id),
            "the cheaper operator never receives the probe"
        );
        let record = state.jobs().get(job_id).unwrap();
        assert_eq!(
            record.operator_pubkey_b58, probed_key,
            "a probe is never re-pointed at another operator, even a cheaper one"
        );
        assert_eq!(
            record.phase,
            JobPhase::Offered,
            "the probe stays live rather than being swept to a fault"
        );
    }

    #[tokio::test]
    async fn an_expired_accepted_lease_is_voided_on_chain_as_it_is_refunded() {
        use covenant_compute_protocol::{lease_input, LeaseTerms};

        // A deadline refund returns the buyer's whole escrowed window; the
        // funded on-chain vault must be voided in the same pass, or the
        // operator could still settle it for the seconds the session ran —
        // paid twice for a lease the buyer got back. The void is sequenced
        // ahead of the phase write precisely because a later tick, finding
        // the hold already refunded, never reaches this job again.
        let meter = Arc::new(crate::onchain_meter::NoopLeaseMeter::new());
        let config = CoordinatorConfig {
            lease_meter: Some(meter.clone()),
            ..Default::default()
        };
        let state = CoordinatorState::new(
            LocalIdentity::generate("coordinator@test"),
            config,
            Arc::new(NoReputation),
            Arc::new(MockPayout::new()),
            Arc::new(InMemoryAuditLog::new()) as Arc<dyn covenant_audit::AuditLog>,
        );

        let buyer = LocalIdentity::generate("buyer@test");
        let terms = LeaseTerms {
            max_duration_secs: 600,
            rate_micro_usdc_per_sec: 100,
            client_public_key: None,
        };
        let ceiling = terms.max_price_micro_usdc().unwrap();

        let job_id = Uuid::new_v4();
        let attestation = state
            .escrow()
            .hold(job_id, &buyer.agent_id(), ceiling)
            .await
            .unwrap();
        let mut record = job_record(job_id, 0, 1_000, attestation);
        let payload = JobEnvelopePayload {
            job_id,
            buyer: buyer.agent_id(),
            kind: JobKind::LeaseSession,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: None,
                kind: JobKind::LeaseSession,
                max_duration_secs: 600,
                min_reputation_bps: None,
            },
            input: vec![lease_input(terms).unwrap()],
            price_micro_usdc: ceiling,
            deadline_ms: 1_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "void-test"),
            issued_at_ms: 0,
            referral_code: None,
            stream: true,
        };
        record.envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
        record.phase = JobPhase::Accepted;
        record.accepted_at_ms = Some(0);
        state.jobs().insert(job_id, record).unwrap();

        // The lease is live on-chain: accepted, delegated, metering.
        crate::onchain_meter::open_lease_onchain(&state, job_id).await;
        assert_eq!(meter.opened().len(), 1, "the lease opened on-chain");

        let refunded = sweep_expired(&state, 2_000).await;

        assert_eq!(refunded, vec![job_id]);
        assert_eq!(
            state.escrow().status(job_id).await.unwrap(),
            EscrowStatus::Refunded,
            "the whole window returns to the buyer off-chain"
        );
        assert_eq!(
            meter.voided(),
            vec![job_id],
            "and the funded vault is voided on-chain in the same pass"
        );
    }

    #[tokio::test]
    async fn sweep_skips_a_job_already_released_in_the_race_window() {
        // submit_result releases the escrow, then updates the job book's
        // phase — a sweep landing in that narrow gap would still see
        // Offered/Accepted. The escrow's own AlreadySettled guard, not
        // the job book's phase, is what must stop a double-settlement.
        use covenant_a2a::A2ATaskStatus;
        use covenant_compute_protocol::{JobMeter, SignedWorkReceipt, WorkReceiptPayload};

        let state = test_state();
        let buyer = LocalIdentity::generate("buyer@test").agent_id();
        let operator = LocalIdentity::generate("operator@test");

        let job_id = Uuid::new_v4();
        let hold = state.escrow().hold(job_id, &buyer, 100).await.unwrap();
        state
            .jobs()
            .insert(job_id, job_record(job_id, 0, 1_000, hold))
            .unwrap();

        let receipt = SignedWorkReceipt::sign(
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
            &operator,
        )
        .unwrap();
        state.escrow().release(job_id, &receipt).await.unwrap();
        // Job book phase deliberately left at Offered — the race window
        // this test targets.

        let refunded = sweep_expired(&state, 2_000).await;
        assert!(
            !refunded.contains(&job_id),
            "an already-released hold must never be refunded, even if the job book hasn't caught up yet"
        );
        assert_eq!(
            state.escrow().status(job_id).await.unwrap(),
            EscrowStatus::Released
        );
    }

    fn signed_receipt(
        job_id: Uuid,
        operator: &LocalIdentity,
        price_micro_usdc: u64,
    ) -> covenant_compute_protocol::SignedWorkReceipt {
        use covenant_a2a::A2ATaskStatus;
        use covenant_compute_protocol::{JobMeter, SignedWorkReceipt, WorkReceiptPayload};
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
                price_micro_usdc,
                status: A2ATaskStatus::Ok,
                executed_at_ms: 1,
                node_audit_root_hex: "cc".repeat(32),
            },
            operator,
        )
        .unwrap()
    }

    /// A completed record whose release was never honored by a push —
    /// the retry sweep's exact worklist shape.
    async fn completed_unpaid_record(state: &CoordinatorState, job_id: Uuid, fee_micro_usdc: u64) {
        let operator = LocalIdentity::generate("operator@test");
        let hold = state
            .escrow()
            .hold(
                job_id,
                &LocalIdentity::generate("buyer@test").agent_id(),
                100,
            )
            .await
            .unwrap();
        let mut record = job_record(job_id, 0, 1_000, hold);
        record.phase = JobPhase::Completed;
        record.receipt = Some(signed_receipt(job_id, &operator, 100));
        record.fee_micro_usdc = fee_micro_usdc;
        state.jobs().insert(job_id, record).unwrap();
    }

    #[tokio::test]
    async fn payout_retry_pushes_owed_money_and_records_it() {
        let identity = LocalIdentity::generate("coordinator@test");
        let audit: Arc<dyn covenant_audit::AuditLog> = Arc::new(InMemoryAuditLog::new());
        let payout = Arc::new(MockPayout::new());
        let state = CoordinatorState::new(
            identity,
            CoordinatorConfig::default(),
            Arc::new(NoReputation),
            payout.clone(),
            audit,
        );
        let job_id = Uuid::new_v4();
        completed_unpaid_record(&state, job_id, 10).await;

        let pushed = sweep_unpaid(&state).await;
        assert_eq!(pushed, vec![job_id]);
        let recorded = state.jobs().get(job_id).unwrap().payout.expect("recorded");
        assert_eq!(
            recorded.amount_micro_usdc, 90,
            "gross 100 minus the pinned fee 10"
        );
        assert_eq!(payout.records().len(), 1);
        assert_eq!(payout.records()[0].amount_micro_usdc, 90);
        let events = state.audit().recent(10).await.unwrap();
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            AuditKind::ComputePayoutPushed { job_id: id, amount_micro_usdc: 90, .. } if *id == job_id
        )));

        // The next sweep finds nothing owed — no double push.
        assert!(sweep_unpaid(&state).await.is_empty());
        assert_eq!(payout.records().len(), 1);
    }

    #[tokio::test]
    async fn payout_retry_skips_fully_fee_consumed_jobs() {
        let state = test_state();
        let job_id = Uuid::new_v4();
        // Fee equals the gross price: nothing is owed the operator.
        completed_unpaid_record(&state, job_id, 100).await;

        assert!(sweep_unpaid(&state).await.is_empty());
        assert!(state.jobs().get(job_id).unwrap().payout.is_none());
    }

    #[tokio::test]
    async fn reconciliation_owes_a_recovered_lease_its_metered_draw_not_the_ceiling() {
        use covenant_compute_protocol::{lease_input, LeaseTerms};

        let state = test_state();
        let buyer = LocalIdentity::generate("buyer@test");
        let operator = LocalIdentity::generate("operator@test");

        // 100 micro/s over a 600s window: escrow holds the 60_000
        // ceiling, the session runs 5s, and release_metered writes the
        // hold down to 500 and refunds the rest.
        let terms = LeaseTerms {
            max_duration_secs: 600,
            rate_micro_usdc_per_sec: 100,
            client_public_key: None,
        };
        let ceiling = terms.max_price_micro_usdc().unwrap();
        let used = 500u64;
        let fee = 20u64;

        let job_id = Uuid::new_v4();
        let attestation = state
            .escrow()
            .hold(job_id, &buyer.agent_id(), ceiling)
            .await
            .unwrap();
        state
            .escrow()
            .release_metered(job_id, &signed_receipt(job_id, &operator, ceiling), used)
            .await
            .unwrap();

        // The crash recovered the record without its receipt, so it
        // carries no metered stamp: released_gross falls back to the
        // ceiling and sweep_unpaid cannot retry it. Only the hold still
        // knows the 500 that actually left escrow.
        let mut record = job_record(job_id, 0, 660_000, attestation);
        let payload = JobEnvelopePayload {
            job_id,
            buyer: buyer.agent_id(),
            kind: JobKind::LeaseSession,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: None,
                kind: JobKind::LeaseSession,
                max_duration_secs: 600,
                min_reputation_bps: None,
            },
            input: vec![lease_input(terms).unwrap()],
            price_micro_usdc: ceiling,
            deadline_ms: 660_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "recon-test"),
            issued_at_ms: 0,
            referral_code: None,
            stream: true,
        };
        record.envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
        record.phase = JobPhase::Completed;
        record.fee_micro_usdc = fee;
        state.jobs().insert(job_id, record).unwrap();

        // The record account reads its owed net back at the ceiling,
        // which would drift the identity by ceiling - used against
        // escrow that only ever moved `used`.
        let (_, naive_outstanding) = state.jobs().payout_totals();
        assert_eq!(naive_outstanding, ceiling - fee);

        // Summed from the hold instead, the books balance exactly.
        assert_eq!(state.reconciliation_drift_micro_usdc(), 0);
    }

    #[tokio::test]
    async fn an_unpushed_withdrawal_debit_is_honored_by_the_sweep() {
        let identity = LocalIdentity::generate("coordinator@test");
        let audit: Arc<dyn covenant_audit::AuditLog> = Arc::new(InMemoryAuditLog::new());
        let payout = Arc::new(MockPayout::new());
        let state = CoordinatorState::new(
            identity,
            CoordinatorConfig::default(),
            Arc::new(NoReputation),
            payout.clone(),
            audit,
        );

        // A debit with no push — the crash window between the books
        // commit and the backend call.
        state
            .accounts()
            .credit_deposit("sig-w", "buyer-w", 1_000)
            .unwrap();
        let withdrawal_id = Uuid::new_v4();
        state
            .escrow()
            .withdraw(
                state.accounts(),
                "buyer-w",
                withdrawal_id,
                "recipient-w",
                900,
            )
            .unwrap();
        assert_eq!(state.withdrawals().unpushed().len(), 1);

        let pushed = sweep_unpushed_withdrawals(&state).await;
        assert_eq!(pushed, vec![withdrawal_id]);
        assert!(state.withdrawals().unpushed().is_empty());
        assert!(state
            .withdrawals()
            .get(withdrawal_id)
            .unwrap()
            .pushed
            .is_some());
        assert_eq!(payout.transfers().len(), 1);
        assert_eq!(payout.transfers()[0].amount_micro_usdc, 900);

        // Nothing left to heal: the next tick pushes nothing.
        assert!(sweep_unpushed_withdrawals(&state).await.is_empty());
        assert_eq!(payout.transfers().len(), 1);
    }

    #[tokio::test]
    async fn a_failing_backend_leaves_the_job_owed_for_the_next_sweep() {
        struct AlwaysFails;
        #[async_trait::async_trait]
        impl crate::payout::Payout for AlwaysFails {
            async fn pay(
                &self,
                _job_id: Uuid,
                _operator_pubkey_b58: &str,
                _payout_address: &str,
                _amount_micro_usdc: u64,
                _receipt: &covenant_compute_protocol::SignedWorkReceipt,
            ) -> Result<crate::payout::PayoutRecord, crate::payout::PayoutError> {
                Err(crate::payout::PayoutError::Backend("rpc down".into()))
            }

            async fn transfer(
                &self,
                _transfer_id: Uuid,
                _recipient_address: &str,
                _amount_micro_usdc: u64,
                _memo: &str,
            ) -> Result<crate::payout::TransferRecord, crate::payout::PayoutError> {
                Err(crate::payout::PayoutError::Backend("rpc down".into()))
            }
        }

        let identity = LocalIdentity::generate("coordinator@test");
        let audit: Arc<dyn covenant_audit::AuditLog> = Arc::new(InMemoryAuditLog::new());
        let state = CoordinatorState::new(
            identity,
            CoordinatorConfig::default(),
            Arc::new(NoReputation),
            Arc::new(AlwaysFails),
            audit,
        );
        let job_id = Uuid::new_v4();
        completed_unpaid_record(&state, job_id, 0).await;

        assert!(sweep_unpaid(&state).await.is_empty());
        assert!(
            state.jobs().get(job_id).unwrap().payout.is_none(),
            "a failed retry must stay on the worklist, never fake a push"
        );
    }

    /// A backend whose every push reports an unknown outcome — the
    /// signer died after the request left the process. Counts calls so
    /// tests can prove nothing ever retries through it.
    struct AlwaysUnresolved {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl AlwaysUnresolved {
        fn new() -> Self {
            Self {
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn unresolved() -> crate::payout::PayoutError {
            crate::payout::PayoutError::Unresolved {
                message: "confirm timed out".into(),
                tx_signature: Some("maybe-live-sig".into()),
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::payout::Payout for AlwaysUnresolved {
        async fn pay(
            &self,
            _job_id: Uuid,
            _operator_pubkey_b58: &str,
            _payout_address: &str,
            _amount_micro_usdc: u64,
            _receipt: &covenant_compute_protocol::SignedWorkReceipt,
        ) -> Result<crate::payout::PayoutRecord, crate::payout::PayoutError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(Self::unresolved())
        }

        async fn transfer(
            &self,
            _transfer_id: Uuid,
            _recipient_address: &str,
            _amount_micro_usdc: u64,
            _memo: &str,
        ) -> Result<crate::payout::TransferRecord, crate::payout::PayoutError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(Self::unresolved())
        }
    }

    #[tokio::test]
    async fn an_unknown_outcome_suspends_the_job_and_no_sweep_ever_retries_it() {
        // The cross-restart double-spend's in-process half: once a push
        // reports an unknown outcome, the money may be live on-chain,
        // and the ONLY thing that may move the obligation again is an
        // explicit reconciliation — never the retry sweep.
        let backend = Arc::new(AlwaysUnresolved::new());
        let state = CoordinatorState::new(
            LocalIdentity::generate("coordinator@test"),
            CoordinatorConfig::default(),
            Arc::new(NoReputation),
            backend.clone(),
            Arc::new(InMemoryAuditLog::new()),
        );
        let job_id = Uuid::new_v4();
        completed_unpaid_record(&state, job_id, 0).await;

        assert!(sweep_unpaid(&state).await.is_empty());
        assert_eq!(backend.calls(), 1);
        let attempt = state.attempts().get(job_id).expect("bracket stays open");
        assert_eq!(attempt.tx_signature.as_deref(), Some("maybe-live-sig"));

        // Every later sweep skips the suspended job without touching
        // the backend.
        assert!(sweep_unpaid(&state).await.is_empty());
        assert!(sweep_unpaid(&state).await.is_empty());
        assert_eq!(backend.calls(), 1, "a suspended push must never retry");

        // Reconciled as landed: the completion is booked from the
        // found signature, no new transfer is spawned, and the sweep
        // finds nothing owed.
        let record = state.jobs().get(job_id).unwrap();
        state
            .record_payout_pushed(
                job_id,
                &record.operator_pubkey_b58,
                &crate::payout::PayoutRecord {
                    job_id,
                    operator_pubkey_b58: record.operator_pubkey_b58.clone(),
                    payout_address: record.payout_address.clone(),
                    amount_micro_usdc: 100,
                    recorded_at_ms: 9,
                    tx_signature: Some("maybe-live-sig".into()),
                },
            )
            .await;
        state
            .attempts()
            .resolve(job_id, Some("maybe-live-sig"))
            .unwrap();
        assert!(sweep_unpaid(&state).await.is_empty());
        assert_eq!(backend.calls(), 1);
    }

    #[tokio::test]
    async fn a_suspended_refund_keeps_its_stake_fenced_from_slash() {
        // The unbond half: while a refund's outcome is unknown, the
        // principal it may already have paid out must be invisible to
        // the slash clamp — otherwise the same stake pays the refund
        // AND the fault.
        let backend = Arc::new(AlwaysUnresolved::new());
        let state = CoordinatorState::new(
            LocalIdentity::generate("coordinator@test"),
            CoordinatorConfig::default(),
            Arc::new(NoReputation),
            backend.clone(),
            Arc::new(InMemoryAuditLog::new()),
        );
        state.bonds().credit_post("sig-1", "op-a", 1_000).unwrap();
        let exit = crate::bond::UnbondState {
            unbond_id: Uuid::new_v4(),
            operator_pubkey_b58: "op-a".into(),
            recipient_address_b58: "op-wallet".into(),
            amount_micro_usdc: 1_000,
            requested_at_ms: 1,
            matures_at_ms: 0,
            pushed: None,
        };
        state.bonds().request_unbond(exit.clone()).unwrap();

        assert!(sweep_matured_unbonds(&state).await.is_empty());
        assert_eq!(backend.calls(), 1);
        assert!(state.attempts().is_open(exit.unbond_id));

        // A fault adjudicated while the refund is in doubt takes
        // nothing — the whole stake is spoken for.
        let outcome = state
            .bonds()
            .slash("canary:j1", "op-a", 600, Uuid::new_v4(), "wrong answer", 5)
            .unwrap();
        assert_eq!(outcome, crate::bond::SlashOutcome::NoStake);

        // The sweep never re-selects the reserved request either.
        assert!(sweep_matured_unbonds(&state).await.is_empty());
        assert_eq!(backend.calls(), 1);

        // Reconciled as landed: the push books, and total outflow
        // never exceeds what was posted.
        state
            .record_bond_refunded(&exit, 1_000, Some("found-sig".into()), 9)
            .await;
        state
            .attempts()
            .resolve(exit.unbond_id, Some("found-sig"))
            .unwrap();
        let status = state.bonds().status("op-a");
        assert_eq!(status.refunded_micro_usdc, 1_000);
        assert_eq!(status.slashed_micro_usdc, 0);
        assert!(status.slashed_micro_usdc + status.refunded_micro_usdc <= status.posted_micro_usdc);
    }

    #[tokio::test]
    async fn a_slash_landing_mid_transfer_cannot_double_draw_the_same_stake() {
        // The live race from the adversarial pass: the refund transfer
        // holds the money mid-air (the sidecar await, with the ledger
        // lock released) while a redundancy fault adjudicates. Without
        // the reservation the slash reads the full stake and 1000 of
        // principal pays out 1500.
        struct BlockingPayout {
            gate: tokio::sync::Semaphore,
            started: tokio::sync::Notify,
        }

        #[async_trait::async_trait]
        impl crate::payout::Payout for BlockingPayout {
            async fn pay(
                &self,
                _job_id: Uuid,
                _operator_pubkey_b58: &str,
                _payout_address: &str,
                _amount_micro_usdc: u64,
                _receipt: &covenant_compute_protocol::SignedWorkReceipt,
            ) -> Result<crate::payout::PayoutRecord, crate::payout::PayoutError> {
                unreachable!("this test only transfers")
            }

            async fn transfer(
                &self,
                transfer_id: Uuid,
                recipient_address: &str,
                amount_micro_usdc: u64,
                _memo: &str,
            ) -> Result<crate::payout::TransferRecord, crate::payout::PayoutError> {
                self.started.notify_one();
                let _permit = self.gate.acquire().await.unwrap();
                Ok(crate::payout::TransferRecord {
                    transfer_id,
                    recipient_address: recipient_address.to_string(),
                    amount_micro_usdc,
                    recorded_at_ms: 7,
                    tx_signature: Some("refund-sig".into()),
                })
            }
        }

        let backend = Arc::new(BlockingPayout {
            gate: tokio::sync::Semaphore::new(0),
            started: tokio::sync::Notify::new(),
        });
        let state = CoordinatorState::new(
            LocalIdentity::generate("coordinator@test"),
            CoordinatorConfig::default(),
            Arc::new(NoReputation),
            backend.clone(),
            Arc::new(InMemoryAuditLog::new()),
        );
        state.bonds().credit_post("sig-1", "op-a", 1_000).unwrap();
        let exit = crate::bond::UnbondState {
            unbond_id: Uuid::new_v4(),
            operator_pubkey_b58: "op-a".into(),
            recipient_address_b58: "op-wallet".into(),
            amount_micro_usdc: 1_000,
            requested_at_ms: 1,
            matures_at_ms: 0,
            pushed: None,
        };
        state.bonds().request_unbond(exit.clone()).unwrap();

        let sweep_state = state.clone();
        let sweep = tokio::spawn(async move { sweep_matured_unbonds(&sweep_state).await });
        backend.started.notified().await;

        // The transfer is mid-air. The fault lands NOW.
        let outcome = state
            .bonds()
            .slash("redundancy:j1", "op-a", 500, Uuid::new_v4(), "minority", 5)
            .unwrap();
        assert_eq!(
            outcome,
            crate::bond::SlashOutcome::NoStake,
            "the in-flight refund owns the whole stake; the slash may take nothing"
        );

        backend.gate.add_permits(1);
        let concluded = sweep.await.unwrap();
        assert_eq!(concluded, vec![exit.unbond_id]);

        let status = state.bonds().status("op-a");
        assert_eq!(status.refunded_micro_usdc, 1_000);
        assert_eq!(status.slashed_micro_usdc, 0);
        assert!(
            status.slashed_micro_usdc + status.refunded_micro_usdc <= status.posted_micro_usdc,
            "1000 of stake must never pay out more than 1000"
        );
    }

    /// Builds a journal-backed state for the restart tests below —
    /// each call is one "life" of the same coordinator data directory.
    async fn durable_state(
        journal_path: &std::path::Path,
        payout: Arc<dyn crate::payout::Payout>,
    ) -> CoordinatorState {
        CoordinatorState::with_journal(
            LocalIdentity::generate("coordinator@restart"),
            CoordinatorConfig::default(),
            Arc::new(NoReputation),
            payout,
            Arc::new(InMemoryAuditLog::new()),
            journal_path,
            None,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn an_open_attempt_bracket_survives_a_restart_and_keeps_the_obligation_suspended() {
        // The cross-restart double-spend itself: the process dies while
        // a transfer's outcome is unknown, and the restarted sweeps —
        // whose backend ledger is rebuilt empty — used to submit a
        // second, distinct transaction. The journaled bracket must keep
        // the obligation frozen across the restart until a human
        // reconciles it.
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("journal.jsonl");

        let backend = Arc::new(AlwaysUnresolved::new());
        let state1 = durable_state(&journal_path, backend.clone()).await;
        let operator = LocalIdentity::generate("operator@test");
        let job_id = Uuid::new_v4();
        let buyer = LocalIdentity::generate("buyer@test").agent_id();
        let hold = state1.escrow().hold(job_id, &buyer, 100).await.unwrap();
        let receipt = signed_receipt(job_id, &operator, 100);
        let mut record = job_record(job_id, 0, 1_000, hold);
        record.phase = JobPhase::Completed;
        record.receipt = Some(receipt.clone());
        state1.jobs().insert(job_id, record).unwrap();
        state1.escrow().release(job_id, &receipt).await.unwrap();

        assert!(sweep_unpaid(&state1).await.is_empty());
        assert_eq!(backend.calls(), 1);
        assert!(state1.attempts().is_open(job_id));
        drop(state1);

        // Life 2: a fresh process with a working backend. The bracket
        // restored from the journal must keep the sweep's hands off.
        let mock = Arc::new(MockPayout::new());
        let state2 = durable_state(&journal_path, mock.clone()).await;
        assert!(
            state2.attempts().is_open(job_id),
            "the crash-window bracket survives the restart"
        );
        assert!(sweep_unpaid(&state2).await.is_empty());
        assert!(sweep_unpaid(&state2).await.is_empty());
        assert!(
            mock.records().is_empty(),
            "no restarted sweep may re-push a transfer whose outcome is unknown"
        );

        // The admin checked the chain: nothing landed. Clearing the
        // bracket frees the sweep, which pays exactly once.
        state2
            .attempts()
            .clear(job_id, "reconciled: not landed")
            .unwrap();
        assert_eq!(sweep_unpaid(&state2).await, vec![job_id]);
        assert_eq!(mock.records().len(), 1);

        // Life 3: nothing resurrects.
        drop(state2);
        let mock3 = Arc::new(MockPayout::new());
        let state3 = durable_state(&journal_path, mock3.clone()).await;
        assert!(!state3.attempts().is_open(job_id));
        assert!(sweep_unpaid(&state3).await.is_empty());
        assert!(mock3.records().is_empty());
    }

    #[tokio::test]
    async fn an_attempt_whose_completion_marker_landed_auto_resolves_at_boot() {
        // The benign half of the crash window: the transfer landed and
        // its completion marker journaled, but the process died before
        // the bracket's own resolve line. Boot must close it from the
        // marker — no operator ceremony for money that is provably
        // where it belongs.
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("journal.jsonl");

        let mock = Arc::new(MockPayout::new());
        let state1 = durable_state(&journal_path, mock.clone()).await;
        let operator = LocalIdentity::generate("operator@test");
        let job_id = Uuid::new_v4();
        let buyer = LocalIdentity::generate("buyer@test").agent_id();
        let hold = state1.escrow().hold(job_id, &buyer, 100).await.unwrap();
        let receipt = signed_receipt(job_id, &operator, 100);
        let mut record = job_record(job_id, 0, 1_000, hold);
        record.phase = JobPhase::Completed;
        record.receipt = Some(receipt.clone());
        state1.jobs().insert(job_id, record).unwrap();
        state1.escrow().release(job_id, &receipt).await.unwrap();
        assert_eq!(sweep_unpaid(&state1).await, vec![job_id]);

        // Re-open the bracket on top of the recorded completion — the
        // exact journal state a crash between the marker and the
        // resolve leaves behind.
        state1
            .journal()
            .unwrap()
            .record_transfer_attempt(&crate::journal::TransferAttemptState {
                attempt_id: job_id,
                kind: crate::journal::TransferAttemptKind::JobPayout,
                amount_micro_usdc: 100,
                recipient_address_b58: "operator-payout".into(),
                memo: "memo".into(),
                attempted_at_ms: 5,
                status: crate::journal::TransferAttemptStatus::Attempted,
                tx_signature: None,
                detail: String::new(),
            })
            .unwrap();
        drop(state1);

        let mock2 = Arc::new(MockPayout::new());
        let state2 = durable_state(&journal_path, mock2.clone()).await;
        assert!(
            !state2.attempts().is_open(job_id),
            "a bracket whose completion marker landed closes itself at boot"
        );
        assert!(sweep_unpaid(&state2).await.is_empty());
        assert!(mock2.records().is_empty());
    }

    #[tokio::test]
    async fn a_conclusion_pins_the_receipts_operator_over_a_racing_reassign() {
        // The misattribution race: submit_result verifies the receipt
        // against operator A, a stale-offer reassign moves the durable
        // record to B in the handler's window, and the conclusion used
        // to leave B's name — and payout address — on A's work. The
        // sweep pays whoever the record names; it must be A.
        let payout = Arc::new(MockPayout::new());
        let state = CoordinatorState::new(
            LocalIdentity::generate("coordinator@test"),
            CoordinatorConfig::default(),
            Arc::new(NoReputation),
            payout.clone(),
            Arc::new(InMemoryAuditLog::new()),
        );
        let operator = LocalIdentity::generate("operator-a@test");
        let job_id = Uuid::new_v4();
        let buyer = LocalIdentity::generate("buyer@test").agent_id();
        let hold = state.escrow().hold(job_id, &buyer, 100).await.unwrap();

        // The durable record already reads operator B — the reassign
        // won the race before the conclusion wrote.
        let mut record = job_record(job_id, 0, 1_000, hold);
        record.operator_pubkey_b58 = "operator-b-pubkey".into();
        record.payout_address = "operator-b-payout".into();
        state.jobs().insert(job_id, record).unwrap();

        let receipt = signed_receipt(job_id, &operator, 100);
        state.escrow().release(job_id, &receipt).await.unwrap();
        state
            .jobs()
            .set_receipt_and_phase(
                job_id,
                receipt,
                vec![Content::text("done")],
                crate::jobs::ReleaseCharges::default(),
                JobPhase::Completed,
                None,
                crate::jobs::ReceiptAssignment {
                    operator_pubkey_b58: "operator-a-pubkey".into(),
                    payout_address: "operator-a-payout".into(),
                    metered_elapsed_ms: None,
                },
            )
            .unwrap();

        let concluded = state.jobs().get(job_id).unwrap();
        assert_eq!(concluded.operator_pubkey_b58, "operator-a-pubkey");
        assert_eq!(concluded.payout_address, "operator-a-payout");

        let pushed = sweep_unpaid(&state).await;
        assert_eq!(pushed, vec![job_id]);
        assert_eq!(payout.records().len(), 1);
        assert_eq!(
            payout.records()[0].payout_address,
            "operator-a-payout",
            "the sweep pays the operator whose receipt concluded the job, never the reassignee"
        );
    }

    /// Registers a `BatchJob` operator asking 100 at `seen_ms` and
    /// returns its pubkey — the sweep-side twin of the registry tests'
    /// fixture, priced to satisfy [`job_record`]'s envelopes.
    fn register_operator(state: &CoordinatorState, display: &str, seen_ms: u64) -> String {
        use covenant_compute_protocol::{
            CapabilityProfile, HardwareClass, PriceAsk, PriceUnit, RegisterRequest,
        };
        let identity = LocalIdentity::generate(display);
        let profile = CapabilityProfile {
            operator: identity.agent_id(),
            hardware: HardwareClass::CpuOnly,
            vram_gb: 0,
            models_served: vec!["any".into()],
            job_kinds: vec![JobKind::BatchJob],
            price: PriceAsk {
                unit: PriceUnit::PerJob,
                micro_usdc: 100,
            },
            tee_capable: false,
        };
        let req = RegisterRequest::sign(profile, payout_for(display), &identity).unwrap();
        state.registry().register(&req, seen_ms, None).unwrap();
        identity.agent_id().pubkey_base58()
    }

    /// The payout address [`register_operator`] registers `display`
    /// under — derived, so an assert can name the expected address
    /// without repeating the encoding.
    fn payout_for(display: &str) -> String {
        let mut key = [0u8; 32];
        for (i, b) in display.bytes().take(32).enumerate() {
            key[i] = b;
        }
        bs58::encode(key).into_string()
    }

    #[tokio::test]
    async fn a_stale_offer_re_matches_and_locks_out_the_dead_assignee() {
        use covenant_audit::AuditLog as _;

        let audit = Arc::new(InMemoryAuditLog::new());
        let state = CoordinatorState::new(
            LocalIdentity::generate("coordinator@test"),
            CoordinatorConfig::default(),
            Arc::new(NoReputation),
            Arc::new(MockPayout::new()),
            audit.clone(),
        );
        let now = 100_000;
        // The assignee registered long ago and went silent — past the
        // 45s liveness cutoff by `now`. The alternative is fresh.
        let dead = register_operator(&state, "dead@test", 0);
        let live = register_operator(&state, "live@test", now - 1_000);

        let job_id = Uuid::new_v4();
        let buyer = LocalIdentity::generate("buyer@test").agent_id();
        let hold = state.escrow().hold(job_id, &buyer, 100).await.unwrap();
        let mut record = job_record(job_id, 0, 300_000, hold);
        record.operator_pubkey_b58 = dead.clone();
        record.payout_address = "dead-payout".into();
        state.jobs().insert(job_id, record).unwrap();

        assert_eq!(sweep_stale_offers(&state, now).await, vec![job_id]);

        let record = state.jobs().get(job_id).unwrap();
        assert_eq!(record.operator_pubkey_b58, live);
        assert_eq!(record.payout_address, payout_for("live@test"));
        assert_eq!(record.offered_at_ms, now);
        assert_eq!(record.phase, JobPhase::Offered, "money untouched");

        // The new assignee's queue holds the offer...
        let offer = state
            .registry()
            .poll_next_job(&live, std::time::Duration::from_millis(20), now)
            .await
            .unwrap()
            .expect("re-offered to the live operator");
        assert_eq!(offer.envelope.payload.job_id, job_id);

        // ...the old assignee's late decision cannot land...
        assert!(!state
            .jobs()
            .set_phase_if_assigned(job_id, &dead, JobPhase::Accepted, None)
            .unwrap());

        // ...and the move is on the record, as routing history, with
        // no refund row faulting anyone.
        let events = audit.recent(10).await.unwrap();
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            AuditKind::ComputeJobReoffered {
                job_id: id,
                from_operator_pubkey_b58: from,
                to_operator_pubkey_b58: to,
            } if *id == job_id && *from == dead && *to == live
        )));
        assert!(!events
            .iter()
            .any(|e| matches!(&e.kind, AuditKind::ComputeJobRefunded { .. })));
    }

    #[tokio::test]
    async fn a_stale_offer_with_no_live_alternative_keeps_its_assignee() {
        let state = test_state();
        let now = 100_000;
        let dead = register_operator(&state, "dead@test", 0);

        let job_id = Uuid::new_v4();
        let buyer = LocalIdentity::generate("buyer@test").agent_id();
        let hold = state.escrow().hold(job_id, &buyer, 100).await.unwrap();
        let mut record = job_record(job_id, 0, 300_000, hold);
        record.operator_pubkey_b58 = dead.clone();
        state.jobs().insert(job_id, record).unwrap();

        assert!(sweep_stale_offers(&state, now).await.is_empty());
        let record = state.jobs().get(job_id).unwrap();
        assert_eq!(
            record.operator_pubkey_b58, dead,
            "with nobody live to take it, the assignee may yet come back"
        );
        assert_eq!(record.offered_at_ms, 0, "the clock keeps aging");
    }

    #[tokio::test]
    async fn a_lost_queue_redelivers_to_the_still_best_assignee() {
        use covenant_audit::AuditLog as _;

        let audit = Arc::new(InMemoryAuditLog::new());
        let state = CoordinatorState::new(
            LocalIdentity::generate("coordinator@test"),
            CoordinatorConfig::default(),
            Arc::new(NoReputation),
            Arc::new(MockPayout::new()),
            audit.clone(),
        );
        let now = 100_000;
        // Live and polling — but its queue is empty, the way every
        // queue is after a coordinator restart.
        let assignee = register_operator(&state, "assignee@test", now - 1_000);

        let job_id = Uuid::new_v4();
        let buyer = LocalIdentity::generate("buyer@test").agent_id();
        let hold = state.escrow().hold(job_id, &buyer, 100).await.unwrap();
        let mut record = job_record(job_id, 0, 300_000, hold);
        record.operator_pubkey_b58 = assignee.clone();
        record.payout_address = "as-won".into();
        state.jobs().insert(job_id, record).unwrap();

        // Not a reassignment: same operator, so no ComputeJobReoffered.
        assert!(sweep_stale_offers(&state, now).await.is_empty());
        let record = state.jobs().get(job_id).unwrap();
        assert_eq!(record.operator_pubkey_b58, assignee);
        assert_eq!(record.payout_address, "as-won", "terms stay as won");
        assert_eq!(record.offered_at_ms, now, "the clock restarts");
        assert!(!audit
            .recent(10)
            .await
            .unwrap()
            .iter()
            .any(|e| matches!(&e.kind, AuditKind::ComputeJobReoffered { .. })));

        let offer = state
            .registry()
            .poll_next_job(&assignee, std::time::Duration::from_millis(20), now)
            .await
            .unwrap()
            .expect("the lost offer is back in the queue");
        assert_eq!(offer.envelope.payload.job_id, job_id);

        // Freshly stamped: the very next tick has nothing to heal.
        assert!(sweep_stale_offers(&state, now + 10_000).await.is_empty());
    }

    #[tokio::test]
    async fn an_assignee_sitting_on_its_offer_is_routed_around() {
        let audit = Arc::new(InMemoryAuditLog::new());
        let state = CoordinatorState::new(
            LocalIdentity::generate("coordinator@test"),
            CoordinatorConfig::default(),
            Arc::new(NoReputation),
            Arc::new(MockPayout::new()),
            audit.clone(),
        );
        let now = 100_000;
        // Both look equally alive to the matcher. The assignee's queue
        // still holds the offer after the whole window — the one fact
        // that says it isn't polling, however healthy it looks.
        let ignorer = register_operator(&state, "ignorer@test", now - 1_000);
        let alternative = register_operator(&state, "alternative@test", now - 1_000);

        let job_id = Uuid::new_v4();
        let buyer = LocalIdentity::generate("buyer@test").agent_id();
        let hold = state.escrow().hold(job_id, &buyer, 100).await.unwrap();
        let mut record = job_record(job_id, 0, 300_000, hold);
        record.operator_pubkey_b58 = ignorer.clone();
        state
            .registry()
            .deliver(
                &ignorer,
                JobOffer {
                    envelope: record.envelope.clone(),
                    escrow_hold: record.escrow_hold.clone(),
                },
            )
            .then_some(())
            .expect("delivered");
        state.jobs().insert(job_id, record).unwrap();

        assert_eq!(sweep_stale_offers(&state, now).await, vec![job_id]);
        assert_eq!(
            state.jobs().get(job_id).unwrap().operator_pubkey_b58,
            alternative
        );
        assert!(
            !state.registry().queue_holds(&ignorer, job_id),
            "the ignored copy is revoked with the move"
        );
        assert!(state.registry().queue_holds(&alternative, job_id));
    }

    #[tokio::test]
    async fn a_wedged_assignee_with_no_alternative_keeps_the_job_and_its_offer() {
        let state = test_state();
        let now = 100_000;
        let ignorer = register_operator(&state, "ignorer@test", now - 1_000);

        let job_id = Uuid::new_v4();
        let buyer = LocalIdentity::generate("buyer@test").agent_id();
        let hold = state.escrow().hold(job_id, &buyer, 100).await.unwrap();
        let mut record = job_record(job_id, 0, 300_000, hold);
        record.operator_pubkey_b58 = ignorer.clone();
        state.registry().deliver(
            &ignorer,
            JobOffer {
                envelope: record.envelope.clone(),
                escrow_hold: record.escrow_hold.clone(),
            },
        );
        state.jobs().insert(job_id, record).unwrap();

        // Excluded, and nobody else fits: nothing moves, the queued
        // offer stays where it is (the node may yet wake up and accept)
        // and the deadline sweep remains the backstop.
        assert!(sweep_stale_offers(&state, now).await.is_empty());
        assert_eq!(
            state.jobs().get(job_id).unwrap().operator_pubkey_b58,
            ignorer
        );
        assert!(state.registry().queue_holds(&ignorer, job_id));
    }

    #[tokio::test]
    async fn a_zero_window_disables_the_re_offer_sweep() {
        let state = CoordinatorState::new(
            LocalIdentity::generate("coordinator@test"),
            CoordinatorConfig {
                reoffer_after: std::time::Duration::ZERO,
                ..CoordinatorConfig::default()
            },
            Arc::new(NoReputation),
            Arc::new(MockPayout::new()),
            Arc::new(InMemoryAuditLog::new()),
        );
        let now = 100_000;
        let dead = register_operator(&state, "dead@test", 0);
        register_operator(&state, "live@test", now - 1_000);

        let job_id = Uuid::new_v4();
        let buyer = LocalIdentity::generate("buyer@test").agent_id();
        let hold = state.escrow().hold(job_id, &buyer, 100).await.unwrap();
        let mut record = job_record(job_id, 0, 300_000, hold);
        record.operator_pubkey_b58 = dead.clone();
        state.jobs().insert(job_id, record).unwrap();

        assert!(sweep_stale_offers(&state, now).await.is_empty());
        assert_eq!(state.jobs().get(job_id).unwrap().operator_pubkey_b58, dead);
    }

    /// Inserts an `Offered` job assigned to `operator` with its offer
    /// delivered into the operator's queue — the state an Offline
    /// declaration finds when the assignee never polled.
    async fn queued_offer(
        state: &CoordinatorState,
        operator: &str,
        offered_at_ms: u64,
        deadline_ms: u64,
    ) -> Uuid {
        let job_id = Uuid::new_v4();
        let buyer = LocalIdentity::generate("buyer@test").agent_id();
        let hold = state.escrow().hold(job_id, &buyer, 100).await.unwrap();
        let mut record = job_record(job_id, offered_at_ms, deadline_ms, hold);
        record.operator_pubkey_b58 = operator.to_string();
        state.registry().deliver(
            operator,
            JobOffer {
                envelope: record.envelope.clone(),
                escrow_hold: record.escrow_hold.clone(),
            },
        );
        state.jobs().insert(job_id, record).unwrap();
        job_id
    }

    #[tokio::test]
    async fn an_offline_declaration_re_matches_its_queued_offers_immediately() {
        use covenant_audit::AuditLog as _;

        let audit = Arc::new(InMemoryAuditLog::new());
        let state = CoordinatorState::new(
            LocalIdentity::generate("coordinator@test"),
            CoordinatorConfig::default(),
            Arc::new(NoReputation),
            Arc::new(MockPayout::new()),
            audit.clone(),
        );
        let now = 100_000;
        let declarer = register_operator(&state, "declarer@test", now - 1_000);
        let alternative = register_operator(&state, "alternative@test", now - 1_000);

        // Freshly offered — no stale window has passed. Only the
        // declaration moves it.
        let job_id = queued_offer(&state, &declarer, now, 300_000).await;
        // A second job already past its deadline belongs to the refund
        // sweep; the heal must not hand a dying job to a live node.
        let expired = queued_offer(&state, &declarer, 0, 50_000).await;

        assert_eq!(reoffer_offline(&state, &declarer, now).await, vec![job_id]);

        let record = state.jobs().get(job_id).unwrap();
        assert_eq!(record.operator_pubkey_b58, alternative);
        assert_eq!(record.phase, JobPhase::Offered, "money untouched");
        assert!(!state.registry().queue_holds(&declarer, job_id));
        assert!(state.registry().queue_holds(&alternative, job_id));
        assert_eq!(
            state.jobs().get(expired).unwrap().operator_pubkey_b58,
            declarer,
            "an expired job is the refund sweep's, not the heal's"
        );

        let events = audit.recent(10).await.unwrap();
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            AuditKind::ComputeJobReoffered {
                job_id: id,
                from_operator_pubkey_b58: from,
                to_operator_pubkey_b58: to,
            } if *id == job_id && *from == declarer && *to == alternative
        )));
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, AuditKind::ComputeJobRefunded { .. })),
            "an honest outage is never a fault"
        );
    }

    #[tokio::test]
    async fn the_offline_heal_leaves_polled_out_offers_alone() {
        let state = test_state();
        let now = 100_000;
        let declarer = register_operator(&state, "declarer@test", now - 1_000);
        register_operator(&state, "alternative@test", now - 1_000);

        let job_id = queued_offer(&state, &declarer, now, 300_000).await;
        // The assignee polled the offer out before going dark: a
        // decision is in flight, the accept guard's domain.
        state
            .registry()
            .poll_next_job(&declarer, std::time::Duration::from_millis(20), now)
            .await
            .unwrap()
            .expect("polled out");

        assert!(reoffer_offline(&state, &declarer, now).await.is_empty());
        assert_eq!(
            state.jobs().get(job_id).unwrap().operator_pubkey_b58,
            declarer
        );
    }

    #[tokio::test]
    async fn the_offline_heal_never_moves_pinned_probes() {
        let state = test_state();
        let now = 100_000;
        let declarer = register_operator(&state, "declarer@test", now - 1_000);
        register_operator(&state, "alternative@test", now - 1_000);

        let job_id = Uuid::new_v4();
        let buyer = LocalIdentity::generate("buyer@test").agent_id();
        let hold = state.escrow().hold(job_id, &buyer, 100).await.unwrap();
        let mut record = job_record(job_id, now, 300_000, hold);
        record.operator_pubkey_b58 = declarer.clone();
        record.pinned = true;
        state.registry().deliver(
            &declarer,
            JobOffer {
                envelope: record.envelope.clone(),
                escrow_hold: record.escrow_hold.clone(),
            },
        );
        state.jobs().insert(job_id, record).unwrap();

        assert!(
            reoffer_offline(&state, &declarer, now).await.is_empty(),
            "a probe pinned to its target must stay pinned"
        );
        assert_eq!(
            state.jobs().get(job_id).unwrap().operator_pubkey_b58,
            declarer
        );
    }

    #[tokio::test]
    async fn the_offline_heal_with_no_alternative_leaves_the_assignment() {
        let state = test_state();
        let now = 100_000;
        let declarer = register_operator(&state, "declarer@test", now - 1_000);

        let job_id = queued_offer(&state, &declarer, now, 300_000).await;

        assert!(reoffer_offline(&state, &declarer, now).await.is_empty());
        assert_eq!(
            state.jobs().get(job_id).unwrap().operator_pubkey_b58,
            declarer,
            "nobody else can take it; the node may recover inside the deadline"
        );
        assert!(
            state.registry().queue_holds(&declarer, job_id),
            "the queued offer stays for a recovery"
        );
    }

    #[tokio::test]
    async fn a_zero_window_disables_the_offline_heal_too() {
        let state = CoordinatorState::new(
            LocalIdentity::generate("coordinator@test"),
            CoordinatorConfig {
                reoffer_after: std::time::Duration::ZERO,
                ..CoordinatorConfig::default()
            },
            Arc::new(NoReputation),
            Arc::new(MockPayout::new()),
            Arc::new(InMemoryAuditLog::new()),
        );
        let now = 100_000;
        let declarer = register_operator(&state, "declarer@test", now - 1_000);
        register_operator(&state, "alternative@test", now - 1_000);

        let job_id = queued_offer(&state, &declarer, now, 300_000).await;

        assert!(reoffer_offline(&state, &declarer, now).await.is_empty());
        assert_eq!(
            state.jobs().get(job_id).unwrap().operator_pubkey_b58,
            declarer,
            "one switch means the coordinator never moves assignments"
        );
    }
}
