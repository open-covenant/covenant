//! Agent work: an agent task pays only once another operator's check of
//! its result passes.
//!
//! When a builder's verified `Ok` result lands, the task is parked in
//! [`JobPhase::AwaitingCheck`] with the buyer's hold still held, and a check
//! job is ordered from an operator that is neither the builder nor staked by
//! the builder's wallet. The check is coordinator traffic, funded from the
//! bootstrap subsidy like a canary, so the buyer pays the builder's price and
//! nothing more. The task then settles on the check's verdict: a pass
//! releases the hold and pays the builder through the ordinary release path,
//! a fail refunds the buyer and faults the builder, and a check that cannot
//! be completed in time refunds the buyer without faulting anyone.
//!
//! [`settle`] is the one place a parked task concludes. It runs from a
//! periodic tick and, for a fast happy path, right after a check job's own
//! result lands; an in-process guard keeps the two from settling one task
//! twice.

use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::Duration;

use base64::Engine as _;
use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
use covenant_audit::AuditKind;
use covenant_compute_protocol::{
    agent_check_input, check_commands, parse_agent_check_verdict, parse_agent_task,
    parse_agent_task_output, sha256_hex, AgentCheckSpec, AgentCheckVerdict, AgentSkill,
    AgentTaskOutput, AgentTaskSpec, CapabilityRequirement, EscrowError, EscrowStatus,
    FederationEscrow, FundingSource, JobEnvelopePayload, JobKind, JobOffer, RefundReason,
    ResultSettlement, SignedJobEnvelope, SignedWorkReceipt,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use parking_lot::Mutex;
use uuid::Uuid;

use crate::jobs::{JobPhase, JobRecord, ReceiptAssignment, ReleaseCharges};
use crate::matcher::{select_operator_excluding, Exclusions};
use crate::rounds::{agreed, RoundVote, VoteRounds, MAX_ROUND_VOTES};
use crate::state::CoordinatorState;

/// The synthetic buyer name a check envelope is signed under.
pub(crate) const CHECK_BUYER_NAME: &str = "checker@compute";
/// Room a check needs beyond the acceptance commands themselves: matching,
/// cloning the repository, applying the patch, starting containers.
const CHECK_SETUP_MS: u64 = 300_000;
/// A check must conclude this long before the task's own deadline, so the
/// task can still settle on it.
const CHECK_MARGIN_MS: u64 = 30_000;

/// Who may buy agent work, and what checking it costs. Absent from the
/// config, every agent task is refused: opening agent work to buyers, and
/// the subsidy a check draws on, are deployment decisions.
#[derive(Debug, Clone)]
pub struct AgentPolicy {
    /// Buyer keys allowed to post agent tasks, base58; `*` admits anyone.
    pub buyers: Vec<String>,
    /// What one check pays its operator, from the bootstrap subsidy.
    pub check_price_micro_usdc: u64,
    /// How many checks one task may order before it is refunded as
    /// uncheckable. A check that ends without a verdict (refused, failed
    /// to run, timed out) is retried with another operator up to this.
    pub max_check_attempts: u32,
    /// How long an ordered check may wait for its operator to pick it up
    /// before it is withdrawn and ordered from someone else.
    pub check_accept_timeout_ms: u64,
    /// A failed verdict is confirmed by a second independent check before
    /// the builder is refused, so one checker cannot deny a builder its pay.
    pub confirm_failures: bool,
    /// The share of passing verdicts, in basis points, double-checked before
    /// payment, so a checker passing junk is caught at that rate.
    pub pass_sample_bps: u32,
}

impl AgentPolicy {
    pub fn admits(&self, buyer_b58: &str) -> bool {
        self.buyers.iter().any(|b| b == "*" || b == buyer_b58)
    }
}

fn settling() -> &'static Mutex<HashSet<Uuid>> {
    static SETTLING: OnceLock<Mutex<HashSet<Uuid>>> = OnceLock::new();
    SETTLING.get_or_init(Default::default)
}

