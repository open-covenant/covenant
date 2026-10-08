//! Redundancy sampling (C5's second half): the coordinator re-buys a
//! released job's exact input from other operators and compares
//! the `result_hash_hex` each receipt signed. The canary catches an
//! operator that can't do the work; this catches one whose work
//! quietly differs from everyone else's on inputs the coordinator
//! never planted an answer for. A strict majority of matching hashes
//! is the verdict: operators outside it land a
//! `ComputeRedundancyResult { agreed: Some(false) }` audit row, which
//! reputation counts as a fault next to the refund, canary and dispute
//! ones.
//!
//! Honesty first: hash agreement only means anything when the workload
//! is deterministic — same input, same bytes out. Batch jobs qualify by
//! the deployment's own assertion. Inference does not by default: an LLM
//! samples. But an inference job that pinned greedy decoding —
//! temperature 0 and a seed — is reproducible on a given backend, so a
//! deployment whose operator pool is homogeneous enough (same model,
//! quantization and hardware class) can opt inference in with
//! `COVENANT_COMPUTE_REDUNDANCY_INFERENCE` and the sampler cross-checks
//! those jobs too. It never samples a request that left decoding free to
//! vary (temperature above 0, or no seed) — that would fault honest
//! operators on output they were entitled to differ on. The whole
//! sampler stays opt-in per deployment
//! (`COVENANT_COMPUTE_REDUNDANCY_INTERVAL_SECS`); enabling it is the
//! operator's assertion that the sampled population is deterministic, and
//! the inference toggle is that same assertion for a pool across which
//! temperature-0 output is trusted to reproduce (which `GenerationParams`
//! warns a seed alone does not promise across machines or quantizations).
//!
//! Money: mirrors ride the canary's exact funding posture —
//! coordinator spend, `FundingSource::Bootstrap`, admitted under the
//! subsidy ceiling and stopped dead by its kill-switch. Mirror
//! operators are paid their full ask whatever the verdict; the
//! sample's value is the reputation row, never a clawback. One sample
//! in flight at a time bounds the spend per tick.
//!
//! Fingerprints: a mirror is a real buyer's real input re-signed by a
//! freshly generated buyer key wearing the stock MCP buyer's display
//! name — there is no probe-shaped prompt to classify this time, so a
//! freeloader can't special-case mirrors even in principle. What this
//! does NOT resist is collusion: mirror operators are picked from the
//! same open registry the source operator came from, and nothing
//! stops one entity registering all of them. The deployment's trust
//! floor (`min_operator_score_bps`) keeps PROVEN-bad operators out of
//! the mirror pool — a mirror is a measuring instrument, unlike a
//! canary probe whose whole point is targeting the floored operator —
//! but a fresh sybil starts at the neutral prior, so sybil-resistant
//! sampling still needs stake at risk (Phase 2).
//!
//! Verdicts need a strict majority (> half of participating receipts),
//! so a fault can only ever emerge from three or more participants —
//! with two, agreement is recorded and disagreement is a 1-v-1 the
//! coordinator refuses to break. Mirrors that refund or fail simply
//! don't participate; their faults were already attributed by the
//! refund machinery. A sample that can't produce a verdict writes a
//! single `agreed: None` row on the source operator — the durable
//! "sampled, inconclusive" marker a restart respects.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
use covenant_audit::AuditKind;
use covenant_compute_protocol::{
    parse_generation_params, FederationEscrow, FundingSource, JobEnvelopePayload, JobKind,
    JobOffer, RefundReason, SignedJobEnvelope,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use parking_lot::Mutex;
use uuid::Uuid;

use crate::canary::CANARY_BUYER_NAME;
use crate::jobs::{JobPhase, JobRecord};
use crate::state::CoordinatorState;

#[derive(Debug, Clone)]
pub struct RedundancyConfig {
    /// Mirrors dispatched per sample. The verdict quorum is mirrors +
    /// the source receipt, so 2 (the default) is the smallest count
    /// that can ever fault anyone.
    pub mirrors: usize,
    /// Operators asking above this are not picked as mirrors — same
    /// budget-starvation guard as the canary's.
    pub max_price_micro_usdc: u64,
    /// Cross-check deterministic inference jobs too, not just batch. Off
    /// by default: it is only sound when the operator pool returns
    /// byte-identical output for a temperature-0, seeded request (same
    /// model, quantization and hardware class). A heterogeneous pool
    /// leaves this off and samples batch alone.
    pub sample_inference: bool,
}

impl Default for RedundancyConfig {
    fn default() -> Self {
        Self {
            mirrors: 2,
            max_price_micro_usdc: 10_000,
            sample_inference: false,
        }
    }
}

/// What one [`RedundancySampler::tick`] did, returned for tests and
/// operators — the canary's `TickReport` posture.
#[derive(Debug, Default)]
pub struct SampleReport {
    /// Samples judged this tick: one entry per verdict row written.
    pub judged: Vec<SampleVerdict>,
    /// The sample dispatched this tick, if any.
    pub dispatched: Option<(Uuid, Vec<(Uuid, String)>)>,
    /// Why nothing was dispatched, when nothing was.
    pub idle_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleVerdict {
    pub source_job_id: Uuid,
    pub operator_pubkey_b58: String,
    /// `Some(false)` is the fault; `None` marks an inconclusive sample.
    pub agreed: Option<bool>,
    pub detail: String,
}

/// The sampler's durable-state mirror, seeded from the audit chain's
/// own rows — the record that survives a restart, since mirror buyer
/// identities rotate per dispatch (same contract as `CanaryBooks`).
#[derive(Default, Clone)]
struct SampleBooks {
    /// source job id → the mirrors dispatched for it.
    samples: HashMap<Uuid, Vec<(Uuid, String)>>,
    /// Every mirror job id — mirrors must never be sampled themselves.
    mirrors: HashSet<Uuid>,
    /// Sources judged to completion. In-process state only — never
    /// seeded from a lone verdict row, because the rows write
    /// non-atomically (mirrors first, the source's own row last) and a
    /// crash mid-set used to leave a partially-judged source "resolved"
    /// with its fault and slash permanently dropped. A restart
    /// re-judges instead; [`SampleBooks::written`] makes that
    /// idempotent.
    resolved: HashSet<Uuid>,
    /// Verdict rows already durably written, as (source, operator)
    /// pairs — seeded from the audit chain. A re-judge skips these, so
    /// completing a partial set writes only the missing rows and a
    /// fully-judged source re-judges to all-skipped (no reputation
    /// double-count, no double-slash).
    written: HashSet<(Uuid, String)>,
    /// Canary probe job ids — coordinator probes are not organic work
    /// worth re-buying.
    canaries: HashSet<Uuid>,
}

pub struct RedundancySampler {
    state: CoordinatorState,
    config: RedundancyConfig,
    books: Mutex<Option<SampleBooks>>,
}

impl RedundancySampler {
    pub fn new(state: CoordinatorState, config: RedundancyConfig) -> Self {
        Self {
            state,
            config,
            books: Mutex::new(None),
        }
    }

    /// One sample cycle: judge every sample whose mirrors all reached a
    /// terminal phase, then dispatch at most one new sample.
    pub async fn tick(&self) -> SampleReport {
        let mut report = SampleReport::default();
        self.judge_ready(&mut report).await;
        self.dispatch_next(&mut report).await;
        report
    }

    async fn seeded_books(&self) -> SampleBooks {
        if let Some(books) = self.books.lock().as_ref() {
            return books.clone();
        }
        // Seed once from the durable log, but only on a clean read. An
        // audit-read failure must not cache an empty history: that would
        // forget every mirror dispatched before this boot for the rest of
        // the process's life, so a sample never gets judged and an operator
        // whose receipt sat outside the majority is never slashed. Serve an
        // empty view for this tick and re-seed on the next, when the read
        // may succeed.
        let Ok(events) = self.state.audit().recent(usize::MAX).await else {
            return self.books.lock().as_ref().cloned().unwrap_or_default();
        };
        let mut seed = SampleBooks::default();
        for event in events {
            match event.kind {
                AuditKind::ComputeRedundancyDispatched {
                    source_job_id,
                    mirror_job_id,
                    operator_pubkey_b58,
                } => {
                    seed.samples
                        .entry(source_job_id)
                        .or_default()
                        .push((mirror_job_id, operator_pubkey_b58));
                    seed.mirrors.insert(mirror_job_id);
                }
                AuditKind::ComputeRedundancyResult {
                    source_job_id,
                    operator_pubkey_b58,
                    ..
                } => {
                    seed.written.insert((source_job_id, operator_pubkey_b58));
                }
                AuditKind::ComputeCanaryDispatched { job_id, .. } => {
                    seed.canaries.insert(job_id);
                }
                _ => {}
            }
        }
        let mut guard = self.books.lock();
        guard.get_or_insert_with(|| seed).clone()
    }

    async fn judge_ready(&self, report: &mut SampleReport) {
        let books = self.seeded_books().await;
        for (source_job_id, mirrors) in &books.samples {
            if books.resolved.contains(source_job_id) {
                continue;
            }
            // Every mirror must be terminal before the comparison means
            // anything — a straggler could still flip the majority.
            let mut participants: Vec<(String, String)> = Vec::new();
            let mut in_flight = false;
            for (mirror_job_id, operator) in mirrors {
                // A dispatch marker whose offer never became a durable
                // record has nothing to wait for.
                let Some(record) = self.state.jobs().get(*mirror_job_id) else {
                    continue;
                };
                match record.phase {
                    JobPhase::Offered | JobPhase::Accepted | JobPhase::AwaitingCheck => {
                        in_flight = true;
                        break;
                    }
                    JobPhase::Completed => {
                        if let Some(hash) = receipt_hash(&record) {
                            participants.push((operator.clone(), hash));
                        }
                    }
                    // Refunded/failed/rejected mirrors don't
                    // participate; their faults are already attributed
                    // where they belong.
                    JobPhase::Failed | JobPhase::Refunded | JobPhase::Rejected => {}
                }
            }
            if in_flight {
                continue;
            }
            let source = self.state.jobs().get(*source_job_id);
            let source_operator = match &source {
                Some(record) => record.operator_pubkey_b58.clone(),
                None => {
                    // The source vanished from the book (pre-journal era
                    // or a lost journal). Retire the sample instead of
                    // rescanning it forever.
                    tracing::warn!(%source_job_id, "redundancy sample has no source record; retiring");
                    self.write_verdicts(
                        report,
                        vec![SampleVerdict {
                            source_job_id: *source_job_id,
                            operator_pubkey_b58: String::new(),
                            agreed: None,
                            detail: "source job record is gone".into(),
                        }],
                        0,
                    )
                    .await;
                    continue;
                }
            };
            if let Some(record) = &source {
                if let Some(hash) = receipt_hash(record) {
                    participants.push((source_operator.clone(), hash));
                }
            }
            let source_price = source
                .as_ref()
                .map(|r| r.envelope.payload.price_micro_usdc)
                .unwrap_or(0);
            let parties: HashMap<String, Vec<String>> = participants
                .iter()
                .map(|(operator, _)| (operator.clone(), self.party_of(operator)))
                .collect();
            let verdicts = verdicts(*source_job_id, &source_operator, &participants, &parties);
            self.write_verdicts(report, verdicts, source_price).await;
        }
    }

    /// The identities that make an operator one party: its own key, and the
    /// wallets behind the stake that counts for it. Two operators sharing
    /// any of them are one party, whatever their node keys.
    fn party_of(&self, operator: &str) -> Vec<String> {
        let mut party = vec![operator.to_string()];
        if let Some(record) = self.state.registry().record(operator) {
            party.extend(record.stake_owners.iter().map(|o| format!("owner:{o}")));
        }
        party
    }

    /// `source_price_micro_usdc` sizes any slash a `Some(false)`
    /// verdict triggers: the value of the sampled work, whether the
    /// minority was the source operator or a mirror that corrupted the
    /// measurement of it.
    async fn write_verdicts(
        &self,
        report: &mut SampleReport,
        verdicts: Vec<SampleVerdict>,
        source_price_micro_usdc: u64,
    ) {
        let source_job_id = match verdicts.first() {
            Some(v) => v.source_job_id,
            None => return,
        };
        for verdict in verdicts {
            let pair = (verdict.source_job_id, verdict.operator_pubkey_b58.clone());
            let already_written = self
                .books
                .lock()
                .as_ref()
                .is_some_and(|books| books.written.contains(&pair));
            if already_written {
                // The row landed in an earlier (possibly crashed) pass.
                // Don't re-write it — reputation tallies rows with no
                // dedup — but do re-fire the idempotent slash: a crash
                // between the row and its slash would otherwise let the
                // one operator redundancy exists to catch keep their
                // stake.
                if verdict.agreed == Some(false) {
                    self.state
                        .slash_for_fault(
                            "redundancy",
                            verdict.source_job_id,
                            &verdict.operator_pubkey_b58,
                            source_price_micro_usdc,
                            &format!("redundancy minority: {}", verdict.detail),
                        )
                        .await;
                }
                continue;
            }
            self.state
                .record_audit(AuditKind::ComputeRedundancyResult {
                    source_job_id: verdict.source_job_id,
                    operator_pubkey_b58: verdict.operator_pubkey_b58.clone(),
                    agreed: verdict.agreed,
                    detail: verdict.detail.clone(),
                })
                .await;
            if let Some(books) = self.books.lock().as_mut() {
                books.written.insert(pair);
            }
            if verdict.agreed == Some(false) {
                // A strict-majority minority is a coordinator-proven
                // fault: take the sampled job's price from the bond
                // (C5 phase 2). The audit row above is the evidence.
                self.state
                    .slash_for_fault(
                        "redundancy",
                        verdict.source_job_id,
                        &verdict.operator_pubkey_b58,
                        source_price_micro_usdc,
                        &format!("redundancy minority: {}", verdict.detail),
                    )
                    .await;
            }
            tracing::info!(
                source_job_id = %verdict.source_job_id,
                operator = %verdict.operator_pubkey_b58,
                agreed = ?verdict.agreed,
                detail = %verdict.detail,
                "redundancy sample judged"
            );
            report.judged.push(verdict);
        }
        // Resolved only once the WHOLE set is on disk: a crash mid-set
        // leaves the source unresolved, and the next tick re-judges it
        // to completion through the written-pair skips above.
        if let Some(books) = self.books.lock().as_mut() {
            books.resolved.insert(source_job_id);
        }
    }

    async fn dispatch_next(&self, report: &mut SampleReport) {
        let books = self.seeded_books().await;
        if books
            .samples
            .keys()
            .any(|source| !books.resolved.contains(source))
        {
            report.idle_reason = Some("a sample is still in flight".into());
            return;
        }
        let Some((source_job_id, source)) = self.next_source(&books) else {
            report.idle_reason = Some("no unsampled released batch job".into());
            return;
        };
        let now_ms = crate::epoch_ms();
        let cutoff_ms = self.state.config().operator_liveness_timeout.as_millis() as u64;
        let model_id = &source.envelope.payload.capability_requirement.model_id;
        let kind = source.envelope.payload.kind;
        let mut candidates: Vec<(String, u64)> = self
            .state
            .registry()
            .snapshot()
            .into_iter()
            .filter(|(key, record)| {
                *key != source.operator_pubkey_b58
                    && record.status == covenant_compute_protocol::OperatorStatus::Online
                    && now_ms.saturating_sub(record.last_seen_ms) <= cutoff_ms
                    && record
                        .profile
                        .job_kinds
                        .contains(&source.envelope.payload.kind)
                    && record.profile.ask_for(kind).micro_usdc <= self.config.max_price_micro_usdc
                    && model_id.as_ref().is_none_or(|m| {
                        record
                            .profile
                            .models_for(kind)
                            .iter()
                            .any(|served| served == m || served == "any")
                    })
            })
            .map(|(key, record)| (key, record.profile.ask_for(kind).micro_usdc))
            .collect();
        // The matcher's trust floor applies to mirrors too: a mirror is
        // a measuring instrument, and letting a proven-bad operator
        // into the majority would fault honest sources on bad evidence.
        // (Canary probes deliberately ignore the floor — a probe's
        // target IS the floored operator, and passing probes are its
        // road back up. A mirror's target is the source operator.)
        let floor = self.state.config().min_operator_score_bps;
        if floor > 0 {
            let mut kept = Vec::with_capacity(candidates.len());
            for (key, ask) in candidates {
                if self.state.reputation().score(&key).await >= floor {
                    kept.push((key, ask));
                }
            }
            candidates = kept;
        }
        if candidates.is_empty() {
            report.idle_reason = Some("no eligible mirror operator".into());
            return;
        }
        // Cheapest asks first — the sample's information is the same
        // whoever serves it, so buy it at the best price. A mirror only
        // measures anything if it is a different party from the source and
        // from every other mirror, so operators staked by a wallet already
        // in the sample are passed over.
        candidates.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        let mut parties: Vec<String> = self.party_of(&source.operator_pubkey_b58);
        candidates.retain(|(key, _)| {
            let party = self.party_of(key);
            if party.iter().any(|p| parties.contains(p)) {
                return false;
            }
            parties.extend(party);
            true
        });
        candidates.truncate(self.config.mirrors);

        let mut dispatched: Vec<(Uuid, String)> = Vec::new();
        for (operator, ask) in candidates {
            match self
                .dispatch_mirror(source_job_id, &source, &operator, ask, now_ms)
                .await
            {
                Ok(mirror_job_id) => dispatched.push((mirror_job_id, operator)),
                Err(reason) => {
                    // A refused hold (subsidy ceiling) stops the whole
                    // tick — the next mirror's hold would bounce too.
                    report.idle_reason = Some(reason);
                    break;
                }
            }
        }
        if !dispatched.is_empty() {
            report.dispatched = Some((source_job_id, dispatched));
        }
    }

    /// The newest released job the sampler may cross-check that nobody
    /// sampled yet. Newest first: an old backlog says less about an
    /// operator than what it served this morning, and the books
    /// guarantee each source is bought at most once either way.
    fn next_source(&self, books: &SampleBooks) -> Option<(Uuid, JobRecord)> {
        let mut kinds = vec![JobKind::BatchJob];
        if self.config.sample_inference {
            kinds.push(JobKind::InferenceCall);
        }
        let mut sources: Vec<(Uuid, JobRecord)> = kinds
            .into_iter()
            .flat_map(|kind| self.state.jobs().completed_of_kind(kind))
            .filter(|(job_id, record)| {
                eligible_source(job_id, record, books, self.config.sample_inference)
            })
            .collect();
        sources.sort_by_key(|(_, record)| {
            std::cmp::Reverse(
                record
                    .concluded_at_ms
                    .unwrap_or(record.envelope.payload.issued_at_ms),
            )
        });
        sources.into_iter().next()
    }

    /// One mirror: the source's exact input re-signed by a fresh buyer
    /// key, held as bootstrap spend, recorded durably, then delivered —
    /// the canary's dispatch discipline step for step.
    async fn dispatch_mirror(
        &self,
        source_job_id: Uuid,
        source: &JobRecord,
        operator: &str,
        ask: u64,
        now_ms: u64,
    ) -> Result<Uuid, String> {
        let mirror_identity = LocalIdentity::generate(CANARY_BUYER_NAME);
        let mirror_job_id = Uuid::new_v4();
        let source_payload = &source.envelope.payload;
        let payload = JobEnvelopePayload {
            job_id: mirror_job_id,
            buyer: mirror_identity.agent_id(),
            kind: source_payload.kind,
            capability_requirement: source_payload.capability_requirement.clone(),
            input: source_payload.input.clone(),
            price_micro_usdc: ask,
            deadline_ms: source_payload.deadline_ms,
            idempotency: A2AIdempotency::new(
                A2ADuplicateSafety::Idempotent,
                mirror_job_id.to_string(),
            ),
            issued_at_ms: now_ms,
            referral_code: None,
            stream: false,
        };
        let envelope = SignedJobEnvelope::sign(payload, &mirror_identity)
            .map_err(|e| format!("mirror envelope signing failed: {e}"))?;

        let escrow_hold = self
            .state
            .escrow()
            .hold_with_source(
                mirror_job_id,
                &mirror_identity.agent_id(),
                ask,
                FundingSource::Bootstrap,
            )
            .await
            .map_err(|e| format!("mirror hold refused: {e}"))?;

        // A redundancy mirror is coordinator-manufactured verification
        // traffic with a synthetic buyer, funded from the bootstrap
        // subsidy held just above — not a real sale. It must not carry the
        // operator's supply-side referral code: settlement would otherwise
        // accrue a partner rev-share (C8) out of that subsidy, paying an
        // external partner for work the coordinator commissioned itself
        // (the same faucet the canary path avoids). The buyer-side code on
        // this record is `None` for the same reason.
        let payout_address = self
            .state
            .registry()
            .record(operator)
            .map(|r| r.payout_address)
            .unwrap_or_default();
        if let Err(e) = self.state.jobs().insert(
            mirror_job_id,
            JobRecord {
                operator_pubkey_b58: operator.to_string(),
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
                offered_at_ms: crate::epoch_ms(),
                pinned: true,
                accepted_at_ms: None,
                metered_elapsed_ms: None,
                close_requested_at_ms: None,
                lease_access: None,
                check_jobs: Vec::new(),
                checks_task: None,
                hidden_checks: None,
                vote_round: None,
            },
        ) {
            let _ = self
                .state
                .escrow()
                .refund(mirror_job_id, RefundReason::AdmissionFailed)
                .await;
            return Err(format!("mirror record not durable: {e}"));
        }
        self.state
            .record_audit(AuditKind::ComputeRedundancyDispatched {
                source_job_id,
                mirror_job_id,
                operator_pubkey_b58: operator.to_string(),
            })
            .await;
        if let Some(books) = self.books.lock().as_mut() {
            books
                .samples
                .entry(source_job_id)
                .or_default()
                .push((mirror_job_id, operator.to_string()));
            books.mirrors.insert(mirror_job_id);
        }
        if !self.state.registry().deliver(
            operator,
            JobOffer {
                envelope,
                escrow_hold,
            },
        ) {
            let _ = self
                .state
                .escrow()
                .refund(mirror_job_id, RefundReason::AdmissionFailed)
                .await;
            let _ = self.state.jobs().conclude_unpaid(
                mirror_job_id,
                JobPhase::Refunded,
                RefundReason::AdmissionFailed,
            );
            return Err(format!(
                "mirror operator {operator} vanished before delivery"
            ));
        }
        self.state
            .record_audit(AuditKind::ComputeJobOffered {
                job_id: mirror_job_id,
                operator_pubkey_b58: operator.to_string(),
                price_micro_usdc: ask,
                funding_source: "bootstrap".into(),
            })
            .await;
        tracing::info!(
            %source_job_id,
            %mirror_job_id,
            operator = %operator,
            price_micro_usdc = ask,
            "redundancy mirror dispatched"
        );
        Ok(mirror_job_id)
    }
}

fn receipt_hash(record: &JobRecord) -> Option<String> {
    record
        .receipt
        .as_ref()
        .map(|r| r.receipt.result_hash_hex.clone())
}

/// Whether a completed job may be bought as a redundancy source.
///
/// A pinned job is a coordinator probe — a canary, or a mirror from a
/// prior sample — never organic demand. Re-buying one spends subsidy to
/// re-run the coordinator's own work, and if the assignee already failed
/// that canary it double-counts the fault under a second slash id. The
/// book sets are a snapshot seeded once, so they can miss a canary
/// dispatched after this sampler started; `pinned` rides the durable job
/// record and can't go stale. Beyond that, an eligible source is one no
/// sample already covers, that carries a receipt hash to compare, and
/// whose kind is reproducible enough to cross-check.
fn eligible_source(
    job_id: &Uuid,
    record: &JobRecord,
    books: &SampleBooks,
    sample_inference: bool,
) -> bool {
    !record.pinned
        && !books.samples.contains_key(job_id)
        && !books.mirrors.contains(job_id)
        && !books.canaries.contains(job_id)
        && receipt_hash(record).is_some()
        && is_sampleable(
            record.envelope.payload.kind,
            &record.envelope.payload.input,
            sample_inference,
        )
}

/// Whether the sampler may cross-check this job's receipt hash at all:
/// its output must be a pure function of the signed input, so two honest
/// operators return the same bytes and a mismatch is a real fault. Batch
/// qualifies by the deployment's assertion (the module note); inference
/// qualifies only when the deployment opted it in and the request pinned
/// greedy, seeded decoding; a lease is never sampled.
fn is_sampleable(kind: JobKind, input: &[Content], sample_inference: bool) -> bool {
    match kind {
        JobKind::BatchJob => true,
        JobKind::InferenceCall => sample_inference && is_deterministic_inference(input),
        JobKind::LeaseSession => false,
        // An embedding vector is real-valued and not bit-reproducible
        // across GPUs, kernels, or quantization, so two honest operators
        // rarely return identical bytes — a hash cross-check would fault
        // them for correct work. Left unsampled until a tolerance-based
        // comparator exists.
        JobKind::Embedding => false,
        // A transcript is not bit-reproducible either: the same audio
        // yields near-identical but not byte-identical text across whisper
        // builds, thread counts, and quantization, so a hash cross-check
        // would fault honest operators. Unsampled like an embedding, until
        // a text-similarity comparator exists.
        JobKind::Transcription => false,
        // Synthesized audio is the least reproducible of all: two voices,
        // sample rates, or engine builds never return identical bytes for
        // the same text, so a hash cross-check would fault every honest
        // operator. Unsampled like a transcript, for the same reason.
        JobKind::SpeechSynthesis => false,
        // An agent's patch is never reproducible, and every agent task is
        // already cross-checked by the check job settlement waits on.
        JobKind::AgentTask | JobKind::AgentCheck => false,
    }
}

/// A `temperature: 0`, seeded generation block that asks for no
/// logprobs is the only inference input a mismatch can be trusted on —
/// greedy decoding is reproducible, so different bytes mean different
/// work, not a different sample. A malformed or absent block, a non-zero
/// temperature, or a missing seed all read as "the buyer left decoding
/// free to vary" and stay unsampled.
///
/// A request for logprobs also stays unsampled, for the reason
/// [`is_sampleable`] excludes embeddings: the reported log probabilities
/// are real-valued floats that ride the hashed output, and greedy
/// decoding reproduces the token *sequence* (argmax is robust to tiny
/// numeric drift) without reproducing those floats bit-for-bit across
/// GPUs, kernels, or quantization. The deployment's inference toggle is
/// an assertion that its pool reproduces temperature-0 text, which a
/// buyer-set logprobs knob silently changes the reproducibility class of;
/// cross-checking the hash anyway would fault an honest operator for
/// correct work.
fn is_deterministic_inference(input: &[Content]) -> bool {
    let Ok(Some(params)) = parse_generation_params(input) else {
        return false;
    };
    // Exact 0.0 is deliberate: it is the one temperature that makes
    // decoding greedy and reproducible; an epsilon band would admit
    // near-zero temperatures that still sample and would fault honest
    // operators on output they were entitled to vary.
    #[allow(clippy::float_cmp)]
    let greedy = params.temperature.is_some_and(|t| t == 0.0);
    greedy && params.seed.is_some() && params.logprobs.is_none()
}

/// The pure comparison: participants are `(operator, result_hash_hex)`
/// pairs, one per receipt (source included), and `parties` names the
/// identities each operator answers for. Votes are counted per party, not
/// per receipt: operators sharing a key or a stake wallet are merged into
/// one vote, so a party running several nodes cannot outvote an honest
/// operator, and a party whose own receipts disagree casts no vote. A
/// strict majority of parties on one hash produces a row per participant;
/// anything else produces the single inconclusive row on the source
/// operator.
fn verdicts(
    source_job_id: Uuid,
    source_operator: &str,
    participants: &[(String, String)],
    parties: &HashMap<String, Vec<String>>,
) -> Vec<SampleVerdict> {
    let votes = party_votes(participants, parties);
    if votes.len() < 2 {
        return vec![SampleVerdict {
            source_job_id,
            operator_pubkey_b58: source_operator.to_string(),
            agreed: None,
            detail: format!(
                "inconclusive: only {} independent participant(s)",
                votes.len()
            ),
        }];
    }
    let mut groups: HashMap<&str, usize> = HashMap::new();
    for hash in &votes {
        *groups.entry(hash.as_str()).or_default() += 1;
    }
    let (majority_hash, majority_count) = groups
        .iter()
        .max_by_key(|(_, count)| **count)
        .map(|(hash, count)| (*hash, *count))
        .expect("votes is non-empty");
    if majority_count * 2 <= votes.len() {
        return vec![SampleVerdict {
            source_job_id,
            operator_pubkey_b58: source_operator.to_string(),
            agreed: None,
            detail: format!(
                "inconclusive: no strict majority across {} independent participants ({} hash \
                 groups)",
                votes.len(),
                groups.len()
            ),
        }];
    }
    participants
        .iter()
        .map(|(operator, hash)| {
            let agreed = hash == majority_hash;
            SampleVerdict {
                source_job_id,
                operator_pubkey_b58: operator.clone(),
                agreed: Some(agreed),
                detail: if agreed {
                    format!(
                        "hash agreed with {majority_count}/{} independent participants",
                        votes.len()
                    )
                } else {
                    format!(
                        "hash disagreed with the majority ({majority_count}/{} on {})",
                        votes.len(),
                        &majority_hash[..majority_hash.len().min(16)]
                    )
                },
            }
        })
        .collect()
}

/// One hash per independent party. Operators are merged into a party when
/// they share any identity; a party whose receipts disagree among
/// themselves casts nothing.
fn party_votes(
    participants: &[(String, String)],
    parties: &HashMap<String, Vec<String>>,
) -> Vec<String> {
    let mut merged: Vec<(Vec<String>, Vec<&str>)> = Vec::new();
    for (operator, hash) in participants {
        let ids = parties
            .get(operator)
            .cloned()
            .unwrap_or_else(|| vec![operator.clone()]);
        let mut joined: Vec<usize> = merged
            .iter()
            .enumerate()
            .filter(|(_, (known, _))| known.iter().any(|k| ids.contains(k)))
            .map(|(i, _)| i)
            .collect();
        let mut entry = (ids, vec![hash.as_str()]);
        while let Some(i) = joined.pop() {
            let (known, hashes) = merged.remove(i);
            entry.0.extend(known);
            entry.1.extend(hashes);
        }
        merged.push(entry);
    }
    merged
        .into_iter()
        .filter_map(|(_, hashes)| {
            let first = *hashes.first()?;
            hashes
                .iter()
                .all(|h| *h == first)
                .then(|| first.to_string())
        })
        .collect()
}

/// Spawns the sampling loop; the caller decides the cadence and holds
/// the handle. Mirrors [`crate::canary::spawn_periodic_canary`].
pub fn spawn_periodic_redundancy(
    sampler: Arc<RedundancySampler>,
    interval: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            sampler.tick().await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_compute_protocol::{generation_input, GenerationParams};

    fn hashes(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(op, hash)| (op.to_string(), hash.to_string()))
            .collect()
    }

    #[test]
    fn a_strict_majority_faults_exactly_the_disagreeing_minority() {
        let source = Uuid::new_v4();
        let rows = verdicts(
            source,
            "alice",
            &hashes(&[("bob", "aa"), ("carol", "aa"), ("alice", "ff")]),
            &HashMap::new(),
        );
        assert_eq!(rows.len(), 3);
        for row in &rows {
            match row.operator_pubkey_b58.as_str() {
                "alice" => {
                    assert_eq!(row.agreed, Some(false));
                    assert!(row.detail.contains("disagreed"), "got: {}", row.detail);
                }
                _ => assert_eq!(row.agreed, Some(true)),
            }
        }
    }

    #[test]
    fn two_way_agreement_is_recorded_but_two_way_disagreement_faults_nobody() {
        let source = Uuid::new_v4();
        let agree = verdicts(
            source,
            "alice",
            &hashes(&[("alice", "aa"), ("bob", "aa")]),
            &HashMap::new(),
        );
        assert_eq!(agree.len(), 2);
        assert!(agree.iter().all(|row| row.agreed == Some(true)));

        let split = verdicts(
            source,
            "alice",
            &hashes(&[("alice", "aa"), ("bob", "ff")]),
            &HashMap::new(),
        );
        assert_eq!(split.len(), 1, "a 1-v-1 has no majority to trust");
        assert_eq!(split[0].agreed, None);
        assert_eq!(split[0].operator_pubkey_b58, "alice");
        assert!(split[0].detail.contains("no strict majority"));
    }

    #[test]
    fn an_even_split_at_four_is_inconclusive_too() {
        let rows = verdicts(
            Uuid::new_v4(),
            "alice",
            &hashes(&[
                ("alice", "aa"),
                ("bob", "aa"),
                ("carol", "ff"),
                ("dave", "ff"),
            ]),
            &HashMap::new(),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].agreed, None);
    }

    #[test]
    fn a_lone_receipt_is_insufficient() {
        let rows = verdicts(
            Uuid::new_v4(),
            "alice",
            &hashes(&[("alice", "aa")]),
            &HashMap::new(),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].agreed, None);
        assert!(rows[0].detail.contains("only 1"), "got: {}", rows[0].detail);
    }

    #[test]
    fn mirrors_staked_by_one_wallet_vote_once_and_cannot_outvote_the_source() {
        let source = Uuid::new_v4();
        let parties: HashMap<String, Vec<String>> = [
            ("alice", vec!["alice".to_string(), "owner:honest".into()]),
            ("bob", vec!["bob".to_string(), "owner:sybil".into()]),
            ("carol", vec!["carol".to_string(), "owner:sybil".into()]),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let rows = verdicts(
            source,
            "alice",
            &hashes(&[("alice", "aa"), ("bob", "ff"), ("carol", "ff")]),
            &parties,
        );
        assert_eq!(rows.len(), 1, "one party against one is no majority");
        assert_eq!(rows[0].agreed, None);
        assert!(
            !rows.iter().any(|r| r.agreed == Some(false)),
            "nobody is faulted on a sybil's say-so"
        );
    }

    #[test]
    fn one_operator_counted_twice_is_one_vote() {
        let rows = verdicts(
            Uuid::new_v4(),
            "alice",
            &hashes(&[("alice", "aa"), ("bob", "ff"), ("bob", "ff")]),
            &HashMap::new(),
        );
        assert_eq!(rows[0].agreed, None, "bob's two receipts are one voice");
    }

    #[test]
    fn a_party_that_disagrees_with_itself_casts_nothing() {
        let parties: HashMap<String, Vec<String>> = [
            ("bob", vec!["bob".to_string(), "owner:x".into()]),
            ("carol", vec!["carol".to_string(), "owner:x".into()]),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let votes = party_votes(
            &hashes(&[
                ("alice", "aa"),
                ("bob", "aa"),
                ("carol", "ff"),
                ("dave", "aa"),
            ]),
            &parties,
        );
        assert_eq!(
            votes.len(),
            2,
            "alice and dave vote; the split bob/carol party does not"
        );
    }

    fn gen_input(temperature: Option<f64>, seed: Option<i64>) -> Vec<Content> {
        let params = GenerationParams {
            temperature,
            seed,
            ..Default::default()
        };
        vec![
            Content::text("prompt"),
            generation_input(params).expect("a valid generation block"),
        ]
    }

    #[test]
    fn only_greedy_seeded_inference_is_sampleable() {
        // temperature 0 + a seed: reproducible, so a mismatch is real.
        assert!(is_sampleable(
            JobKind::InferenceCall,
            &gen_input(Some(0.0), Some(7)),
            true
        ));
        // the same job stays unsampled while the toggle is off.
        assert!(!is_sampleable(
            JobKind::InferenceCall,
            &gen_input(Some(0.0), Some(7)),
            false
        ));
        // temperature above 0 samples — output is free to vary.
        assert!(!is_sampleable(
            JobKind::InferenceCall,
            &gen_input(Some(0.7), Some(7)),
            true
        ));
        // greedy but unseeded: a backend tie-break is not pinned.
        assert!(!is_sampleable(
            JobKind::InferenceCall,
            &gen_input(Some(0.0), None),
            true
        ));
        // no generation block: the executor runs at backend defaults,
        // which the coordinator cannot assume are reproducible.
        assert!(!is_sampleable(
            JobKind::InferenceCall,
            &[Content::text("prompt")],
            true
        ));
    }

    #[test]
    fn batch_is_always_sampleable_and_a_lease_never_is() {
        assert!(is_sampleable(JobKind::BatchJob, &[], false));
        assert!(is_sampleable(JobKind::BatchJob, &[], true));
        assert!(!is_sampleable(JobKind::LeaseSession, &[], true));
    }

    #[test]
    fn an_embedding_is_never_sampleable() {
        // Real-valued vectors are not bit-reproducible across operators,
        // so a hash cross-check would fault honest work.
        assert!(!is_sampleable(JobKind::Embedding, &[], true));
        assert!(!is_sampleable(JobKind::Embedding, &[], false));
    }

    #[test]
    fn a_logprobs_request_is_never_sampleable() {
        // Reported log probabilities are real-valued floats that ride the
        // hashed output; greedy decoding reproduces the tokens but not
        // those floats across GPUs or kernels, so a hash cross-check would
        // fault honest work the same way an embedding vector would. A
        // greedy, seeded job that asks for logprobs stays unsampled; the
        // same job without logprobs is sampled, so logprobs is the
        // discriminator, not the seed or temperature.
        let with_logprobs = {
            let params = GenerationParams {
                temperature: Some(0.0),
                seed: Some(7),
                logprobs: Some(5),
                ..Default::default()
            };
            vec![
                Content::text("prompt"),
                generation_input(params).expect("a valid generation block"),
            ]
        };
        assert!(!is_sampleable(JobKind::InferenceCall, &with_logprobs, true));
        assert!(is_sampleable(
            JobKind::InferenceCall,
            &gen_input(Some(0.0), Some(7)),
            true
        ));
    }

    fn completed_batch_record(pinned: bool) -> (Uuid, JobRecord) {
        use covenant_a2a::A2ATaskStatus;
        use covenant_compute_protocol::{
            CapabilityRequirement, EscrowHoldAttestation, JobMeter, SignedWorkReceipt,
            WorkReceiptPayload,
        };
        let job_id = Uuid::new_v4();
        let buyer = LocalIdentity::generate("buyer@redundancy");
        let operator = LocalIdentity::generate("operator@redundancy");
        let coordinator = LocalIdentity::generate("coordinator@redundancy");
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
            input: vec![Content::text("echo hello")],
            price_micro_usdc: 100,
            deadline_ms: 5_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "redundancy-test"),
            issued_at_ms: 0,
            referral_code: None,
            stream: false,
        };
        let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
        let escrow_hold =
            EscrowHoldAttestation::sign(job_id, 100, FundingSource::Organic, 0, &coordinator)
                .unwrap();
        // `receipt_hash` reads `result_hash_hex` only and never verifies,
        // so a hand-built receipt is enough to make an otherwise-eligible
        // source without dragging in the node's signing path.
        let receipt = SignedWorkReceipt {
            receipt: WorkReceiptPayload {
                job_id,
                operator: operator.agent_id(),
                job_hash_hex: "00".repeat(32),
                result_hash_hex: "ab".repeat(32),
                meter: JobMeter {
                    wall_ms: 1,
                    tokens_in: None,
                    tokens_out: None,
                    gpu_seconds: None,
                    finish_reason: None,
                },
                price_micro_usdc: 100,
                status: A2ATaskStatus::Ok,
                executed_at_ms: 0,
                node_audit_root_hex: "00".repeat(32),
            },
            receipt_json: String::new(),
            signature_b58: String::new(),
            signer_pubkey_b58: operator.agent_id().pubkey_base58(),
        };
        let record = JobRecord {
            operator_pubkey_b58: operator.agent_id().pubkey_base58(),
            payout_address: "payout".into(),
            envelope,
            escrow_hold,
            phase: JobPhase::Completed,
            receipt: Some(receipt),
            output: None,
            fee_micro_usdc: 0,
            referral_code: None,
            partner_share_micro_usdc: 0,
            buyer_referral_code: None,
            buyer_partner_share_micro_usdc: 0,
            payout: None,
            concluded_at_ms: Some(1),
            refund_reason: None,
            dispute: None,
            offered_at_ms: 0,
            pinned,
            accepted_at_ms: None,
            metered_elapsed_ms: None,
            close_requested_at_ms: None,
            lease_access: None,
            check_jobs: Vec::new(),
            checks_task: None,
            hidden_checks: None,
            vote_round: None,
        };
        (job_id, record)
    }

    #[test]
    fn a_pinned_probe_is_never_bought_as_a_redundancy_source() {
        // Empty books: the stale case, as if the canary was dispatched
        // after this sampler seeded its snapshot. The pinned flag is the
        // guard that still holds when the canary set has gone stale.
        let books = SampleBooks::default();
        let (organic_id, organic) = completed_batch_record(false);
        assert!(
            eligible_source(&organic_id, &organic, &books, false),
            "an organic completed batch job with a receipt is a source"
        );
        let (probe_id, probe) = completed_batch_record(true);
        assert!(
            !eligible_source(&probe_id, &probe, &books, false),
            "a pinned coordinator probe is excluded even when the seeded canary set has missed it"
        );
    }
}
