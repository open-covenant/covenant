//! Canary probes (C5): the coordinator periodically buys work from its
//! own operators — a known-answer job dispatched through the ordinary
//! paid pipeline, indistinguishable from organic demand at the protocol
//! level — and checks the completed output against the answer it
//! planted. A hash-verified receipt proves the operator signed what it
//! returned; only a canary can prove what it returned was *work*. A
//! probe whose output flunks the check lands as a
//! `ComputeCanaryResult { passed: false }` audit row, which reputation
//! counts as a fault right next to the refund-attributed ones.
//!
//! Money: probes are the coordinator's own spend, so every canary hold
//! is `FundingSource::Bootstrap` regardless of the deployment's default
//! tag — which puts probe spend under the existing subsidy ceiling and
//! kill-switch. No subsidy policy, no canaries: the prober inherits the
//! anti-faucet discipline instead of needing its own. Operators are
//! paid their full ask for passed AND failed probes alike — the money
//! moved through a verified receipt, and clawing it back on a content
//! judgment would make the coordinator's fee discretionary. The probe's
//! value is the reputation verdict, not the refund.
//!
//! Threat model, honestly: a canary proves the operator is live and
//! actually serves instruction-following inference (or executes batch
//! commands). Fingerprint hardening raises the cost of special-casing
//! probes — the buyer identity is freshly generated per probe (no
//! stable pubkey to allowlist; the name matches the stock MCP buyer's,
//! so probes sit inside the most common buyer population) and the
//! instruction template rotates across a pool — but a determined
//! adversary can still classify known-answer prompt SHAPES. Catching
//! that class needs redundancy over deterministic kinds, TEE
//! attestation, or stake at risk (Phase 2). What this catches is the
//! cheapest real fraud: a registered node that returns garbage, a
//! canned string, or its own input, wrapped in a perfectly valid
//! receipt — now without a free allowlist bit.
//!
//! Rotation's bookkeeping cost: the job book can no longer identify
//! probes by their buyer, so every dispatch lands a
//! `ComputeCanaryDispatched` audit row and a (restarted) prober
//! replays those, minus the `ComputeCanaryResult` verdicts, to find
//! probes still owed a judgment. Probes dispatched by a pre-rotation
//! build (stable canary identity, no dispatch marker) stop being
//! judged after an upgrade — they conclude as ordinary jobs, already
//! paid, just without a verdict row.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
use covenant_audit::AuditKind;
use covenant_compute_protocol::{
    CapabilityRequirement, FederationEscrow, FundingSource, JobEnvelopePayload, JobKind, JobOffer,
    RefundReason, SignedJobEnvelope,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use parking_lot::Mutex;
use uuid::Uuid;

use crate::jobs::{JobPhase, JobRecord};
use crate::state::CoordinatorState;

/// Instruction wrappers for inference canaries, one picked per probe by
/// the job id so no single phrasing is a stable fingerprint. Every
/// template ends with a colon and the nonce is appended as the final
/// whitespace-separated token — the judge re-derives the nonce as the
/// input's last token and the instruction as everything before it, so
/// judging needs no record of which template was used. The judge
/// rejects any output that still contains the instruction: a node that
/// echoes its input back — the `echo` executor, or a "model" that is
/// `cat` — would otherwise pass the contains-the-token check by
/// construction.
pub const INFER_TEMPLATES: &[&str] = &[
    "Reply with exactly this token and nothing else:",
    "Respond with only the following token, no other words:",
    "Return this verification code verbatim and nothing more:",
    "Output exactly this string, with no surrounding text:",
    "Your entire reply must be exactly this code:",
    "Repeat the following token back with nothing added:",
];
/// Per-probe buyer identities carry the same display name the stock
/// standalone MCP server hardcodes for real buyers — a probe should
/// look like the most common buyer, not like a probe. The redundancy
/// sampler's mirror buyers blend the same way.
pub(crate) const CANARY_BUYER_NAME: &str = "buyer@compute";
const BATCH_PREFIX: &str = "echo ";

#[derive(Debug, Clone)]
pub struct CanaryConfig {
    /// Probes are skipped for operators asking above this — a canary
    /// budget leak through one expensive ask would starve probes for
    /// everyone else under the shared subsidy ceiling.
    pub max_price_micro_usdc: u64,
    /// Deadline stamped on each probe envelope. Generous by default:
    /// a cold model load on a consumer node is slow, and a timed-out
    /// canary already costs the operator an attributed refund fault.
    pub deadline_ms: u64,
}

impl Default for CanaryConfig {
    fn default() -> Self {
        Self {
            max_price_micro_usdc: 10_000,
            deadline_ms: 120_000,
        }
    }
}

/// What one [`CanaryProber::tick`] did — returned (not just logged) so
/// tests and operators can assert on probe behavior directly.
#[derive(Debug, Default)]
pub struct TickReport {
    /// Completed probes judged this tick: `(job_id, operator, passed)`.
    pub judged: Vec<(Uuid, String, bool)>,
    /// The probe dispatched this tick, if any.
    pub dispatched: Option<(Uuid, String)>,
    /// Why nothing was dispatched, when nothing was.
    pub idle_reason: Option<String>,
}

/// The prober's durable-state mirror: which jobs are probes (and whose)
/// and which already have a verdict. Seeded once from the audit log's
/// own `ComputeCanaryDispatched`/`ComputeCanaryResult` rows, then kept
/// current in memory — the audit chain is the record that survives a
/// restart, since rotating buyer identities removed the job book's way
/// of recognizing a probe.
#[derive(Default, Clone)]
struct CanaryBooks {
    /// probe job id → the operator it targets.
    probes: HashMap<Uuid, String>,
    /// Probes already judged (or terminal without a judgment).
    resolved: HashSet<Uuid>,
    /// Probes whose fault verdict row was seen at seed time but whose
    /// separate slash may not have landed — a crash between the two
    /// durable writes (the row, then `slash_for_fault`) used to strand a
    /// coordinator-proven fault with its stake intact. Populated only
    /// from the audit chain, never from an in-process judgment (whose
    /// slash fired in the same pass), so this holds exactly the restart
    /// residue. The first post-restart judge pass re-fires each one — the
    /// slash is idempotent by id, so one that already landed moves
    /// nothing — then drops it. Mirrors `RedundancySampler`'s
    /// already-written re-fire.
    faulted_unslashed: HashSet<Uuid>,
}

pub struct CanaryProber {
    state: CoordinatorState,
    config: CanaryConfig,
    /// operator pubkey → last dispatch ms; least-recently-probed wins
    /// the next tick. In-memory: after a restart every operator just
    /// becomes probe-able again.
    last_probed: Mutex<HashMap<String, u64>>,
    books: Mutex<Option<CanaryBooks>>,
}

impl CanaryProber {
    pub fn new(state: CoordinatorState, config: CanaryConfig) -> Self {
        Self {
            state,
            config,
            last_probed: Mutex::new(HashMap::new()),
            books: Mutex::new(None),
        }
    }

    /// One probe cycle: judge every canary that reached a terminal
    /// phase since the last tick, then dispatch at most one new probe
    /// to the least-recently-probed eligible operator.
    pub async fn tick(&self) -> TickReport {
        let mut report = TickReport::default();
        self.judge_terminal(&mut report).await;
        self.dispatch_next(&mut report).await;
        report
    }

    async fn seeded_books(&self) -> CanaryBooks {
        if let Some(books) = self.books.lock().as_ref() {
            return books.clone();
        }
        // Seed once from the durable log, but only on a clean read. An
        // audit-read failure must not cache an empty history: that would
        // forget every probe dispatched before this boot for the rest of
        // the process's life, so a probe never gets judged and an operator
        // that returned garbage under a hash-valid receipt is never
        // slashed. Serve an empty view for this tick and re-seed on the
        // next, when the read may succeed.
        let Ok(events) = self.state.audit().recent(usize::MAX).await else {
            return self.books.lock().as_ref().cloned().unwrap_or_default();
        };
        let mut seed = CanaryBooks::default();
        for event in events {
            match event.kind {
                AuditKind::ComputeCanaryDispatched {
                    job_id,
                    operator_pubkey_b58,
                } => {
                    seed.probes.insert(job_id, operator_pubkey_b58);
                }
                AuditKind::ComputeCanaryResult { job_id, passed, .. } => {
                    seed.resolved.insert(job_id);
                    if !passed {
                        seed.faulted_unslashed.insert(job_id);
                    }
                }
                _ => {}
            }
        }
        let mut guard = self.books.lock();
        guard.get_or_insert_with(|| seed).clone()
    }

    async fn judge_terminal(&self, report: &mut TickReport) {
        let books = self.seeded_books().await;
        for (job_id, operator) in &books.probes {
            if books.resolved.contains(job_id) {
                // The verdict row already stands, so this probe is never
                // re-judged and its row is never re-written (reputation
                // tallies rows with no dedup). But a fault whose row was
                // seeded from the log may have crashed between that row
                // and its separate slash — complete that slash now,
                // idempotent by id so a slash that already landed moves
                // nothing.
                if books.faulted_unslashed.contains(job_id) {
                    self.refire_dropped_slash(*job_id).await;
                }
                continue;
            }
            let Some(record) = self.state.jobs().get(*job_id) else {
                // A dispatch marker with no job record: the offer never
                // became durable (or the book was lost). Nothing will
                // ever be judgeable here — retire it instead of
                // rescanning forever.
                tracing::warn!(%job_id, operator = %operator, "canary marker has no job record; retiring");
                self.mark_resolved(*job_id);
                continue;
            };
            match record.phase {
                JobPhase::Completed => {}
                // Never served: the refund machinery already attributed
                // the fault (deadline_expired / operator_rejected /
                // execution_failed with an assignee). A canary row on
                // top would double-count one event.
                JobPhase::Failed | JobPhase::Refunded | JobPhase::Rejected => {
                    self.mark_resolved(*job_id);
                    continue;
                }
                JobPhase::Offered | JobPhase::Accepted | JobPhase::AwaitingCheck => continue,
            }
            let (passed, detail) = judge_output(&record);
            self.state
                .record_audit(AuditKind::ComputeCanaryResult {
                    job_id: *job_id,
                    operator_pubkey_b58: record.operator_pubkey_b58.clone(),
                    passed,
                    detail: detail.clone(),
                })
                .await;
            if !passed {
                // The wrong answer is a coordinator-proven fault: take
                // the probe's price from the operator's bond (C5 phase
                // 2). The audit row above is the evidence.
                self.state
                    .slash_for_fault(
                        "canary",
                        *job_id,
                        &record.operator_pubkey_b58,
                        record.envelope.payload.price_micro_usdc,
                        &format!("canary wrong-answer: {detail}"),
                    )
                    .await;
            }
            tracing::info!(%job_id, operator = %record.operator_pubkey_b58, passed, detail, "canary judged");
            report
                .judged
                .push((*job_id, record.operator_pubkey_b58, passed));
            self.mark_resolved(*job_id);
        }
    }

    fn mark_resolved(&self, job_id: Uuid) {
        if let Some(books) = self.books.lock().as_mut() {
            books.resolved.insert(job_id);
        }
    }

    /// Completes a fault's slash after a restart found its verdict row
    /// but cannot be sure its slash landed. Sized and attributed from the
    /// probe's own record, exactly as the first judgment sized it, so the
    /// deterministic slash id (`canary:{job_id}:{operator}`) dedups a
    /// slash that in fact already landed — the take happens at most once.
    /// The record survives every restart (a terminal probe is durable);
    /// only a book so torn that the record is gone drops the re-fire, and
    /// then the verdict row still stands as the fault's evidence.
    async fn refire_dropped_slash(&self, job_id: Uuid) {
        if let Some(record) = self.state.jobs().get(job_id) {
            self.state
                .slash_for_fault(
                    "canary",
                    job_id,
                    &record.operator_pubkey_b58,
                    record.envelope.payload.price_micro_usdc,
                    "canary wrong-answer (slash re-fired after a crash between the verdict and \
                     its slash)",
                )
                .await;
        }
        self.clear_faulted(job_id);
    }

    fn clear_faulted(&self, job_id: Uuid) {
        if let Some(books) = self.books.lock().as_mut() {
            books.faulted_unslashed.remove(&job_id);
        }
    }

    fn mark_dispatched(&self, job_id: Uuid, operator: &str) {
        if let Some(books) = self.books.lock().as_mut() {
            books.probes.insert(job_id, operator.to_string());
        }
    }

    /// Operators with a canary still in flight are skipped — one
    /// unanswered question per operator at a time, so a wedged node
    /// can't accumulate probe spend past its first fault.
    async fn operators_with_pending_probe(&self) -> HashSet<String> {
        let books = self.seeded_books().await;
        books
            .probes
            .iter()
            .filter(|(job_id, _)| !books.resolved.contains(job_id))
            .filter(|(job_id, _)| {
                self.state
                    .jobs()
                    .get(**job_id)
                    .is_some_and(|r| matches!(r.phase, JobPhase::Offered | JobPhase::Accepted))
            })
            .map(|(_, operator)| operator.clone())
            .collect()
    }

    async fn dispatch_next(&self, report: &mut TickReport) {
        let now_ms = crate::epoch_ms();
        let cutoff_ms = self.state.config().operator_liveness_timeout.as_millis() as u64;
        let pending = self.operators_with_pending_probe().await;

        let mut candidates: Vec<(String, u64)> = Vec::new();
        let mut plans: HashMap<String, (JobKind, Option<String>, u64)> = HashMap::new();
        for (key, record) in self.state.registry().snapshot() {
            if record.status != covenant_compute_protocol::OperatorStatus::Online
                || now_ms.saturating_sub(record.last_seen_ms) > cutoff_ms
                || pending.contains(&key)
            {
                continue;
            }
            let Some(plan) = probe_plan(&record.profile) else {
                continue;
            };
            let ask = record.profile.ask_for(plan.0).micro_usdc;
            if ask > self.config.max_price_micro_usdc {
                continue;
            }
            plans.insert(key.clone(), (plan.0, plan.1, ask));
            let last = self.last_probed.lock().get(&key).copied().unwrap_or(0);
            candidates.push((key, last));
        }
        let Some((operator, _)) = candidates.into_iter().min_by_key(|(_, last)| *last) else {
            report.idle_reason = Some("no eligible operator to probe".into());
            return;
        };
        let (kind, model_id, price) = plans.remove(&operator).expect("planned above");

        // A fresh key per probe: the buyer pubkey a freeloader would
        // allowlist changes every time. The private key is dropped
        // after signing — judging reads the job book, and probe money
        // is the coordinator's own, so nothing ever needs it again.
        let probe_identity = LocalIdentity::generate(CANARY_BUYER_NAME);
        let job_id = Uuid::new_v4();
        let nonce = Uuid::new_v4().simple().to_string()[..12].to_string();
        let input_text = match kind {
            JobKind::InferenceCall => {
                let template =
                    INFER_TEMPLATES[job_id.as_bytes()[0] as usize % INFER_TEMPLATES.len()];
                format!("{template} {nonce}")
            }
            _ => format!("{BATCH_PREFIX}{nonce}"),
        };
        let payload = JobEnvelopePayload {
            job_id,
            buyer: probe_identity.agent_id(),
            kind,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id,
                kind,
                max_duration_secs: (self.config.deadline_ms / 1_000).max(1) as u32,
                // A canary pins its target operator; it never matches on
                // reputation, so it carries no floor.
                min_reputation_bps: None,
            },
            input: vec![Content::text(input_text)],
            price_micro_usdc: price,
            deadline_ms: self.config.deadline_ms,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, job_id.to_string()),
            issued_at_ms: now_ms,
            referral_code: None,
            stream: false,
        };
        let envelope = match SignedJobEnvelope::sign(payload, &probe_identity) {
            Ok(envelope) => envelope,
            Err(e) => {
                report.idle_reason = Some(format!("canary envelope signing failed: {e}"));
                return;
            }
        };

        // Bootstrap-tagged always: probe spend rides the subsidy books
        // and stops dead at the kill-switch ceiling.
        let escrow_hold = match self
            .state
            .escrow()
            .hold_with_source(
                job_id,
                &probe_identity.agent_id(),
                price,
                FundingSource::Bootstrap,
            )
            .await
        {
            Ok(hold) => hold,
            Err(e) => {
                report.idle_reason = Some(format!("canary hold refused: {e}"));
                tracing::debug!(operator = %operator, error = %e, "canary probe skipped");
                return;
            }
        };

        // A canary is coordinator-manufactured probe traffic with a
        // synthetic per-probe buyer, not a real sale. It must not carry
        // the operator's supply-side referral code: settlement carves the
        // partner rev-share (C8) out of the captured fee, and a probe's
        // fee is carved from the bootstrap subsidy that funds it — so an
        // inherited code would pay an external partner, in withdrawable
        // money, for work the coordinator commissioned itself. That is the
        // faucet the funding-source discipline forbids. The buyer-side
        // code on this record is `None` for the same reason.
        let payout_address = self
            .state
            .registry()
            .record(&operator)
            .map(|r| r.payout_address)
            .unwrap_or_default();
        // Same discipline as the submit path: the offer must not go out
        // unless the record authorizing its release is durable.
        if let Err(e) = self.state.jobs().insert(
            job_id,
            JobRecord {
                operator_pubkey_b58: operator.clone(),
                payout_address,
                buyer_referral_code: None,
                envelope: envelope.clone(),
                escrow_hold: escrow_hold.clone(),
                phase: JobPhase::Offered,
                receipt: None,
                output: None,
                fee_micro_usdc: 0,
                referral_code: None,
                partner_share_micro_usdc: 0,
                buyer_partner_share_micro_usdc: 0,
                payout: None,
                concluded_at_ms: None,
                refund_reason: None,
                dispute: None,
                offered_at_ms: now_ms,
                pinned: true,
                accepted_at_ms: None,
                metered_elapsed_ms: None,
                close_requested_at_ms: None,
                lease_access: None,
                check_jobs: Vec::new(),
                checks_task: None,
                hidden_checks: None,
                vote_round: None,
                rework: None,
                order: None,
            },
        ) {
            let _ = self
                .state
                .escrow()
                .refund(job_id, RefundReason::AdmissionFailed)
                .await;
            report.idle_reason = Some(format!("canary record not durable: {e}"));
            return;
        }
        // The durable probe marker. With per-probe buyer identities the
        // job book cannot say which jobs are canaries, so the audit
        // chain must — recorded before the offer goes out, so a crash
        // between here and delivery leaves a marker whose job simply
        // concludes (or refunds) and gets retired, never a served probe
        // no restart can judge.
        self.state
            .record_audit(AuditKind::ComputeCanaryDispatched {
                job_id,
                operator_pubkey_b58: operator.clone(),
            })
            .await;
        self.mark_dispatched(job_id, &operator);
        if !self.state.registry().deliver(
            &operator,
            JobOffer {
                envelope,
                escrow_hold,
                rework: None,
                reproduction: None,
            },
        ) {
            let _ = self
                .state
                .escrow()
                .refund(job_id, RefundReason::AdmissionFailed)
                .await;
            let _ = self.state.jobs().conclude_unpaid(
                job_id,
                JobPhase::Refunded,
                RefundReason::AdmissionFailed,
            );
            report.idle_reason = Some(format!("operator {operator} vanished before delivery"));
            return;
        }
        self.state
            .record_audit(AuditKind::ComputeJobOffered {
                job_id,
                operator_pubkey_b58: operator.clone(),
                price_micro_usdc: price,
                funding_source: "bootstrap".into(),
            })
            .await;
        self.last_probed.lock().insert(operator.clone(), now_ms);
        tracing::info!(%job_id, operator = %operator, price_micro_usdc = price, "canary dispatched");
        report.dispatched = Some((job_id, operator));
    }
}