/// Holds a builder's verified `Ok` result for a check and orders the first
/// one. A result that is not a well-formed patch against the task's commit
/// is the builder's failure: the buyer is refunded and the builder faulted,
/// with no check spent on it.
pub async fn park_result(
    state: &CoordinatorState,
    job_id: Uuid,
    record: &JobRecord,
    receipt: SignedWorkReceipt,
    output: Vec<Content>,
) -> Result<ResultSettlement, String> {
    let spec = parse_agent_task(&record.envelope.payload.input)
        .map_err(|e| format!("task input unreadable: {e}"))?;
    if let Err(defect) = built_patch(&spec, &output) {
        tracing::info!(%job_id, %defect, "agent result is not a usable patch; refunding");
        state
            .jobs()
            .set_receipt_and_phase(
                job_id,
                receipt,
                output,
                ReleaseCharges::default(),
                JobPhase::Failed,
                Some(RefundReason::ExecutionFailed),
                ReceiptAssignment {
                    operator_pubkey_b58: record.operator_pubkey_b58.clone(),
                    payout_address: record.payout_address.clone(),
                    metered_elapsed_ms: None,
                },
            )
            .map_err(|e| e.to_string())?;
        refund_hold(state, job_id, RefundReason::ExecutionFailed).await;
        state
            .record_audit(AuditKind::ComputeJobRefunded {
                job_id,
                reason: RefundReason::ExecutionFailed.as_str().into(),
                operator_pubkey_b58: Some(record.operator_pubkey_b58.clone()),
            })
            .await;
        return Ok(ResultSettlement::Refunded);
    }
    let parked = state
        .jobs()
        .park_for_check(job_id, receipt, output, &record.operator_pubkey_b58)
        .map_err(|e| e.to_string())?;
    if !parked {
        return Err(format!(
            "job {job_id} concluded before its result could be held"
        ));
    }
    if let Err(e) = order_check(state, job_id).await {
        // The settle tick retries the order until the task's deadline.
        tracing::warn!(%job_id, error = %e, "first check not ordered yet");
    }
    Ok(ResultSettlement::AwaitingCheck)
}

/// Reads the builder's patch and confirms it is the patch it claims to be,
/// against the commit the task named.
fn built_patch(spec: &AgentTaskSpec, output: &[Content]) -> Result<AgentTaskOutput, String> {
    let built = parse_agent_task_output(output).map_err(|e| e.to_string())?;
    if built.base_commit != spec.repo.commit() {
        return Err(format!(
            "patch is against {} but the task named {}",
            built.base_commit,
            spec.repo.commit()
        ));
    }
    let patch = base64::engine::general_purpose::STANDARD
        .decode(&built.patch_b64)
        .map_err(|e| format!("patch is not base64: {e}"))?;
    if sha256_hex(&patch) != built.patch_sha256 {
        return Err("patch does not match its digest".into());
    }
    Ok(built)
}

/// Orders a check of a parked task's patch from an operator independent of
/// its builder and of every earlier checker. Fails without side effects when
/// no such operator is available, the subsidy refuses the hold, or too
/// little of the task's window is left for a check to finish.
pub async fn order_check(state: &CoordinatorState, task_id: Uuid) -> Result<Uuid, String> {
    let policy = state
        .config()
        .agent
        .clone()
        .ok_or("agent work is not configured")?;
    let task = state.jobs().get(task_id).ok_or("no such task")?;
    if task.phase != JobPhase::AwaitingCheck {
        return Err(format!(
            "task is {}, not awaiting a check",
            task.phase.as_str()
        ));
    }
    if task.check_jobs.len() >= policy.max_check_attempts as usize {
        return Err(format!(
            "{} checks ordered already, the most this task may have",
            task.check_jobs.len()
        ));
    }
    let spec = parse_agent_task(&task.envelope.payload.input).map_err(|e| e.to_string())?;
    let built = built_patch(&spec, task.output.as_deref().unwrap_or_default())?;
    if spec.acceptance.hidden_sha256.is_some() && task.hidden_checks.is_none() {
        return Err("waiting for the buyer's hidden checks".into());
    }

    let now_ms = crate::epoch_ms();
    let task_deadline_ms = task
        .envelope
        .payload
        .issued_at_ms
        .saturating_add(task.envelope.payload.deadline_ms);
    let window_ms = u64::from(spec.acceptance.timeout_secs) * 1_000 + CHECK_SETUP_MS;
    let left_ms = task_deadline_ms.saturating_sub(now_ms);
    if left_ms < window_ms + CHECK_MARGIN_MS {
        return Err(format!(
            "{left_ms}ms of the task's window is left, short of the {window_ms}ms a check needs"
        ));
    }

    let mut exclusions = Exclusions {
        operators: vec![task.operator_pubkey_b58.clone()],
        stake_owners: state
            .registry()
            .record(&task.operator_pubkey_b58)
            .map(|r| r.stake_owners)
            .unwrap_or_default(),
    };
    for earlier in &task.check_jobs {
        if let Some(check) = state.jobs().get(*earlier) {
            exclusions.operators.push(check.operator_pubkey_b58);
        }
    }
    let requirement = CapabilityRequirement {
        gpu_class: None,
        min_vram_gb: None,
        model_id: Some(spec.acceptance.image.clone()),
        kind: JobKind::AgentCheck,
        max_duration_secs: u32::try_from(window_ms / 1_000).unwrap_or(u32::MAX),
        min_reputation_bps: None,
    };
    let config = state.config();
    let checker = select_operator_excluding(
        state.registry(),
        state.reputation(),
        state.bonds(),
        &requirement,
        policy.check_price_micro_usdc,
        now_ms,
        config.operator_liveness_timeout,
        config.min_operator_score_bps,
        config.bond_floor(),
        &exclusions,
    )
    .await
    .ok_or("no independent operator can check this task right now")?;

    let check_id = Uuid::new_v4();
    let identity = LocalIdentity::generate(CHECK_BUYER_NAME);
    let input = agent_check_input(AgentCheckSpec {
        task_job_id: task_id,
        repo: spec.repo.clone(),
        acceptance: spec.acceptance.clone(),
        patch_b64: built.patch_b64.clone(),
        patch_sha256: built.patch_sha256.clone(),
        hidden: task.hidden_checks.clone(),
    })
    .map_err(|e| format!("check input: {e}"))?;
    let payload = JobEnvelopePayload {
        job_id: check_id,
        buyer: identity.agent_id(),
        kind: JobKind::AgentCheck,
        capability_requirement: requirement,
        input: vec![input],
        price_micro_usdc: policy.check_price_micro_usdc,
        // The check's own window, not the task's remainder: a checker that
        // stalls times out while there is still room to order another.
        deadline_ms: window_ms.min(left_ms - CHECK_MARGIN_MS),
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, check_id.to_string()),
        issued_at_ms: now_ms,
        referral_code: None,
        stream: false,
    };
    payload
        .validate_input()
        .map_err(|e| format!("check envelope: {e}"))?;
    let envelope = SignedJobEnvelope::sign(payload, &identity)
        .map_err(|e| format!("check envelope signing failed: {e}"))?;
    let escrow_hold = state
        .escrow()
        .hold_with_source(
            check_id,
            &identity.agent_id(),
            policy.check_price_micro_usdc,
            FundingSource::Bootstrap,
        )
        .await
        .map_err(|e| format!("check hold refused: {e}"))?;

    // Coordinator-ordered verification traffic, like a canary: no referral
    // code on either side, so no partner accrues a share of the subsidy.
    let payout_address = state
        .registry()
        .record(&checker)
        .map(|r| r.payout_address)
        .unwrap_or_default();
    let inserted = state.jobs().insert(
        check_id,
        JobRecord {
            operator_pubkey_b58: checker.clone(),
            payout_address,
            envelope: envelope.clone(),
            escrow_hold: escrow_hold.clone(),
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
            offered_at_ms: crate::epoch_ms(),
            pinned: true,
            accepted_at_ms: None,
            metered_elapsed_ms: None,
            close_requested_at_ms: None,
            lease_access: None,
            check_jobs: Vec::new(),
            checks_task: Some(task_id),
            hidden_checks: None,
            vote_round: None,
        },
    );
    if let Err(e) = inserted {
        refund_hold(state, check_id, RefundReason::AdmissionFailed).await;
        return Err(format!("check record not durable: {e}"));
    }
    if let Err(e) = state.jobs().add_check_job(task_id, check_id) {
        refund_hold(state, check_id, RefundReason::AdmissionFailed).await;
        let _ = state.jobs().conclude_unpaid(
            check_id,
            JobPhase::Refunded,
            RefundReason::AdmissionFailed,
        );
        return Err(format!("check link not durable: {e}"));
    }
    if !state.registry().deliver(
        &checker,
        JobOffer {
            envelope,
            escrow_hold,
        },
    ) {
        refund_hold(state, check_id, RefundReason::AdmissionFailed).await;
        let _ = state.jobs().conclude_unpaid(
            check_id,
            JobPhase::Refunded,
            RefundReason::AdmissionFailed,
        );
        return Err(format!("checker {checker} vanished before delivery"));
    }
    state
        .record_audit(AuditKind::ComputeJobOffered {
            job_id: check_id,
            operator_pubkey_b58: checker.clone(),
            price_micro_usdc: policy.check_price_micro_usdc,
            funding_source: "bootstrap".into(),
        })
        .await;
    tracing::info!(%task_id, %check_id, checker = %checker, "agent check ordered");
    Ok(check_id)
}