/// What to probe an operator with: inference against a concrete model
/// it claims to serve, else a batch echo if it takes batch work, else
/// nothing — `LeaseSession` has no known-answer shape.
fn probe_plan(
    profile: &covenant_compute_protocol::CapabilityProfile,
) -> Option<(JobKind, Option<String>)> {
    if profile.job_kinds.contains(&JobKind::InferenceCall) {
        if let Some(model) = profile.models_served.iter().find(|m| *m != "any") {
            return Some((JobKind::InferenceCall, Some(model.clone())));
        }
    }
    if profile.job_kinds.contains(&JobKind::BatchJob) {
        return Some((JobKind::BatchJob, None));
    }
    None
}

/// Judges a completed canary from its own durable record: the planted
/// answer is re-derived from the envelope's input, so a judgment needs
/// no state that a restart could lose.
fn judge_output(record: &JobRecord) -> (bool, String) {
    let Some(Content::Text { text: input }) = record.envelope.payload.input.first() else {
        return (false, "canary input was not a text block".into());
    };
    let output = record
        .output
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|block| match block {
            Content::Text { text } => Some(text.as_str()),
            Content::Json { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    match record.envelope.payload.kind {
        JobKind::InferenceCall => {
            let Some(nonce) = input.rsplit(' ').next().filter(|n| !n.is_empty()) else {
                return (false, "canary input carried no token".into());
            };
            if !output.contains(nonce) {
                return (false, "output did not contain the expected token".into());
            }
            // Whatever template this probe used, the instruction is the
            // input minus its trailing nonce — so the echo check needs
            // no record of the template choice.
            let instruction = input[..input.len() - nonce.len()].trim();
            if !instruction.is_empty() && output.contains(instruction) {
                return (
                    false,
                    "output echoed the instruction — input reflected back, not inference".into(),
                );
            }
            (true, "ok".into())
        }
        _ => {
            let Some(nonce) = input.strip_prefix(BATCH_PREFIX) else {
                return (false, "canary input was not the expected command".into());
            };
            if output.trim() == nonce {
                (true, "ok".into())
            } else {
                (false, "output was not exactly the expected token".into())
            }
        }
    }
}

/// Spawns the probe loop; the caller decides the cadence and holds the
/// handle. Mirrors [`crate::sweep::spawn_periodic_sweep`].
pub fn spawn_periodic_canary(
    prober: Arc<CanaryProber>,
    interval: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            prober.tick().await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_compute_protocol::{
        CapabilityProfile, EscrowHoldAttestation, HardwareClass, PriceAsk, PriceUnit,
    };

    fn canary_record(
        kind: JobKind,
        input: &str,
        output: Option<Vec<Content>>,
        buyer: &LocalIdentity,
    ) -> JobRecord {
        let job_id = Uuid::new_v4();
        let payload = JobEnvelopePayload {
            job_id,
            buyer: buyer.agent_id(),
            kind,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: None,
                kind,
                max_duration_secs: 60,
                min_reputation_bps: None,
            },
            input: vec![Content::text(input)],
            price_micro_usdc: 100,
            deadline_ms: 60_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, job_id.to_string()),
            issued_at_ms: 1,
            referral_code: None,
            stream: false,
        };
        let envelope = SignedJobEnvelope::sign(payload, buyer).unwrap();
        let escrow_hold =
            EscrowHoldAttestation::sign(job_id, 100, FundingSource::Bootstrap, 1, buyer).unwrap();
        JobRecord {
            operator_pubkey_b58: "op".into(),
            payout_address: "addr".into(),
            envelope,
            escrow_hold,
            phase: JobPhase::Completed,
            receipt: None,
            output,
            fee_micro_usdc: 0,
            referral_code: None,
            partner_share_micro_usdc: 0,
            buyer_referral_code: None,
            buyer_partner_share_micro_usdc: 0,
            payout: None,
            concluded_at_ms: None,
            refund_reason: None,
            dispute: None,
            offered_at_ms: 1,
            pinned: true,
            accepted_at_ms: None,
            metered_elapsed_ms: None,
            close_requested_at_ms: None,
            lease_access: None,
            check_jobs: Vec::new(),
            checks_task: None,
            hidden_checks: None,
            vote_round: None,
            rework: None,
            order: None,
        }
    }

    #[test]
    fn an_inference_canary_passes_on_the_bare_token_and_fails_on_an_echo() {
        let buyer = LocalIdentity::generate("canary@test");
        // The judge is template-agnostic: every phrasing in the pool
        // must judge identically, or rotating templates would rotate
        // the verdict rules with them.
        for template in INFER_TEMPLATES {
            let input = format!("{template} abc123def456");

            let honest = canary_record(
                JobKind::InferenceCall,
                &input,
                Some(vec![Content::text("abc123def456")]),
                &buyer,
            );
            assert!(judge_output(&honest).0, "template: {template}");

            // Chatty-but-correct still passes: the token is there and
            // the instruction is not.
            let chatty = canary_record(
                JobKind::InferenceCall,
                &input,
                Some(vec![Content::text("Sure — abc123def456.")]),
                &buyer,
            );
            assert!(judge_output(&chatty).0, "template: {template}");

            // An echo node reflects the whole prompt: token present,
            // instruction present, no inference happened.
            let echo = canary_record(
                JobKind::InferenceCall,
                &input,
                Some(vec![Content::text(input.clone())]),
                &buyer,
            );
            let (passed, detail) = judge_output(&echo);
            assert!(!passed, "template: {template}");
            assert!(detail.contains("echoed"), "got: {detail}");

            // Garbage output: no token at all.
            let garbage = canary_record(
                JobKind::InferenceCall,
                &input,
                Some(vec![Content::text("as an ai model i cannot")]),
                &buyer,
            );
            assert!(!judge_output(&garbage).0, "template: {template}");

            // Completed with no output should never pass.
            let empty = canary_record(JobKind::InferenceCall, &input, None, &buyer);
            assert!(!judge_output(&empty).0, "template: {template}");
        }
    }

    #[test]
    fn a_batch_canary_demands_the_exact_token() {
        let buyer = LocalIdentity::generate("canary@test");
        let input = format!("{BATCH_PREFIX}feedbeef0123");

        let exact = canary_record(
            JobKind::BatchJob,
            &input,
            Some(vec![Content::text("feedbeef0123\n")]),
            &buyer,
        );
        assert!(judge_output(&exact).0, "trailing newline is trimmed");

        let padded = canary_record(
            JobKind::BatchJob,
            &input,
            Some(vec![Content::text("ran: feedbeef0123")]),
            &buyer,
        );
        assert!(!judge_output(&padded).0, "batch output must be exact");
    }

    #[test]
    fn probe_plans_prefer_a_concrete_model_and_skip_unprobeable_profiles() {
        let identity = LocalIdentity::generate("op@test");
        let mut profile = CapabilityProfile {
            operator: identity.agent_id(),
            hardware: HardwareClass::CpuOnly,
            vram_gb: 0,
            models_served: vec!["any".into(), "qwen2.5:7b".into()],
            job_kinds: vec![JobKind::InferenceCall, JobKind::BatchJob],
            price: PriceAsk {
                unit: PriceUnit::PerJob,
                micro_usdc: 100,
            },
            tee_capable: false,
            kind_prices: Vec::new(),
            kind_models: Vec::new(),
        };
        assert_eq!(
            probe_plan(&profile),
            Some((JobKind::InferenceCall, Some("qwen2.5:7b".into())))
        );

        // Inference with only "any" models: fall through to batch.
        profile.models_served = vec!["any".into()];
        assert_eq!(probe_plan(&profile), Some((JobKind::BatchJob, None)));

        // Lease-only capacity has no known-answer probe.
        profile.job_kinds = vec![JobKind::LeaseSession];
        assert_eq!(probe_plan(&profile), None);
    }

    #[tokio::test]
    async fn a_canary_fault_whose_slash_crashed_re_fires_it_on_restart() {
        use crate::payout::MockPayout;
        use crate::reputation::NoReputation;
        use crate::state::CoordinatorConfig;
        use covenant_audit::{AuditLog as _, InMemoryAuditLog};

        let audit = Arc::new(InMemoryAuditLog::new());
        let state = CoordinatorState::new(
            LocalIdentity::generate("coordinator@canary"),
            CoordinatorConfig::default(),
            Arc::new(NoReputation),
            Arc::new(MockPayout::new()),
            audit.clone(),
        );

        // A completed canary the operator flunked: it echoed the whole
        // instruction back instead of returning the bare token.
        let buyer = LocalIdentity::generate(CANARY_BUYER_NAME);
        let input = format!("{} tok-abc123", INFER_TEMPLATES[0]);
        let record = canary_record(
            JobKind::InferenceCall,
            &input,
            Some(vec![Content::text(input.clone())]),
            &buyer,
        );
        let job_id = record.envelope.payload.job_id;
        let operator = record.operator_pubkey_b58.clone();
        assert!(!judge_output(&record).0, "the echo must judge as a fault");
        state.jobs().insert(job_id, record).unwrap();

        // Stake for the slash the recovered fault must take.
        state
            .bonds()
            .credit_post("bond-canary-crash", &operator, 5_000)
            .unwrap();

        // The crash residue: the dispatch marker and the fault verdict
        // row both landed durably; the slash that should have followed
        // did not.
        state
            .record_audit(AuditKind::ComputeCanaryDispatched {
                job_id,
                operator_pubkey_b58: operator.clone(),
            })
            .await;
        state
            .record_audit(AuditKind::ComputeCanaryResult {
                job_id,
                operator_pubkey_b58: operator.clone(),
                passed: false,
                detail: "output echoed the instruction".into(),
            })
            .await;
        assert_eq!(
            state.bonds().status(&operator).slashed_micro_usdc,
            0,
            "no slash landed before the crash"
        );

        // The restart: a fresh prober seeded only from the audit chain.
        // The verdict row stands, so nothing is re-judged — but the
        // dropped slash is completed.
        let prober = CanaryProber::new(state.clone(), CanaryConfig::default());
        let report = prober.tick().await;
        assert!(
            report.judged.is_empty(),
            "a probe that already has a verdict row is never re-judged: {report:?}"
        );
        assert_eq!(
            state.bonds().status(&operator).slashed_micro_usdc,
            100,
            "the recovered fault takes the probe's price from the operator's bond"
        );

        // The slash wrote no second verdict row, so reputation cannot
        // double-count the fault.
        let result_rows = audit
            .recent(usize::MAX)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| {
                matches!(&e.kind, AuditKind::ComputeCanaryResult { job_id: j, .. } if *j == job_id)
            })
            .count();
        assert_eq!(result_rows, 1, "the verdict row is never re-written");

        // A second restart re-seeds the same residue, but the durable
        // slash id dedups: the stake is taken exactly once.
        let again = CanaryProber::new(state.clone(), CanaryConfig::default());
        again.tick().await;
        assert_eq!(
            state.bonds().status(&operator).slashed_micro_usdc,
            100,
            "a slash that already landed is never taken twice"
        );
    }

    #[tokio::test]
    async fn a_transient_audit_read_failure_does_not_permanently_disable_probing() {
        use crate::payout::MockPayout;
        use crate::reputation::NoReputation;
        use crate::state::CoordinatorConfig;
        use covenant_audit::{
            AuditError, AuditEvent, AuditIntegrityReport, AuditLog, InMemoryAuditLog,
        };
        use std::sync::atomic::{AtomicBool, Ordering};

        // An audit log whose reads can be made to fail on demand — the
        // transient a file-backed log can hit — while its writes still land.
        struct FlakyAudit {
            inner: InMemoryAuditLog,
            fail_reads: AtomicBool,
        }

        #[async_trait::async_trait]
        impl AuditLog for FlakyAudit {
            async fn record(&self, event: AuditEvent) -> Result<(), AuditError> {
                self.inner.record(event).await
            }
            async fn recent(&self, limit: usize) -> Result<Vec<AuditEvent>, AuditError> {
                if self.fail_reads.load(Ordering::SeqCst) {
                    return Err(AuditError::Io(std::io::Error::other("audit unavailable")));
                }
                self.inner.recent(limit).await
            }
            async fn purge_older_than(&self, before_ms: u64) -> Result<u64, AuditError> {
                self.inner.purge_older_than(before_ms).await
            }
            async fn verify_integrity(&self) -> Result<AuditIntegrityReport, AuditError> {
                self.inner.verify_integrity().await
            }
        }

        let audit = Arc::new(FlakyAudit {
            inner: InMemoryAuditLog::new(),
            fail_reads: AtomicBool::new(false),
        });
        let state = CoordinatorState::new(
            LocalIdentity::generate("coordinator@canary"),
            CoordinatorConfig::default(),
            Arc::new(NoReputation),
            Arc::new(MockPayout::new()),
            audit.clone(),
        );

        // The same crash residue as the re-fire test: a flunked probe with
        // its dispatch marker and fault verdict on the chain, slash dropped.
        let buyer = LocalIdentity::generate(CANARY_BUYER_NAME);
        let input = format!("{} tok-flaky1", INFER_TEMPLATES[0]);
        let record = canary_record(
            JobKind::InferenceCall,
            &input,
            Some(vec![Content::text(input.clone())]),
            &buyer,
        );
        let job_id = record.envelope.payload.job_id;
        let operator = record.operator_pubkey_b58.clone();
        assert!(!judge_output(&record).0, "the echo must judge as a fault");
        state.jobs().insert(job_id, record).unwrap();
        state
            .bonds()
            .credit_post("bond-canary-flaky", &operator, 5_000)
            .unwrap();
        state
            .record_audit(AuditKind::ComputeCanaryDispatched {
                job_id,
                operator_pubkey_b58: operator.clone(),
            })
            .await;
        state
            .record_audit(AuditKind::ComputeCanaryResult {
                job_id,
                operator_pubkey_b58: operator.clone(),
                passed: false,
                detail: "output echoed the instruction".into(),
            })
            .await;

        let prober = CanaryProber::new(state.clone(), CanaryConfig::default());

        // A tick whose seed read fails judges nothing and takes no slash —
        // but it must not cache that empty history.
        audit.fail_reads.store(true, Ordering::SeqCst);
        prober.tick().await;
        assert_eq!(
            state.bonds().status(&operator).slashed_micro_usdc,
            0,
            "a failed audit read judges nothing this tick"
        );

        // The next tick on the SAME prober reads cleanly, so the books
        // re-seed from the chain and the dropped slash finally lands. A
        // cached empty history would leave this at zero for the process's life.
        audit.fail_reads.store(false, Ordering::SeqCst);
        prober.tick().await;
        assert_eq!(
            state.bonds().status(&operator).slashed_micro_usdc,
            100,
            "the next clean read re-seeds and completes the dropped slash"
        );
    }
}