/// Settles one parked task from the state of its latest check. A no-op for
/// a task that is not parked, or that another caller is settling right now.
pub async fn settle(state: &CoordinatorState, task_id: Uuid) {
    if !settling().lock().insert(task_id) {
        return;
    }
    settle_once(state, task_id).await;
    settling().lock().remove(&task_id);
}

async fn settle_once(state: &CoordinatorState, task_id: Uuid) {
    let Some(task) = state.jobs().get(task_id) else {
        return;
    };
    if task.phase != JobPhase::AwaitingCheck {
        return;
    }
    let Some(policy) = state.config().agent.clone() else {
        return;
    };
    let now_ms = crate::epoch_ms();
    let past_deadline = task
        .envelope
        .payload
        .issued_at_ms
        .saturating_add(task.envelope.payload.deadline_ms)
        < now_ms;
    let checks: Vec<JobRecord> = task
        .check_jobs
        .iter()
        .filter_map(|id| state.jobs().get(*id))
        .collect();

    if let Some(latest) = checks.last() {
        let out = matches!(latest.phase, JobPhase::Offered | JobPhase::Accepted);
        if latest.phase == JobPhase::Offered && unclaimed(state, latest, now_ms) {
            if !withdraw(state, task_id, latest).await {
                return;
            }
        } else if out && !past_deadline {
            return;
        }
    }

    let judged: Vec<(String, AgentCheckVerdict)> = checks
        .iter()
        .filter(|c| c.phase == JobPhase::Completed)
        .filter_map(|c| match judge(&task, c) {
            Ok(verdict) => Some((c.operator_pubkey_b58.clone(), verdict)),
            Err(defect) => {
                tracing::warn!(%task_id, check_id = %c.envelope.payload.job_id, %defect, "check returned no usable verdict");
                None
            }
        })
        .collect();
    let verdicts: Vec<(String, bool)> = judged
        .iter()
        .map(|(checker, verdict)| (checker.clone(), verdict.passed))
        .collect();

    let outcome = match decide(task_id, &verdicts, &policy) {
        Decision::Pay => Some(true),
        Decision::Refund => Some(false),
        Decision::Another { fallback } if past_deadline => fallback,
        Decision::Another { fallback } => match order_check(state, task_id).await {
            Ok(_) => return,
            Err(e) => {
                let exhausted = task.check_jobs.len() >= policy.max_check_attempts as usize
                    || e.contains("window is left");
                match fallback {
                    Some(settled) => {
                        tracing::info!(%task_id, error = %e, "no confirming check available; settling on the one verdict");
                        Some(settled)
                    }
                    None if exhausted => None,
                    None => {
                        tracing::info!(%task_id, error = %e, "check not ordered yet; will retry");
                        return;
                    }
                }
            }
        },
    };
    let outcome = match state.vote_rounds() {
        Some(rounds) if !judged.is_empty() => {
            counted(state, rounds, task_id, &task, &judged, outcome).await
        }
        _ => outcome,
    };
    match outcome {
        Some(true) => {
            if release(state, task_id, &task).await {
                record_agreement(state, task_id, &verdicts, true).await;
            }
        }
        Some(false) => {
            tracing::info!(%task_id, verdicts = ?verdicts, "agent work failed its check");
            if refund_task(state, task_id, &task, RefundReason::CheckFailed).await {
                record_agreement(state, task_id, &verdicts, false).await;
            }
        }
        None => {
            tracing::warn!(%task_id, verdicts = ?verdicts, "no check could be completed; refunding");
            refund_task(state, task_id, &task, RefundReason::CheckUnavailable).await;
        }
    }
}

/// Lets the chain count the checkers' votes before anything moves, and
/// settles on what the chain and the coordinator agree on. A round that
/// cannot run leaves the coordinator's decision standing. A round that
/// already ran for this task is read back from the record, not run again.
async fn counted(
    state: &CoordinatorState,
    rounds: &dyn VoteRounds,
    task_id: Uuid,
    task: &JobRecord,
    judged: &[(String, AgentCheckVerdict)],
    decided: Option<bool>,
) -> Option<bool> {
    let record = match task.vote_round.clone() {
        Some(record) => record,
        None => {
            let Some(votes) = signed_votes(judged) else {
                tracing::info!(%task_id, "not every vote carries its checker's signature; settling on the coordinator's count");
                return decided;
            };
            match rounds.run(task_id, &judged[0].1.patch_sha256, &votes).await {
                Ok(record) => {
                    if let Err(e) = state.jobs().set_vote_round(task_id, record.clone()) {
                        tracing::warn!(%task_id, error = %e, "could not pin the round on the task");
                    }
                    record
                }
                Err(e) => {
                    tracing::warn!(%task_id, error = %e, "vote round unavailable; settling on the coordinator's count");
                    return decided;
                }
            }
        }
    };
    let settled = agreed(decided, record.result);
    if settled == decided {
        tracing::info!(%task_id, counted = ?record.result, round = %record.round, settle = %record.signature, "votes counted on chain");
    } else {
        tracing::error!(%task_id, ?decided, counted = ?record.result, round = %record.round, "the chain's count and the coordinator's decision disagree; nobody is paid");
    }
    settled
}

/// Every verdict as a vote its checker signed, or `None` if any is unsigned
/// or there are more than a round holds.
fn signed_votes(judged: &[(String, AgentCheckVerdict)]) -> Option<Vec<RoundVote>> {
    if judged.len() > MAX_ROUND_VOTES {
        return None;
    }
    judged
        .iter()
        .map(|(checker, verdict)| {
            let signature = verdict
                .vote_signature_b58
                .clone()
                .filter(|_| verdict.vote_signed_by(checker))?;
            Some(RoundVote {
                voter: checker.clone(),
                passed: verdict.passed,
                signature,
            })
        })
        .collect()
}

/// What the verdicts so far settle a task as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Pay,
    Refund,
    /// Another check is wanted. If none can be had, `fallback` settles it:
    /// the one verdict there is, or with split or absent evidence, nobody
    /// paid and nobody faulted.
    Another {
        fallback: Option<bool>,
    },
}

/// One verdict decides unless it is a fail the policy confirms, or a pass
/// the policy samples. Two or more decide by majority; a tie asks for one
/// more.
fn decide(task_id: Uuid, verdicts: &[(String, bool)], policy: &AgentPolicy) -> Decision {
    let passes = verdicts.iter().filter(|(_, passed)| *passed).count();
    let fails = verdicts.len() - passes;
    match verdicts.len() {
        0 => Decision::Another { fallback: None },
        1 if passes == 1 && sampled(task_id, policy.pass_sample_bps) => Decision::Another {
            fallback: Some(true),
        },
        1 if passes == 1 => Decision::Pay,
        1 if policy.confirm_failures => Decision::Another {
            fallback: Some(false),
        },
        1 => Decision::Refund,
        _ if passes > fails => Decision::Pay,
        _ if fails > passes => Decision::Refund,
        _ => Decision::Another { fallback: None },
    }
}

/// Whether this task's single pass is one the policy double-checks.
/// Derived from the task id, so a retry of the decision never flips it.
fn sampled(task_id: Uuid, bps: u32) -> bool {
    task_id.as_u128() % 10_000 < u128::from(bps)
}

/// Withdraws a check its operator never picked up: the coordinator is its
/// buyer, so this is a cancel and nobody's fault.
async fn withdraw(state: &CoordinatorState, task_id: Uuid, check: &JobRecord) -> bool {
    let check_id = check.envelope.payload.job_id;
    match state.jobs().cancel_if_offered(check_id) {
        Ok(true) => {
            refund_hold(state, check_id, RefundReason::BuyerCancelled).await;
            state
                .record_audit(AuditKind::ComputeJobRefunded {
                    job_id: check_id,
                    reason: RefundReason::BuyerCancelled.as_str().into(),
                    operator_pubkey_b58: None,
                })
                .await;
            tracing::info!(%task_id, %check_id, checker = %check.operator_pubkey_b58, "check not picked up; withdrawn");
            true
        }
        Ok(false) => false,
        Err(e) => {
            tracing::warn!(%task_id, %check_id, error = %e, "withdrawing an unclaimed check failed");
            false
        }
    }
}

/// With more than one verdict, each checker's agreement with the outcome is
/// a reputation row: the checker in the minority took a position the other
/// independent checks contradicted.
async fn record_agreement(
    state: &CoordinatorState,
    task_id: Uuid,
    verdicts: &[(String, bool)],
    paid: bool,
) {
    if verdicts.len() < 2 {
        return;
    }
    for (checker, passed) in verdicts {
        let agreed = *passed == paid;
        state
            .record_audit(AuditKind::ComputeRedundancyResult {
                source_job_id: task_id,
                operator_pubkey_b58: checker.clone(),
                agreed: Some(agreed),
                detail: if agreed {
                    "agent check agreed with the independent checks".into()
                } else {
                    "agent check contradicted by the independent checks".into()
                },
            })
            .await;
    }
}

/// Whether an offered check has waited past the policy's pickup window,
/// measured from when it was ordered: redelivery re-stamps the offer time,
/// the order time never moves.
fn unclaimed(state: &CoordinatorState, check: &JobRecord, now_ms: u64) -> bool {
    let timeout = state
        .config()
        .agent
        .as_ref()
        .map(|p| p.check_accept_timeout_ms)
        .unwrap_or(u64::MAX);
    now_ms.saturating_sub(check.envelope.payload.issued_at_ms) > timeout
}

/// The verdict a completed check returned, if it is a verdict on exactly
/// this task's patch and, when it passes, ran exactly the task's commands.
fn judge(task: &JobRecord, check: &JobRecord) -> Result<AgentCheckVerdict, String> {
    let spec = parse_agent_task(&task.envelope.payload.input).map_err(|e| e.to_string())?;
    let built = parse_agent_task_output(task.output.as_deref().unwrap_or_default())
        .map_err(|e| e.to_string())?;
    let verdict = parse_agent_check_verdict(check.output.as_deref().unwrap_or_default())
        .map_err(|e| e.to_string())?;
    if Some(verdict.task_job_id) != check.checks_task {
        return Err("verdict names another task".into());
    }
    if verdict.patch_sha256 != built.patch_sha256 {
        return Err("verdict is on another patch".into());
    }
    if verdict.skill != spec.acceptance.skill {
        return Err("verdict judges another skill than the task asked for".into());
    }
    if verdict.passed {
        let ran = |outcomes: &[covenant_compute_protocol::CommandOutcome]| -> Vec<String> {
            outcomes.iter().map(|c| c.command.clone()).collect()
        };
        let visible = spec.acceptance.commands.clone();
        let every: Vec<String> = check_commands(&spec.acceptance, task.hidden_checks.as_ref())
            .into_iter()
            .map(str::to_string)
            .collect();
        let honest = match spec.acceptance.skill {
            AgentSkill::CodeChange => ran(&verdict.commands) == every,
            // The tests' run stops at the failure that caught the bug, so
            // it is a prefix of the visible commands rather than all of them.
            AgentSkill::CodeTests => {
                ran(&verdict.baseline) == visible
                    && visible.starts_with(&ran(&verdict.commands))
                    && ran(&verdict.fixed) == every
            }
        };
        if !honest {
            return Err("a passing verdict did not run the task's commands".into());
        }
    }
    Ok(verdict)
}

async fn release(state: &CoordinatorState, task_id: Uuid, task: &JobRecord) -> bool {
    let (Some(receipt), Some(output)) = (task.receipt.clone(), task.output.clone()) else {
        tracing::error!(%task_id, "parked task has no receipt to release on");
        return false;
    };
    if let Err(e) = state.escrow().release(task_id, &receipt).await {
        let released = matches!(e, EscrowError::AlreadySettled(_))
            && matches!(
                state.escrow().status(task_id).await,
                Ok(EscrowStatus::Released)
            );
        if !released {
            tracing::error!(%task_id, error = %e, "passing check, but the hold would not release");
            return false;
        }
    }
    let (amount, funding_source) = state.escrow().hold_info(task_id).unwrap_or((
        receipt.receipt.price_micro_usdc,
        state.config().default_funding_source,
    ));
    if let Err(e) = crate::http::finish_release(
        state,
        task_id,
        task,
        receipt,
        output,
        amount,
        funding_source,
        None,
        crate::onchain_meter::LeaseConclusion::OffChain,
    )
    .await
    {
        tracing::error!(%task_id, error = ?e, "check passed and the hold released, but concluding the task failed");
        return false;
    }
    tracing::info!(%task_id, amount_micro_usdc = amount, "agent work passed its check; paid");
    true
}

/// Concludes a parked task unpaid: the record first, so a crash before the
/// refund lands leaves boot recovery the reason to refund it with. A failed
/// check is the builder's fault; a check that could not be completed is not.
async fn refund_task(
    state: &CoordinatorState,
    task_id: Uuid,
    task: &JobRecord,
    reason: RefundReason,
) -> bool {
    match state.jobs().conclude_parked_unpaid(task_id, reason) {
        Ok(true) => {}
        Ok(false) => return false,
        Err(e) => {
            tracing::error!(%task_id, error = %e, "concluding a parked task failed");
            return false;
        }
    }
    refund_hold(state, task_id, reason).await;
    let operator_pubkey_b58 =
        (reason == RefundReason::CheckFailed).then(|| task.operator_pubkey_b58.clone());
    state
        .record_audit(AuditKind::ComputeJobRefunded {
            job_id: task_id,
            reason: reason.as_str().into(),
            operator_pubkey_b58,
        })
        .await;
    true
}

async fn refund_hold(state: &CoordinatorState, job_id: Uuid, reason: RefundReason) {
    if let Err(e) = state.escrow().refund(job_id, reason).await {
        if !matches!(e, EscrowError::AlreadySettled(_)) {
            tracing::warn!(%job_id, error = %e, "refund deferred to boot reconcile");
        }
    }
}

/// Takes a task's hidden checks from whoever holds them. Knowing a set whose
/// digest matches the buyer-signed commitment is the credential, so this
/// needs no signature of its own; a set that does not match is refused.
pub async fn accept_hidden_checks(
    state: &CoordinatorState,
    task_id: Uuid,
    hidden: covenant_compute_protocol::HiddenChecks,
) -> Result<(), String> {
    let task = state.jobs().get(task_id).ok_or("no such task")?;
    if task.envelope.payload.kind != JobKind::AgentTask {
        return Err("only an agent task takes hidden checks".into());
    }
    let spec = parse_agent_task(&task.envelope.payload.input).map_err(|e| e.to_string())?;
    let commitment = spec
        .acceptance
        .hidden_sha256
        .clone()
        .ok_or("this task committed to no hidden checks")?;
    hidden.validate().map_err(|e| e.to_string())?;
    spec.acceptance
        .admits_hidden(&hidden)
        .map_err(|e| e.to_string())?;
    if hidden.digest() != commitment {
        return Err("these checks do not match the task's commitment".into());
    }
    if !state
        .jobs()
        .set_hidden_checks(task_id, hidden)
        .map_err(|e| e.to_string())?
    {
        return Err("the task already holds different hidden checks".into());
    }
    if task.phase == JobPhase::AwaitingCheck {
        let state = state.clone();
        tokio::spawn(async move { settle(&state, task_id).await });
    }
    Ok(())
}

/// The latest check's verdict on a task, for the buyer's status view.
pub fn latest_verdict(state: &CoordinatorState, task: &JobRecord) -> Option<AgentCheckVerdict> {
    let check = state.jobs().get(*task.check_jobs.last()?)?;
    parse_agent_check_verdict(check.output.as_deref()?).ok()
}

/// Settles every parked task on a fixed tick, for checks whose result no
/// nudge reached (a coordinator restart, a check that ended without one).
pub fn spawn_periodic_settle(state: CoordinatorState, interval: Duration) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            // Each settle may wait on a vote round, so none waits on another.
            for (task_id, _) in state.jobs().awaiting_check() {
                let state = state.clone();
                tokio::spawn(async move { settle(&state, task_id).await });
            }
        }
    });
}

/// Hands a check job's conclusion straight to its task, so a passing check
/// pays the builder now rather than on the next tick.
pub fn nudge(state: &CoordinatorState, check: &JobRecord) {
    if check.envelope.payload.kind != JobKind::AgentCheck {
        return;
    }
    let Some(task_id) = check.checks_task else {
        return;
    };
    let state = state.clone();
    tokio::spawn(async move { settle(&state, task_id).await });
}
