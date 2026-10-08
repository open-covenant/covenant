//! The coordinator's own per-job lifecycle ledger — distinct from
//! [`crate::escrow::CustodialEscrow`]'s fund-hold bookkeeping. This is
//! "which operator is this job assigned to, and where is it in its
//! lifecycle" (routing state); the escrow is "is money held, released,
//! or refunded" (fund state). The two move together but are checked
//! independently, matching the `FederationEscrow` trait boundary: this
//! ledger never reaches into escrow internals, only calls its public
//! methods.

use std::collections::HashMap;
use std::sync::Arc;

use covenant_compute_protocol::{
    DisputeRequest, EscrowHoldAttestation, RefundReason, SignedJobEnvelope, SignedWorkReceipt,
};
use covenant_mcp::Content;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::journal::Journal;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobPhase {
    Offered,
    Accepted,
    Rejected,
    Completed,
    /// The operator submitted a verified non-`Ok` receipt: the job ran
    /// and failed. The buyer's hold is refunded, the receipt is kept as
    /// the operator's own signed evidence of the failure.
    Failed,
    Refunded,
    /// An agent task's verified result, held while another operator
    /// checks it. The buyer's hold stays held: the task pays only once a
    /// check passes, and goes back if it fails or none can be completed.
    AwaitingCheck,
}

impl JobPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            JobPhase::Offered => "offered",
            JobPhase::Accepted => "accepted",
            JobPhase::Rejected => "rejected",
            JobPhase::Completed => "completed",
            JobPhase::Failed => "failed",
            JobPhase::Refunded => "refunded",
            JobPhase::AwaitingCheck => "awaiting_check",
        }
    }
}

/// One-pass aggregate of the book: jobs per phase plus how many carry
/// a dispute (a dispute pins to a job in any concluded phase, so it is
/// a separate axis, not a seventh phase).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JobStats {
    pub offered: usize,
    pub accepted: usize,
    pub rejected: usize,
    pub completed: usize,
    pub failed: usize,
    pub refunded: usize,
    pub awaiting_check: usize,
    pub disputed: usize,
}

/// The payout push that honored a release, pinned onto the job record
/// the moment the backend accepts the transfer. Its absence on a
/// `Completed` job is load-bearing: escrow released but the push
/// failed (or the coordinator died first) — money the coordinator
/// still owes, and exactly what the operator-facing books must show
/// as outstanding rather than quietly forget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PayoutOutcome {
    /// The operator's net actually transferred (gross minus fee).
    pub amount_micro_usdc: u64,
    /// The on-chain transaction, when the backend submitted one.
    /// `None` for backends that record intent without touching a chain.
    pub tx_signature: Option<String>,
    pub recorded_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub operator_pubkey_b58: String,
    /// The payout address the assigned operator had declared when it
    /// won the match — captured here because the registry is rebuilt
    /// empty on restart, and the address agreed at assignment is the
    /// one the release pays.
    #[serde(default)]
    pub payout_address: String,
    pub envelope: SignedJobEnvelope,
    pub escrow_hold: EscrowHoldAttestation,
    pub phase: JobPhase,
    pub receipt: Option<SignedWorkReceipt>,
    /// The job's actual output, kept only once it hash-verified against
    /// the receipt's `result_hash_hex` — what the buyer's receipt poll
    /// hands back.
    pub output: Option<Vec<Content>>,
    /// The marketplace fee withheld from this job's payout push, set
    /// when the release completes — the rate can change across
    /// coordinator restarts, so the amount actually taken is pinned
    /// per job rather than re-derived from config. Zero for refunded,
    /// failed and pre-fee-era jobs.
    #[serde(default)]
    pub fee_micro_usdc: u64,
    /// The signed partner attribution the winning operator registered
    /// with, captured at assignment for the same reason as
    /// `payout_address`: the registry restarts empty, and the partner
    /// whose referral won this job is the one the accrual credits.
    #[serde(default)]
    pub referral_code: Option<String>,
    /// The rev-share accrued to that partner out of `fee_micro_usdc`
    /// when the release completed — pinned per job, like the fee.
    #[serde(default)]
    pub partner_share_micro_usdc: u64,
    /// The demand-side attribution: the referral code the buyer signed
    /// inside the job envelope itself. Copied out at submission so the
    /// accrual survives however the envelope is stored or migrated.
    #[serde(default)]
    pub buyer_referral_code: Option<String>,
    /// The rev-share accrued to the buyer's partner, carved from what
    /// was left of `fee_micro_usdc` after the supply-side share.
    #[serde(default)]
    pub buyer_partner_share_micro_usdc: u64,
    /// The payout push that honored this job's release. `None` until
    /// the backend accepts the transfer — and permanently `None` on a
    /// completed job whose push failed, which is what the operator
    /// books read as money still owed.
    #[serde(default)]
    pub payout: Option<PayoutOutcome>,
    /// Coordinator-clock time the receipt landed and the job settled
    /// into `Completed`/`Failed` — the dispute window's anchor. The
    /// receipt's own `executed_at_ms` is the operator's claim and
    /// would let an operator pre-date a receipt to shrink the window.
    /// `None` on pre-upgrade journal rows and jobs that never got a
    /// receipt; the window check falls back to the buyer-signed
    /// `issued_at_ms + deadline_ms`.
    #[serde(default)]
    pub concluded_at_ms: Option<u64>,
    /// Why this job's money went back, pinned by the conclusion that
    /// refunded it — the audit stream's `reason`, made a fact of the
    /// record so both parties' reads can serve it. `None` while the
    /// job lives, on jobs that paid out, and on journal rows from
    /// before the field existed (including a crash-window record whose
    /// escrow settled first: the boot reconcile can't know why money
    /// it didn't move moved).
    #[serde(default)]
    pub refund_reason: Option<RefundReason>,
    /// The buyer's signed dispute of this completed job, kept verbatim
    /// (C4) — self-verifying evidence, same posture as `receipt`. At
    /// most one per job; the phase stays `Completed` because the money
    /// moved and the receipt stands, so a dispute is an annotation
    /// reputation reads, not a lifecycle state.
    #[serde(default)]
    pub dispute: Option<DisputeRequest>,
    /// Coordinator-clock time the current offer went out — stamped at
    /// assignment and re-stamped whenever the job is re-offered, so the
    /// stale-offer sweep ages the offer actually in flight, not the
    /// submission. Zero on pre-upgrade journal rows and never-matched
    /// records, which reads as maximally stale — exactly right after a
    /// restart, since the in-memory delivery queues died with the old
    /// process and every restored offer needs redelivering anyway.
    #[serde(default)]
    pub offered_at_ms: u64,
    /// This job exists to probe its assignee (canary, redundancy
    /// mirror): the operator is the point, so the stale-offer sweep
    /// must never re-match it — a probe answered by someone else would
    /// be judged against the wrong operator.
    #[serde(default)]
    pub pinned: bool,
    /// Coordinator-clock time the assignee accepted — the moment a
    /// lease session's meter starts. Coordinator-observed on purpose:
    /// the operator's own claims never size a lease bill. `None` on
    /// pre-upgrade journal rows and jobs concluded without an accept
    /// (a lease among them bills nothing).
    #[serde(default)]
    pub accepted_at_ms: Option<u64>,
    /// The elapsed run a lease settlement actually billed, pinned at
    /// conclusion so the charge on the receipt view is explainable
    /// forever — `LeaseTerms::metered_micro_usdc(this)` is the exact
    /// released amount. `None` for every non-lease job.
    #[serde(default)]
    pub metered_elapsed_ms: Option<u64>,
    /// The buyer asked to end this lease session. Set by the close
    /// endpoint while the session runs; the serving node sees it on its
    /// next poll and shuts the session down, which submits the receipt
    /// and settles the meter. Durable, so a coordinator restart mid
    /// session does not lose the buyer's instruction — the meter is
    /// still running and still costing them.
    #[serde(default)]
    pub close_requested_at_ms: Option<u64>,
    /// Where the running session can be reached, as published by the
    /// serving node. Pinned on the record so a buyer that lost the
    /// stream (or restarted their client) can still find the machine
    /// they are paying for.
    #[serde(default)]
    pub lease_access: Option<covenant_compute_protocol::LeaseAccess>,
    /// On an agent task: the check jobs ordered for its result, latest
    /// last. Only the latest is live; earlier ones ran without a verdict.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub check_jobs: Vec<Uuid>,
    /// On a check job: the agent task whose result it checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks_task: Option<Uuid>,
    /// On an agent task: the buyer's hidden checks, once handed over and
    /// matched against the task's commitment. Only ever relayed to checkers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden_checks: Option<covenant_compute_protocol::HiddenChecks>,
    /// On an agent task: what the chain counted when its checkers' votes
    /// went through a round.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vote_round: Option<crate::rounds::RoundRecord>,
}

impl JobRecord {
    /// What escrow released for this job — the gross the operator's net
    /// and the marketplace fee are carved from.
    ///
    /// For a metered lease this is the seconds the session actually ran,
    /// the same figure [`crate::escrow::CustodialEscrow::release_metered`]
    /// wrote down onto the hold at settlement:
    /// `LeaseTerms::metered_micro_usdc` of the pinned `metered_elapsed_ms`.
    /// Every other kind releases its whole envelope price. The distinction
    /// is money — a lease escrows its window's ceiling but is owed only
    /// the seconds used, so sizing a payout from the envelope price would
    /// pay the ceiling and overdraw the buyer's refunded remainder.
    ///
    /// This is the job record's own account of the released gross: what
    /// `payout_totals` reports as still owed, and the fallback the retry
    /// sweep sizes from when the authoritative escrow hold cannot be read.
    /// A lease conclusion normally pins the elapsed, so the envelope-price
    /// fallback is normally a non-lease job's, for which it is exactly the
    /// released gross. The exception is a lease the boot reconcile concluded
    /// from a released hold without a stamp: this would read the window
    /// ceiling for it, so every money-moving caller prefers the escrow's
    /// `hold_info` and reaches here only as a last resort.
    pub fn released_gross_micro_usdc(&self) -> u64 {
        if self.envelope.payload.kind == covenant_compute_protocol::JobKind::LeaseSession {
            if let (Ok(Some(terms)), Some(elapsed)) = (
                covenant_compute_protocol::parse_lease_terms(&self.envelope.payload.input),
                self.metered_elapsed_ms,
            ) {
                return terms.metered_micro_usdc(elapsed);
            }
        }
        self.envelope.payload.price_micro_usdc
    }

    /// The elapsed milliseconds a lease view reports as of `now_ms`.
    ///
    /// A metered stamp is the settled figure the meter billed. Without one,
    /// only a still-running (`Accepted`) lease keeps metering against the
    /// wall clock; every other stampless lease reports no elapsed time. Most
    /// concluded to a whole refund (an operator rejected it, its execution
    /// failed, its deadline passed unserved), so zero is the whole truth.
    /// One does not: a lease the boot reconcile concluded from a released
    /// hold whose stamp died with the crash released a real charge but can
    /// no longer prove the seconds behind it — its money is recovered from
    /// the escrow by [`JobRecord::lease_charged_micro_usdc`], while the
    /// elapsed it cannot reconstruct stays zero. Metering any concluded
    /// lease against the wall clock would surface a charge that climbs
    /// forever after the buyer's money was already settled.
    pub fn lease_elapsed_ms(&self, now_ms: u64) -> u64 {
        if let Some(metered) = self.metered_elapsed_ms {
            return metered;
        }
        if self.phase == JobPhase::Accepted {
            return self
                .accepted_at_ms
                .map(|started| now_ms.saturating_sub(started))
                .unwrap_or(0);
        }
        0
    }

    /// The micro-USDC a lease view reports as charged as of `now_ms`, given
    /// the escrow hold's released amount `settled_gross` (read from
    /// `hold_info` at the call site).
    ///
    /// A live or normally-concluded lease bills its own metered figure. A
    /// lease the boot reconcile concluded from a released hold never pinned
    /// a meter stamp — the write died with the crash — so its own account
    /// would report zero for a lease that in fact released a positive
    /// charge, and the view would tell the buyer their whole window was
    /// refunded when it was not. For exactly that shape the escrow hold is
    /// the authority: `release_metered` wrote it down to what was billed,
    /// the same figure the payout sweep sizes from. The record heals once
    /// the operator re-submits its receipt and the stamp lands.
    pub fn lease_charged_micro_usdc(
        &self,
        terms: &covenant_compute_protocol::LeaseTerms,
        now_ms: u64,
        settled_gross: Option<u64>,
    ) -> u64 {
        if self.phase == JobPhase::Completed && self.metered_elapsed_ms.is_none() {
            return settled_gross.unwrap_or(0);
        }
        terms.metered_micro_usdc(self.lease_elapsed_ms(now_ms))
    }

    /// The access grant while the session is live. The grant is published
    /// once, when the machine comes up, and the record keeps it, but a
    /// concluded lease's machine is gone. Only a still-running (`Accepted`)
    /// lease can be reached, so a terminal one reports no endpoint rather
    /// than a dead address the buyer might try.
    pub fn live_lease_access(&self) -> Option<covenant_compute_protocol::LeaseAccess> {
        if self.phase == JobPhase::Accepted {
            return self.lease_access.clone();
        }
        None
    }

    /// This job's public [`covenant_compute_protocol::SettlementProof`],
    /// when it has one: a concluded job a receipt paid out on-chain.
    /// `None` for a job still running, one refunded or unpaid, one whose
    /// payout touched no chain (nothing to cite), and for the
    /// coordinator's own probe jobs (canary, redundancy mirror), which
    /// are not marketplace work. The gross/fee/net are the record's own,
    /// so the proof verifies offline against the split the release took.
    pub fn settlement_proof(
        &self,
        job_id: Uuid,
        mint_b58: &str,
    ) -> Option<covenant_compute_protocol::SettlementProof> {
        if self.phase != JobPhase::Completed || self.pinned {
            return None;
        }
        let receipt = self.receipt.clone()?;
        let payout = self.payout.as_ref()?;
        let tx_signature = payout.tx_signature.clone()?;
        let payout_memo = receipt.payout_memo();
        Some(covenant_compute_protocol::SettlementProof {
            job_id,
            gross_micro_usdc: self.released_gross_micro_usdc(),
            fee_micro_usdc: self.fee_micro_usdc,
            net_micro_usdc: payout.amount_micro_usdc,
            mint_b58: mint_b58.to_string(),
            payout_address_b58: self.payout_address.clone(),
            tx_signature,
            payout_memo,
            receipt,
        })
    }

    /// What this job owes the operator: its released gross minus the pinned
    /// fee once completed, zero on every other phase. For a metered lease the
    /// released gross is the seconds it ran, not the window ceiling the buyer
    /// escrowed, so an early-closed lease is never reported at its ceiling the
    /// way the envelope price would.
    ///
    /// `settled_gross` is the escrow hold's released amount (from `hold_info`
    /// at the call site), preferred over the record's own account. It matters
    /// for one shape the record reads back at the ceiling: a lease the boot
    /// reconcile concluded from a released hold whose meter stamp died with the
    /// crash. The hold is the authoritative, immutable record of what was
    /// released, so preferring it — with the record's own account as the same
    /// fallback the sweep uses when the hold cannot be read — makes the
    /// operator earnings feed report exactly what the payout sweep pays.
    pub fn owed_net_micro_usdc(&self, settled_gross: Option<u64>) -> u64 {
        if self.phase != JobPhase::Completed {
            return 0;
        }
        settled_gross
            .unwrap_or_else(|| self.released_gross_micro_usdc())
            .saturating_sub(self.fee_micro_usdc)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error("no such job {0}")]
    NotFound(Uuid),
    #[error("job {0} is already disputed")]
    AlreadyDisputed(Uuid),
    #[error("job journal: {0}")]
    Journal(#[from] crate::journal::JournalError),
}

/// What a completed release pinned onto the job record: the
/// marketplace's take and the partner rev-shares carved out of it —
/// the supply side's first, the buyer's partner from the remainder,
/// so the two can never sum past the fee. All zero on the failure
/// path.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReleaseCharges {
    pub fee_micro_usdc: u64,
    pub partner_share_micro_usdc: u64,
    pub buyer_partner_share_micro_usdc: u64,
}

/// What a conclusion pins onto the record beyond the receipt: the
/// operator identity the receipt was verified against (and their
/// payout address as resolved at that moment), plus — for a metered
/// lease — the elapsed run the settlement billed. See
/// [`JobBook::set_receipt_and_phase`].
#[derive(Debug, Clone)]
pub struct ReceiptAssignment {
    pub operator_pubkey_b58: String,
    pub payout_address: String,
    /// `Some` only for a lease settlement: the coordinator-observed
    /// elapsed milliseconds the released amount was metered from.
    pub metered_elapsed_ms: Option<u64>,
}

/// In-memory unless built with [`JobBook::restore`], in which case
/// every mutation journals the job's full record before it commits —
/// a mutation that can't be made durable fails instead of silently
/// diverging from the file (see [`crate::journal`]).
#[derive(Default)]
pub struct JobBook {
    jobs: Mutex<HashMap<Uuid, JobRecord>>,
    journal: Option<Arc<Journal>>,
}

impl JobBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seeds the book with journal-recovered records and journals every
    /// mutation from here on.
    pub fn restore(jobs: HashMap<Uuid, JobRecord>, journal: Arc<Journal>) -> Self {
        Self {
            jobs: Mutex::new(jobs),
            journal: Some(journal),
        }
    }

    pub fn insert(&self, job_id: Uuid, record: JobRecord) -> Result<(), JobError> {
        let mut guard = self.jobs.lock();
        if let Some(journal) = &self.journal {
            journal.record_job(job_id, &record)?;
        }
        guard.insert(job_id, record);
        Ok(())
    }

    pub fn get(&self, job_id: Uuid) -> Option<JobRecord> {
        self.jobs.lock().get(&job_id).cloned()
    }

    /// Every job this buyer has submitted, newest first by the
    /// envelope's own `issued_at_ms`. A scan, like the deadline sweep —
    /// job volume is v1-small.
    pub fn by_buyer(&self, buyer_pubkey_b58: &str) -> Vec<(Uuid, JobRecord)> {
        let mut jobs: Vec<(Uuid, JobRecord)> = self
            .jobs
            .lock()
            .iter()
            .filter(|(_, r)| r.envelope.payload.buyer.pubkey_base58() == buyer_pubkey_b58)
            .map(|(id, r)| (*id, r.clone()))
            .collect();
        jobs.sort_by(|a, b| {
            b.1.envelope
                .payload
                .issued_at_ms
                .cmp(&a.1.envelope.payload.issued_at_ms)
        });
        jobs
    }

    /// Every job assigned to this operator, newest first by the
    /// envelope's `issued_at_ms` — the supply-side mirror of
    /// [`JobBook::by_buyer`], and the same v1-small scan.
    pub fn by_operator(&self, operator_pubkey_b58: &str) -> Vec<(Uuid, JobRecord)> {
        let mut jobs: Vec<(Uuid, JobRecord)> = self
            .jobs
            .lock()
            .iter()
            .filter(|(_, r)| r.operator_pubkey_b58 == operator_pubkey_b58)
            .map(|(id, r)| (*id, r.clone()))
            .collect();
        jobs.sort_by(|a, b| {
            b.1.envelope
                .payload
                .issued_at_ms
                .cmp(&a.1.envelope.payload.issued_at_ms)
        });
        jobs
    }

    /// The public settlement feed: concluded jobs with an on-chain payout
    /// to cite, newest first by conclusion time, capped at `limit` and
    /// starting strictly before `before_ms` when paging. Each entry is a
    /// self-verifying [`covenant_compute_protocol::SettlementProof`]. A
    /// row that never recorded its conclusion time (a pre-upgrade journal
    /// row) is skipped so the feed's cursor is always a real timestamp —
    /// the same v1-small scan as [`JobBook::by_buyer`].
    pub fn settled_proofs(
        &self,
        mint_b58: &str,
        before_ms: Option<u64>,
        limit: usize,
    ) -> Vec<covenant_compute_protocol::SettlementProof> {
        let mut settled: Vec<(u64, covenant_compute_protocol::SettlementProof)> = self
            .jobs
            .lock()
            .iter()
            .filter_map(|(id, r)| {
                let at = r.concluded_at_ms?;
                if before_ms.is_some_and(|b| at >= b) {
                    return None;
                }
                Some((at, r.settlement_proof(*id, mint_b58)?))
            })
            .collect();
        settled.sort_by(|a, b| b.0.cmp(&a.0));
        settled.truncate(limit);
        settled.into_iter().map(|(_, proof)| proof).collect()
    }

    /// Every settled proof, in the batch's canonical leaf order: oldest
    /// conclusion first, ties broken by job id so the order is total and
    /// independent of map iteration. Unlike [`Self::settled_proofs`] this is
    /// the whole set, unpaged — a Merkle commitment only means something over a
    /// fixed, complete membership.
    fn settled_in_commit_order(
        &self,
        mint_b58: &str,
    ) -> Vec<(Uuid, covenant_compute_protocol::SettlementProof)> {
        let mut settled: Vec<(u64, Uuid, covenant_compute_protocol::SettlementProof)> = self
            .jobs
            .lock()
            .iter()
            .filter_map(|(id, r)| {
                Some((r.concluded_at_ms?, *id, r.settlement_proof(*id, mint_b58)?))
            })
            .collect();
        settled.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        settled
            .into_iter()
            .map(|(_, id, proof)| (id, proof))
            .collect()
    }

    /// The Merkle commitment over every settled job the feed can cite. A reader
    /// pins the returned root and proves any one settlement against it with
    /// [`Self::inclusion_proof`].
    pub fn settlement_batch(&self, mint_b58: &str) -> covenant_compute_protocol::SettlementBatch {
        let proofs: Vec<_> = self
            .settled_in_commit_order(mint_b58)
            .into_iter()
            .map(|(_, proof)| proof)
            .collect();
        covenant_compute_protocol::SettlementBatch::commit(&proofs)
    }

    /// One job's inclusion proof against the current batch root, or `None` when
    /// the job has no settlement to cite. Rebuilds the same canonical order
    /// [`Self::settlement_batch`] commits, so the audit path fits that root.
    pub fn inclusion_proof(
        &self,
        job_id: Uuid,
        mint_b58: &str,
    ) -> Option<covenant_compute_protocol::BatchInclusionProof> {
        let ordered = self.settled_in_commit_order(mint_b58);
        let index = ordered.iter().position(|(id, _)| *id == job_id)?;
        let proofs: Vec<_> = ordered.into_iter().map(|(_, proof)| proof).collect();
        covenant_compute_protocol::BatchInclusionProof::build(&proofs, index)
    }

    pub fn set_phase(&self, job_id: Uuid, phase: JobPhase) -> Result<(), JobError> {
        self.update(job_id, |record| record.phase = phase)
    }

    /// [`JobBook::set_phase`] for a money-back conclusion: the phase
    /// and the reason the refund landed with, written together under
    /// one journal row — no way to conclude a job unpaid through here
    /// without saying why.
    pub fn conclude_unpaid(
        &self,
        job_id: Uuid,
        phase: JobPhase,
        reason: RefundReason,
    ) -> Result<(), JobError> {
        self.update(job_id, |record| {
            record.phase = phase;
            record.refund_reason = Some(reason);
        })
    }

    pub fn set_payout(&self, job_id: Uuid, payout: PayoutOutcome) -> Result<(), JobError> {
        self.update(job_id, |record| record.payout = Some(payout))
    }

    /// Completed jobs whose release no recorded push ever honored —
    /// the payout-retry sweep's worklist. Money the books say is owed.
    pub fn completed_unpaid(&self) -> Vec<(Uuid, JobRecord)> {
        self.jobs
            .lock()
            .iter()
            .filter(|(_, r)| matches!(r.phase, JobPhase::Completed) && r.payout.is_none())
            .map(|(id, r)| (*id, r.clone()))
            .collect()
    }

    /// Completed jobs of one kind — the redundancy sampler's source
    /// pool. The same v1-small scan as every other book read.
    pub fn completed_of_kind(
        &self,
        kind: covenant_compute_protocol::JobKind,
    ) -> Vec<(Uuid, JobRecord)> {
        self.jobs
            .lock()
            .iter()
            .filter(|(_, r)| {
                matches!(r.phase, JobPhase::Completed) && r.envelope.payload.kind == kind
            })
            .map(|(id, r)| (*id, r.clone()))
            .collect()
    }

    /// Concludes a job on its verified receipt. The conclusion pins the
    /// record's operator and payout address back to the identity the
    /// receipt was verified against (`assignment`): between the
    /// handler's read and this write, a stale-offer reassign can move
    /// the durable record to another operator, and concluding without
    /// re-pinning would leave that operator's name on a job someone
    /// else completed — which `sweep_unpaid` would then pay. The
    /// receipt's signer is the one identity that can never be stale.
    #[allow(clippy::too_many_arguments)]
    pub fn set_receipt_and_phase(
        &self,
        job_id: Uuid,
        receipt: SignedWorkReceipt,
        output: Vec<Content>,
        charges: ReleaseCharges,
        phase: JobPhase,
        refund_reason: Option<RefundReason>,
        assignment: ReceiptAssignment,
    ) -> Result<(), JobError> {
        self.update(job_id, |record| {
            record.operator_pubkey_b58 = assignment.operator_pubkey_b58;
            record.payout_address = assignment.payout_address;
            record.metered_elapsed_ms = assignment.metered_elapsed_ms;
            record.receipt = Some(receipt);
            record.output = Some(output);
            record.fee_micro_usdc = charges.fee_micro_usdc;
            record.partner_share_micro_usdc = charges.partner_share_micro_usdc;
            record.buyer_partner_share_micro_usdc = charges.buyer_partner_share_micro_usdc;
            record.phase = phase;
            record.refund_reason = refund_reason;
            record.concluded_at_ms = Some(crate::epoch_ms());
        })
    }

    /// Holds an agent task's verified result for a check: the receipt and
    /// output are kept, the phase becomes `AwaitingCheck`, and the escrow
    /// is not touched. Guarded on the job still being live and assigned to
    /// the operator the receipt verified against, so a racing reassign or
    /// sweep cannot have a stranger's result parked over it. `Ok(false)`
    /// when the guard refuses.
    pub fn park_for_check(
        &self,
        job_id: Uuid,
        receipt: SignedWorkReceipt,
        output: Vec<Content>,
        operator_pubkey_b58: &str,
    ) -> Result<bool, JobError> {
        self.update_if(
            job_id,
            |r| {
                matches!(r.phase, JobPhase::Offered | JobPhase::Accepted)
                    && r.operator_pubkey_b58 == operator_pubkey_b58
            },
            |r| {
                r.receipt = Some(receipt);
                r.output = Some(output);
                r.phase = JobPhase::AwaitingCheck;
            },
        )
    }

    /// Links a check job to the task it checks.
    pub fn add_check_job(&self, task_id: Uuid, check_id: Uuid) -> Result<(), JobError> {
        self.update(task_id, |r| r.check_jobs.push(check_id))
    }

    /// Stores a task's hidden checks. `Ok(false)` when the task already holds
    /// a different set: a commitment names one set, and the first that
    /// matched it stands.
    pub fn set_hidden_checks(
        &self,
        job_id: Uuid,
        hidden: covenant_compute_protocol::HiddenChecks,
    ) -> Result<bool, JobError> {
        let same = hidden.clone();
        self.update_if(
            job_id,
            |r| r.hidden_checks.as_ref().is_none_or(|held| *held == same),
            |r| r.hidden_checks = Some(hidden),
        )
    }

    /// Pins what the chain counted for a task's round.
    pub fn set_vote_round(
        &self,
        task_id: Uuid,
        round: crate::rounds::RoundRecord,
    ) -> Result<bool, JobError> {
        self.update_if(task_id, |_| true, |r| r.vote_round = Some(round))
    }

    /// Agent tasks whose results wait on a check: the settle tick's
    /// worklist.
    pub fn awaiting_check(&self) -> Vec<(Uuid, JobRecord)> {
        self.jobs
            .lock()
            .iter()
            .filter(|(_, r)| r.phase == JobPhase::AwaitingCheck)
            .map(|(id, r)| (*id, r.clone()))
            .collect()
    }

    /// Concludes a parked task unpaid, if it is still parked: `Ok(false)`
    /// means another path already concluded it.
    pub fn conclude_parked_unpaid(
        &self,
        job_id: Uuid,
        reason: RefundReason,
    ) -> Result<bool, JobError> {
        self.update_if(
            job_id,
            |r| r.phase == JobPhase::AwaitingCheck,
            |r| {
                r.phase = JobPhase::Refunded;
                r.refund_reason = Some(reason);
                r.concluded_at_ms = Some(crate::epoch_ms());
            },
        )
    }

    /// Completes a parked task whose release the books already show, pinning
    /// the fee the release took. Boot recovery's path for a crash between
    /// the release and the record write.
    pub fn conclude_parked_released(
        &self,
        job_id: Uuid,
        fee_micro_usdc: u64,
    ) -> Result<(), JobError> {
        self.update(job_id, |r| {
            r.phase = JobPhase::Completed;
            r.fee_micro_usdc = fee_micro_usdc;
            r.concluded_at_ms = Some(crate::epoch_ms());
        })
    }

    /// Records the buyer's close request against a running lease and
    /// returns whether this call set it. `Ok(false)` means the session
    /// was not `Accepted` (never started, or already concluded) or a
    /// close was already recorded — an honest retry, not an error.
    /// One lock hold, so two racing closes agree on one instant and the
    /// meter can never be re-stamped later than the first instruction.
    pub fn request_lease_close(&self, job_id: Uuid, now_ms: u64) -> Result<bool, JobError> {
        self.update_if(
            job_id,
            |r| matches!(r.phase, JobPhase::Accepted) && r.close_requested_at_ms.is_none(),
            |r| r.close_requested_at_ms = Some(now_ms),
        )
    }

    /// Publishes where a running session can be reached. Idempotent per
    /// job: the first grant wins, so a redelivered chunk cannot move a
    /// buyer's session address underneath them.
    pub fn set_lease_access(
        &self,
        job_id: Uuid,
        access: covenant_compute_protocol::LeaseAccess,
    ) -> Result<bool, JobError> {
        self.update_if(
            job_id,
            |r| r.lease_access.is_none(),
            move |r| r.lease_access = Some(access),
        )
    }

    /// Pins the buyer's signed dispute onto the job — once. Callers
    /// validate everything contextual (buyer match, phase, window);
    /// the one-dispute-per-job invariant lives here, under the same
    /// lock as the write, so two racing disputes can't both record.
    pub fn set_dispute(&self, job_id: Uuid, dispute: DisputeRequest) -> Result<(), JobError> {
        let mut guard = self.jobs.lock();
        let record = guard.get_mut(&job_id).ok_or(JobError::NotFound(job_id))?;
        if record.dispute.is_some() {
            return Err(JobError::AlreadyDisputed(job_id));
        }
        let mut updated = record.clone();
        updated.dispute = Some(dispute);
        if let Some(journal) = &self.journal {
            journal.record_job(job_id, &updated)?;
        }
        *record = updated;
        Ok(())
    }

    /// Jobs whose current offer has sat unaccepted for at least
    /// `reoffer_after_ms` as of `now_ms` and is still worth saving —
    /// the stale-offer sweep's worklist. Still `Offered`, actually
    /// matched (an empty assignee is the terminal no-operator record),
    /// and inside its deadline: an expired job belongs to the refund
    /// sweep, and re-offering it would hand the next operator a job the
    /// same tick kills. Pinned probes ride this list too, but the sweep
    /// only ever redelivers them to their own operator (a measurement
    /// can't be re-pointed at another): a queue the coordinator lost on
    /// restart still needs healing, or the deadline sweep faults the
    /// probed operator for a job it never received.
    pub fn stale_offered(&self, now_ms: u64, reoffer_after_ms: u64) -> Vec<(Uuid, JobRecord)> {
        self.jobs
            .lock()
            .iter()
            .filter(|(_, r)| {
                matches!(r.phase, JobPhase::Offered)
                    && !r.operator_pubkey_b58.is_empty()
                    && now_ms.saturating_sub(r.offered_at_ms) >= reoffer_after_ms
                    && r.envelope
                        .payload
                        .issued_at_ms
                        .saturating_add(r.envelope.payload.deadline_ms)
                        >= now_ms
            })
            .map(|(id, r)| (*id, r.clone()))
            .collect()
    }

    /// Every job currently offered to `operator` and still worth
    /// moving — the offline heal's worklist. Same guards as
    /// [`JobBook::stale_offered`] minus the age check: the operator's
    /// own `Offline` declaration replaces the waiting.
    pub fn offered_to(&self, operator: &str, now_ms: u64) -> Vec<(Uuid, JobRecord)> {
        self.jobs
            .lock()
            .iter()
            .filter(|(_, r)| {
                matches!(r.phase, JobPhase::Offered)
                    && !r.pinned
                    && r.operator_pubkey_b58 == operator
                    && r.envelope
                        .payload
                        .issued_at_ms
                        .saturating_add(r.envelope.payload.deadline_ms)
                        >= now_ms
            })
            .map(|(id, r)| (*id, r.clone()))
            .collect()
    }

    /// Re-points a still-`Offered` job at a new operator, capturing the
    /// winner's payout address and referral attribution the way the
    /// original match did, and restarts the stale clock. Guarded and
    /// atomic: `Ok(false)` without writing when the job moved on — the
    /// phase left `Offered` (the assignee's accept or reject won the
    /// race) or the assignee is no longer `expected_operator` (another
    /// reassignment did). The journal write happens under the same lock
    /// as the check, so a racing accept can't interleave.
    pub fn reassign(
        &self,
        job_id: Uuid,
        expected_operator: &str,
        new_operator: &str,
        new_payout_address: String,
        new_referral_code: Option<String>,
        now_ms: u64,
    ) -> Result<bool, JobError> {
        self.update_if(
            job_id,
            |r| matches!(r.phase, JobPhase::Offered) && r.operator_pubkey_b58 == expected_operator,
            |r| {
                r.operator_pubkey_b58 = new_operator.to_string();
                r.payout_address = new_payout_address;
                r.referral_code = new_referral_code;
                r.offered_at_ms = now_ms;
            },
        )
    }

    /// Restarts the stale clock on a still-`Offered` job without moving
    /// it — the re-offer sweep's same-winner outcome, where the current
    /// assignee is still the best (or only) fit and only the lost
    /// delivery needs healing. Captured payout/referral terms stay as
    /// won. Same guard and atomicity as [`JobBook::reassign`].
    pub fn touch_offered(
        &self,
        job_id: Uuid,
        expected_operator: &str,
        now_ms: u64,
    ) -> Result<bool, JobError> {
        self.update_if(
            job_id,
            |r| matches!(r.phase, JobPhase::Offered) && r.operator_pubkey_b58 == expected_operator,
            |r| r.offered_at_ms = now_ms,
        )
    }

    /// Concludes a still-`Offered` job as refunded — the atomic flip
    /// behind the buyer's cancel. `Ok(false)` without writing when the
    /// job moved on first (an accept, the deadline sweep, an earlier
    /// cancel): the caller answers from whatever phase won. The check
    /// and the journal write share one lock hold, so a racing accept
    /// can't interleave between them.
    pub fn cancel_if_offered(&self, job_id: Uuid) -> Result<bool, JobError> {
        self.update_if(
            job_id,
            |r| matches!(r.phase, JobPhase::Offered),
            |r| {
                r.phase = JobPhase::Refunded;
                r.refund_reason = Some(RefundReason::BuyerCancelled);
            },
        )
    }

    /// [`JobBook::set_phase`] guarded on the assignee AND on the job
    /// still being live: writes only while `operator` holds the job and
    /// the phase is `Offered`/`Accepted`, `Ok(false)` otherwise. What
    /// the accept/reject handlers use so a re-offered-away operator's
    /// late decision can't overwrite the new assignment — and so a
    /// decision landing after the job concluded (deadline-swept,
    /// settled) can't resurrect a terminal phase into a live one that
    /// the in-flight ceiling and the books would count forever. An
    /// accept retry whose first ack was lost still lands: `Accepted`
    /// is a live phase.
    pub fn set_phase_if_assigned(
        &self,
        job_id: Uuid,
        operator: &str,
        phase: JobPhase,
        refund_reason: Option<RefundReason>,
    ) -> Result<bool, JobError> {
        self.update_if(
            job_id,
            |r| {
                r.operator_pubkey_b58 == operator
                    && matches!(r.phase, JobPhase::Offered | JobPhase::Accepted)
            },
            |r| {
                if phase == JobPhase::Accepted && r.accepted_at_ms.is_none() {
                    // First accept only: a lost-ack accept retry must
                    // not restart a lease's meter.
                    r.accepted_at_ms = Some(crate::epoch_ms());
                }
                r.phase = phase;
                r.refund_reason = refund_reason;
            },
        )
    }

    /// Aggregate phase counts across the whole book, plus how many jobs
    /// carry a dispute — the `/metrics` scrape shape. One pass under the
    /// lock, same v1-small scan as the sweeps.
    pub fn stats(&self) -> JobStats {
        let guard = self.jobs.lock();
        let mut stats = JobStats::default();
        for record in guard.values() {
            match record.phase {
                JobPhase::Offered => stats.offered += 1,
                JobPhase::Accepted => stats.accepted += 1,
                JobPhase::Rejected => stats.rejected += 1,
                JobPhase::Completed => stats.completed += 1,
                JobPhase::Failed => stats.failed += 1,
                JobPhase::Refunded => stats.refunded += 1,
                JobPhase::AwaitingCheck => stats.awaiting_check += 1,
            }
            if record.dispute.is_some() {
                stats.disputed += 1;
            }
        }
        stats
    }

    /// The fee books, derived on read: total marketplace take across
    /// completed jobs and how many jobs it was taken from. Durable for
    /// free — the per-job fee rides the journaled `JobRecord`.
    pub fn fees_captured(&self) -> (u64, usize) {
        self.jobs
            .lock()
            .values()
            .filter(|r| r.fee_micro_usdc > 0)
            .fold((0u64, 0usize), |(total, count), r| {
                (total.saturating_add(r.fee_micro_usdc), count + 1)
            })
    }

    /// The payout books, derived on read: what the coordinator has
    /// pushed to operators, and what it still owes — completed jobs
    /// whose push failed, each owing gross minus the pinned fee. Their
    /// sum plus captured fees is exactly the released escrow gross,
    /// which is the identity `/metrics` exposes for a scraper to watch.
    pub fn payout_totals(&self) -> (u64, u64) {
        self.jobs
            .lock()
            .values()
            .filter(|r| r.phase == JobPhase::Completed)
            .fold((0u64, 0u64), |(pushed, outstanding), r| match &r.payout {
                Some(p) => (pushed.saturating_add(p.amount_micro_usdc), outstanding),
                None => (
                    pushed,
                    outstanding.saturating_add(
                        r.released_gross_micro_usdc()
                            .saturating_sub(r.fee_micro_usdc),
                    ),
                ),
            })
    }

    /// The rev-share books, derived on read: per referral code, the
    /// total accrued out of captured fees (both sides — a partner can
    /// refer operators and buyers alike) and the number of jobs it came
    /// from, each job counted once even when the same code earned on
    /// both sides of it. Durable for free — the per-job shares ride the
    /// journaled `JobRecord`.
    pub fn partner_accruals(&self) -> HashMap<String, (u64, usize)> {
        let mut accruals: HashMap<String, (u64, usize)> = HashMap::new();
        for record in self.jobs.lock().values() {
            let mut per_code: Vec<(&String, u64)> = Vec::with_capacity(2);
            if let (Some(code), share @ 1..) =
                (&record.referral_code, record.partner_share_micro_usdc)
            {
                per_code.push((code, share));
            }
            if let (Some(code), share @ 1..) = (
                &record.buyer_referral_code,
                record.buyer_partner_share_micro_usdc,
            ) {
                match per_code.iter_mut().find(|(c, _)| *c == code) {
                    Some(entry) => entry.1 = entry.1.saturating_add(share),
                    None => per_code.push((code, share)),
                }
            }
            for (code, share) in per_code {
                let entry = accruals.entry(code.clone()).or_default();
                entry.0 = entry.0.saturating_add(share);
                entry.1 += 1;
            }
        }
        accruals
    }

    /// Journal-then-commit under the lock: the updated record hits the
    /// file before memory, so a journal failure leaves memory unchanged
    /// (the disk being one upsert ahead replays to the same state).
    fn update(&self, job_id: Uuid, mutate: impl FnOnce(&mut JobRecord)) -> Result<(), JobError> {
        self.update_if(job_id, |_| true, mutate).map(|_| ())
    }

    /// [`JobBook::update`] with a precondition checked under the same
    /// lock as the write: `Ok(false)` and no journal touch when `check`
    /// refuses, so guard-and-mutate is one atomic step for callers
    /// racing each other over the same record.
    fn update_if(
        &self,
        job_id: Uuid,
        check: impl FnOnce(&JobRecord) -> bool,
        mutate: impl FnOnce(&mut JobRecord),
    ) -> Result<bool, JobError> {
        let mut guard = self.jobs.lock();
        let record = guard.get_mut(&job_id).ok_or(JobError::NotFound(job_id))?;
        if !check(record) {
            return Ok(false);
        }
        let mut updated = record.clone();
        mutate(&mut updated);
        if let Some(journal) = &self.journal {
            journal.record_job(job_id, &updated)?;
        }
        *record = updated;
        Ok(true)
    }

    /// Lease sessions the coordinator currently observes as running —
    /// the on-chain meter's tick worklist. Accepted, so a `t0` exists,
    /// and metered from that stamp until a receipt or a sweep concludes
    /// them. The same v1-small scan under the lock as every other book
    /// read.
    pub fn live_leases(&self) -> Vec<(Uuid, JobRecord)> {
        self.jobs
            .lock()
            .iter()
            .filter(|(_, r)| crate::onchain_meter::is_live_lease(r))
            .map(|(id, r)| (*id, r.clone()))
            .collect()
    }

    /// Job ids still `Offered`/`Accepted` whose buyer-stated
    /// `issued_at_ms + deadline_ms` has already passed `now_ms` — the
    /// coordinator's deadline-refund candidates
    /// (build-notes-phase1-foundation.md §1.6's mechanical refund path).
    pub fn expired(&self, now_ms: u64) -> Vec<Uuid> {
        self.jobs
            .lock()
            .iter()
            .filter(|(_, r)| matches!(r.phase, JobPhase::Offered | JobPhase::Accepted))
            .filter(|(_, r)| {
                r.envelope
                    .payload
                    .issued_at_ms
                    .saturating_add(r.envelope.payload.deadline_ms)
                    < now_ms
            })
            .map(|(id, _)| *id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency, A2ATaskStatus};
    use covenant_compute_protocol::{
        CapabilityRequirement, FundingSource, JobEnvelopePayload, JobKind, JobMeter, LeaseTerms,
        SignedWorkReceipt, WorkReceiptPayload,
    };
    use covenant_identity::LocalIdentity;
    use covenant_mcp::Content;

    const FEED_MINT: &str = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";

    fn record(job_id: Uuid, issued_at_ms: u64, deadline_ms: u64) -> JobRecord {
        let buyer = LocalIdentity::generate("buyer@local");
        let coordinator = LocalIdentity::generate("coordinator@local");
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
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "jobs-test"),
            issued_at_ms,
            referral_code: None,
            stream: false,
        };
        let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
        let escrow_hold = EscrowHoldAttestation::sign(
            job_id,
            100,
            FundingSource::Organic,
            issued_at_ms,
            &coordinator,
        )
        .unwrap();
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
            check_jobs: Vec::new(),
            checks_task: None,
            hidden_checks: None,
            vote_round: None,
        }
    }

    fn signed_receipt(job_id: Uuid, operator: &LocalIdentity, price: u64) -> SignedWorkReceipt {
        let payload = WorkReceiptPayload {
            job_id,
            operator: operator.agent_id(),
            job_hash_hex: "aa".repeat(32),
            result_hash_hex: "bb".repeat(32),
            meter: JobMeter {
                wall_ms: 10,
                tokens_in: None,
                tokens_out: None,
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc: price,
            status: A2ATaskStatus::Ok,
            executed_at_ms: 1,
            node_audit_root_hex: "cc".repeat(32),
        };
        SignedWorkReceipt::sign(payload, operator).unwrap()
    }

    /// A concluded job a receipt paid out on-chain — one feed entry. The
    /// `record` price is the released gross, split into the operator's net
    /// and the pinned fee, so the assembled proof verifies offline.
    fn paid(job_id: Uuid, concluded_at_ms: u64, fee: u64) -> JobRecord {
        let operator = LocalIdentity::generate("operator@local");
        let mut r = record(job_id, 0, 5_000);
        let price = r.envelope.payload.price_micro_usdc;
        r.phase = JobPhase::Completed;
        r.fee_micro_usdc = fee;
        r.receipt = Some(signed_receipt(job_id, &operator, price));
        r.payout = Some(PayoutOutcome {
            amount_micro_usdc: price - fee,
            tx_signature: Some(format!("txsig-{job_id}")),
            recorded_at_ms: concluded_at_ms,
        });
        r.concluded_at_ms = Some(concluded_at_ms);
        r
    }

    #[test]
    fn settled_proofs_list_paid_jobs_newest_first_and_each_verifies() {
        let book = JobBook::new();
        let older = Uuid::new_v4();
        book.insert(older, paid(older, 1_000, 20)).unwrap();
        let newer = Uuid::new_v4();
        book.insert(newer, paid(newer, 2_000, 5)).unwrap();

        let feed = book.settled_proofs(FEED_MINT, None, 50);
        assert_eq!(feed.len(), 2);
        assert_eq!(feed[0].job_id, newer, "newest conclusion first");
        assert_eq!(feed[1].job_id, older);
        for proof in &feed {
            proof
                .verify_offline()
                .expect("a served proof verifies offline");
            assert_eq!(proof.mint_b58, FEED_MINT);
        }
    }

    #[test]
    fn the_feed_excludes_everything_without_a_citable_on_chain_payout() {
        let book = JobBook::new();
        let offered = Uuid::new_v4();
        book.insert(offered, record(offered, 0, 5_000)).unwrap();

        let refunded = Uuid::new_v4();
        let mut r = paid(refunded, 100, 0);
        r.phase = JobPhase::Refunded;
        book.insert(refunded, r).unwrap();

        let unpaid = Uuid::new_v4();
        let mut r = paid(unpaid, 100, 0);
        r.payout = None;
        book.insert(unpaid, r).unwrap();

        let offchain = Uuid::new_v4();
        let mut r = paid(offchain, 100, 0);
        r.payout.as_mut().unwrap().tx_signature = None;
        book.insert(offchain, r).unwrap();

        let probe = Uuid::new_v4();
        let mut r = paid(probe, 100, 0);
        r.pinned = true;
        book.insert(probe, r).unwrap();

        assert!(book.settled_proofs(FEED_MINT, None, 50).is_empty());
    }

    #[test]
    fn the_feed_pages_before_a_cursor_and_caps_at_the_limit() {
        let book = JobBook::new();
        for at in [10_u64, 20, 30, 40] {
            let id = Uuid::new_v4();
            book.insert(id, paid(id, at, 0)).unwrap();
        }
        assert_eq!(book.settled_proofs(FEED_MINT, None, 2).len(), 2, "capped");
        let page = book.settled_proofs(FEED_MINT, Some(30), 50);
        assert_eq!(page.len(), 2, "only conclusions strictly before 30");
        assert!(page.iter().all(|p| p.verify_offline().is_ok()));
    }

    #[test]
    fn the_batch_commits_every_settled_job_independent_of_insertion_order() {
        // a and b conclude in the same millisecond, so the job-id tie-break is
        // what makes the leaf order total. Build the records once and clone
        // them into two books inserted in opposite orders: the commit is the
        // same, so it does not depend on map iteration.
        let a = Uuid::from_u128(3);
        let b = Uuid::from_u128(1);
        let c = Uuid::from_u128(2);
        let records = [
            (a, paid(a, 20, 0)),
            (b, paid(b, 20, 0)),
            (c, paid(c, 30, 5)),
        ];

        let book1 = JobBook::new();
        for (id, r) in &records {
            book1.insert(*id, r.clone()).unwrap();
        }
        let book2 = JobBook::new();
        for (id, r) in records.iter().rev() {
            book2.insert(*id, r.clone()).unwrap();
        }
        let batch = book1.settlement_batch(FEED_MINT);
        assert_eq!(batch.root_hex, book2.settlement_batch(FEED_MINT).root_hex);
        assert_eq!(batch.tree_size, 3);
        // (20, id=1)=b, (20, id=3)=a, then (30, id=2)=c.
        assert_eq!(batch.job_ids, vec![b, a, c]);

        for id in [a, b, c] {
            let inclusion = book1.inclusion_proof(id, FEED_MINT).expect("a settled job");
            inclusion
                .verify_offline(&batch.root_hex)
                .expect("inclusion under the batch root");
            assert_eq!(inclusion.proof.job_id, id);
        }
        assert!(book1.inclusion_proof(Uuid::new_v4(), FEED_MINT).is_none());
    }

    #[test]
    fn an_empty_book_commits_to_the_empty_root() {
        let book = JobBook::new();
        let batch = book.settlement_batch(FEED_MINT);
        assert_eq!(batch.tree_size, 0);
        assert!(batch.job_ids.is_empty());
        assert_eq!(batch.root_hex.len(), 64);
        assert!(book.inclusion_proof(Uuid::new_v4(), FEED_MINT).is_none());
    }

    #[test]
    fn payout_totals_split_pushed_from_outstanding() {
        let book = JobBook::new();
        // A completed job whose push landed counts as pushed.
        let paid = Uuid::new_v4();
        let mut r = record(paid, 0, 5_000);
        r.phase = JobPhase::Completed;
        r.fee_micro_usdc = 20;
        r.payout = Some(PayoutOutcome {
            amount_micro_usdc: 80,
            tx_signature: None,
            recorded_at_ms: 1,
        });
        book.insert(paid, r).unwrap();
        // A completed job whose push never landed owes gross minus the
        // pinned fee.
        let owed = Uuid::new_v4();
        let mut r = record(owed, 0, 5_000);
        r.phase = JobPhase::Completed;
        r.fee_micro_usdc = 5;
        book.insert(owed, r).unwrap();
        // Nothing else counts: a refund owes nothing, an in-flight job
        // owes nothing yet.
        let refunded = Uuid::new_v4();
        let mut r = record(refunded, 0, 5_000);
        r.phase = JobPhase::Refunded;
        book.insert(refunded, r).unwrap();
        let inflight = Uuid::new_v4();
        book.insert(inflight, record(inflight, 0, 5_000)).unwrap();

        assert_eq!(book.payout_totals(), (80, 95));
    }

    /// A concluded lease record: `LeaseSession` kind, the signed terms in
    /// its input, the window ceiling as its escrowed price, and the
    /// coordinator-observed elapsed the settlement metered. Completed with
    /// no payout — a job the retry sweep would size.
    fn lease_record(job_id: Uuid, terms: LeaseTerms, metered_elapsed_ms: Option<u64>) -> JobRecord {
        let buyer = LocalIdentity::generate("buyer@local");
        let coordinator = LocalIdentity::generate("coordinator@local");
        let ceiling = terms.max_price_micro_usdc().unwrap();
        let max_duration_secs = terms.max_duration_secs;
        let payload = JobEnvelopePayload {
            job_id,
            buyer: buyer.agent_id(),
            kind: JobKind::LeaseSession,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: None,
                kind: JobKind::LeaseSession,
                max_duration_secs: u32::try_from(max_duration_secs).unwrap(),
                min_reputation_bps: None,
            },
            input: vec![covenant_compute_protocol::lease_input(terms).unwrap()],
            price_micro_usdc: ceiling,
            deadline_ms: max_duration_secs
                .saturating_mul(1_000)
                .saturating_add(60_000),
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "lease-test"),
            issued_at_ms: 0,
            referral_code: None,
            stream: true,
        };
        let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
        let escrow_hold =
            EscrowHoldAttestation::sign(job_id, ceiling, FundingSource::Organic, 0, &coordinator)
                .unwrap();
        JobRecord {
            operator_pubkey_b58: "operator-pubkey".into(),
            payout_address: "operator-payout".into(),
            envelope,
            escrow_hold,
            phase: JobPhase::Completed,
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
            accepted_at_ms: Some(1),
            metered_elapsed_ms,
            close_requested_at_ms: None,
            lease_access: None,
            check_jobs: Vec::new(),
            checks_task: None,
            hidden_checks: None,
            vote_round: None,
        }
    }

    #[test]
    fn released_gross_is_the_metered_draw_for_a_lease_and_the_price_otherwise() {
        // 100 micro/s over a 600s window: the buyer escrows a 60_000
        // ceiling, but a session that ran 5s is owed only 500.
        let terms = LeaseTerms {
            max_duration_secs: 600,
            rate_micro_usdc_per_sec: 100,
            client_public_key: None,
        };
        let ceiling = terms.max_price_micro_usdc().unwrap();
        assert_eq!(ceiling, 60_000);

        let lease = lease_record(Uuid::new_v4(), terms, Some(5_000));
        assert_eq!(lease.released_gross_micro_usdc(), 500);
        assert!(
            lease.released_gross_micro_usdc() < ceiling,
            "a metered lease is owed its used seconds, never the window ceiling"
        );

        // Every other kind releases its whole envelope price.
        let batch = record(Uuid::new_v4(), 0, 5_000);
        assert_eq!(
            batch.released_gross_micro_usdc(),
            batch.envelope.payload.price_micro_usdc
        );
    }

    #[test]
    fn owed_net_is_the_metered_draw_minus_fee_for_a_completed_lease() {
        // The operator earnings feed reports "owed net". A 100 micro/s lease
        // over a 600s window that ran 5s is owed 500 gross, so after a 50 fee
        // its net is 450 — never the 60_000 ceiling the buyer was refunded
        // out of.
        let terms = LeaseTerms {
            max_duration_secs: 600,
            rate_micro_usdc_per_sec: 100,
            client_public_key: None,
        };
        let mut lease = lease_record(Uuid::new_v4(), terms, Some(5_000));
        lease.fee_micro_usdc = 50;
        assert_eq!(lease.owed_net_micro_usdc(None), 450);
        assert!(
            lease.owed_net_micro_usdc(None)
                < lease.envelope.payload.price_micro_usdc - lease.fee_micro_usdc,
            "owed net must not report the escrowed window ceiling"
        );

        // Only a completed job owes; a still-running lease owes nothing yet.
        lease.phase = JobPhase::Accepted;
        assert_eq!(lease.owed_net_micro_usdc(None), 0);
    }

    #[test]
    fn owed_net_prefers_the_escrow_hold_for_a_stampless_completed_lease() {
        // A crash between release_metered and the meter stamp concludes the
        // lease Completed with no metered_elapsed_ms, so its own released
        // gross reads back at the window ceiling. The operator earnings feed
        // must report what the payout sweep will pay — the escrow hold's
        // written-down draw — not that ceiling.
        let terms = LeaseTerms {
            max_duration_secs: 600,
            rate_micro_usdc_per_sec: 100,
            client_public_key: None,
        };
        let ceiling = terms.max_price_micro_usdc().unwrap();
        let mut lease = lease_record(Uuid::new_v4(), terms, None);
        lease.fee_micro_usdc = 50;

        // With the authoritative hold (the 5s draw release_metered wrote down
        // = 500), the feed owes 450 — the same net the sweep sizes from the
        // same hold, not the 60_000 ceiling.
        assert_eq!(lease.owed_net_micro_usdc(Some(500)), 450);
        assert!(lease.owed_net_micro_usdc(Some(500)) < ceiling - lease.fee_micro_usdc);

        // Without the hold, it falls back to the record's own account, the
        // sweep's own last resort.
        assert_eq!(lease.owed_net_micro_usdc(None), ceiling - 50);
    }

    #[test]
    fn a_concluded_lease_without_a_stamp_reports_no_elapsed_not_the_wall_clock() {
        let terms = LeaseTerms {
            max_duration_secs: 600,
            rate_micro_usdc_per_sec: 100,
            client_public_key: None,
        };
        // Accepted at t=1, read ~1000s later.
        let now = 1_000_000_u64;

        // A live lease still meters against the wall clock.
        let mut live = lease_record(Uuid::new_v4(), terms.clone(), None);
        live.phase = JobPhase::Accepted;
        assert_eq!(live.lease_elapsed_ms(now), now - 1);

        // A lease whose execution failed refunds whole and never stamped a
        // meter: it must read 0, not the ever-growing wall clock, or its
        // view bills the buyer for money it already returned.
        let mut failed = lease_record(Uuid::new_v4(), terms.clone(), None);
        failed.phase = JobPhase::Failed;
        assert_eq!(failed.lease_elapsed_ms(now), 0);

        // A deadline that passed while the session was accepted refunds the
        // same way.
        let mut expired = lease_record(Uuid::new_v4(), terms.clone(), None);
        expired.phase = JobPhase::Refunded;
        assert_eq!(expired.lease_elapsed_ms(now), 0);

        // A rejected lease was never accepted; still 0.
        let mut rejected = lease_record(Uuid::new_v4(), terms.clone(), None);
        rejected.phase = JobPhase::Rejected;
        rejected.accepted_at_ms = None;
        assert_eq!(rejected.lease_elapsed_ms(now), 0);

        // A stamped lease reports exactly what it metered, whatever the phase.
        let stamped = lease_record(Uuid::new_v4(), terms, Some(5_000));
        assert_eq!(stamped.lease_elapsed_ms(now), 5_000);
    }

    #[test]
    fn a_stampless_completed_lease_bills_the_escrow_not_a_lost_meter() {
        let terms = LeaseTerms {
            max_duration_secs: 600,
            rate_micro_usdc_per_sec: 100,
            client_public_key: None,
        };

        // The boot reconcile shape: Completed (the builder's default) with no
        // meter stamp, because the crash fell between the escrow release and
        // the record write. The record alone reports zero elapsed, so the old
        // view billed the buyer zero and refunded the whole window — for a
        // lease whose escrow released a real 500. The escrow hold is the
        // authority now, so the charge follows it.
        let recovered = lease_record(Uuid::new_v4(), terms.clone(), None);
        assert_eq!(recovered.lease_elapsed_ms(1_000_000), 0);
        assert_eq!(
            recovered.lease_charged_micro_usdc(&terms, 1_000_000, Some(500)),
            500,
            "a released hold's amount is the charge when the stamp is gone"
        );
        // Escrow unreadable is the only fallback to zero — no worse than the
        // record alone, and it never invents a charge.
        assert_eq!(
            recovered.lease_charged_micro_usdc(&terms, 1_000_000, None),
            0
        );

        // A normally-concluded lease keeps its pinned meter and ignores the
        // hold: 5_000 ms at 100 micro-USDC/s is 500, whatever the escrow says.
        let stamped = lease_record(Uuid::new_v4(), terms.clone(), Some(5_000));
        assert_eq!(
            stamped.lease_charged_micro_usdc(&terms, 1_000_000, Some(999_999)),
            500
        );

        // A live lease meters against the wall clock and ignores the hold's
        // still-held ceiling: accepted at t=1, read at t=5_001 → 5_000 ms → 500.
        let mut live = lease_record(Uuid::new_v4(), terms.clone(), None);
        live.phase = JobPhase::Accepted;
        assert_eq!(
            live.lease_charged_micro_usdc(&terms, 5_001, Some(60_000)),
            500
        );
    }

    #[test]
    fn a_concluded_lease_stops_serving_its_access_grant() {
        let terms = LeaseTerms {
            max_duration_secs: 600,
            rate_micro_usdc_per_sec: 100,
            client_public_key: None,
        };
        let access = covenant_compute_protocol::LeaseAccess {
            job_id: Uuid::nil(),
            endpoint: "ssh root@203.0.113.9 -p 22".into(),
            ready_at_ms: 0,
            note: None,
        };

        // A live, accepted lease serves its endpoint.
        let mut live = lease_record(Uuid::new_v4(), terms.clone(), None);
        live.phase = JobPhase::Accepted;
        live.lease_access = Some(access.clone());
        assert_eq!(live.live_lease_access(), Some(access.clone()));

        // Once it concludes, the machine is gone. The grant is not served,
        // so the buyer is never handed a dead address to try.
        for phase in [
            JobPhase::Completed,
            JobPhase::Failed,
            JobPhase::Refunded,
            JobPhase::Rejected,
        ] {
            let mut done = lease_record(Uuid::new_v4(), terms.clone(), None);
            done.phase = phase;
            done.lease_access = Some(access.clone());
            assert_eq!(
                done.live_lease_access(),
                None,
                "{phase:?} kept serving a concluded lease's access"
            );
        }
    }

    #[test]
    fn payout_totals_owes_a_lease_its_metered_draw_not_the_window_ceiling() {
        let book = JobBook::new();
        let terms = LeaseTerms {
            max_duration_secs: 600,
            rate_micro_usdc_per_sec: 100,
            client_public_key: None,
        };
        // Ran 5s of the 600s window → metered 500; a 20-micro fee leaves
        // 480 owed. Sizing from the ceiling would report 60_000 − 20 =
        // 59_980 and, on the retry sweep, pay it.
        let owed = Uuid::new_v4();
        let mut r = lease_record(owed, terms, Some(5_000));
        r.fee_micro_usdc = 20;
        book.insert(owed, r).unwrap();

        assert_eq!(book.payout_totals(), (0, 480));
    }

    #[test]
    fn insert_get_and_phase_transitions() {
        let book = JobBook::new();
        let job_id = Uuid::new_v4();
        book.insert(job_id, record(job_id, 0, 5_000)).unwrap();
        assert_eq!(book.get(job_id).unwrap().phase, JobPhase::Offered);

        book.set_phase(job_id, JobPhase::Accepted).unwrap();
        assert_eq!(book.get(job_id).unwrap().phase, JobPhase::Accepted);
    }

    #[test]
    fn unknown_job_operations_error() {
        let book = JobBook::new();
        let job_id = Uuid::new_v4();
        assert!(matches!(
            book.set_phase(job_id, JobPhase::Accepted),
            Err(JobError::NotFound(id)) if id == job_id
        ));
        assert!(book.get(job_id).is_none());
    }

    #[test]
    fn expired_finds_only_offered_or_accepted_past_deadline() {
        let book = JobBook::new();
        let past = Uuid::new_v4();
        let future = Uuid::new_v4();
        let completed = Uuid::new_v4();
        book.insert(past, record(past, 0, 1_000)).unwrap(); // deadline at t=1000
        book.insert(future, record(future, 0, 100_000)).unwrap();
        let mut done = record(completed, 0, 1_000);
        done.phase = JobPhase::Completed;
        book.insert(completed, done).unwrap();

        let expired = book.expired(2_000);
        assert_eq!(expired, vec![past]);
    }

    #[test]
    fn stale_offered_selects_matched_offers_past_the_age_including_pinned_probes() {
        let book = JobBook::new();
        let now = 100_000;
        let age = 30_000;

        let stale = Uuid::new_v4();
        let mut r = record(stale, 60_000, 300_000);
        r.offered_at_ms = 60_000; // 40s ago
        book.insert(stale, r).unwrap();

        let fresh = Uuid::new_v4();
        let mut r = record(fresh, 90_000, 300_000);
        r.offered_at_ms = 90_000; // 10s ago
        book.insert(fresh, r).unwrap();

        let accepted = Uuid::new_v4();
        let mut r = record(accepted, 60_000, 300_000);
        r.offered_at_ms = 60_000;
        r.phase = JobPhase::Accepted;
        book.insert(accepted, r).unwrap();

        // A pinned probe rides the worklist too: the sweep can only ever
        // redeliver it to its own operator, but a queue lost on restart
        // still needs that heal.
        let probe = Uuid::new_v4();
        let mut r = record(probe, 60_000, 300_000);
        r.offered_at_ms = 60_000;
        r.pinned = true;
        book.insert(probe, r).unwrap();

        let expired = Uuid::new_v4();
        let mut r = record(expired, 60_000, 1_000); // dead at t=61s
        r.offered_at_ms = 60_000;
        book.insert(expired, r).unwrap();

        let never_matched = Uuid::new_v4();
        let mut r = record(never_matched, 60_000, 300_000);
        r.offered_at_ms = 0;
        r.operator_pubkey_b58 = String::new();
        r.phase = JobPhase::Refunded;
        book.insert(never_matched, r).unwrap();

        // A pre-upgrade journal row deserializes with offered_at_ms 0:
        // maximally stale, exactly what a restart-restored offer needs.
        let restored = Uuid::new_v4();
        let mut r = record(restored, 60_000, 300_000);
        r.offered_at_ms = 0;
        book.insert(restored, r).unwrap();

        let mut ids: Vec<Uuid> = book
            .stale_offered(now, age)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        ids.sort();
        let mut expected = vec![stale, restored, probe];
        expected.sort();
        assert_eq!(ids, expected);
    }

    #[test]
    fn reassign_repoints_a_still_offered_job_and_survives_replay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let job_id = Uuid::new_v4();

        {
            let journal = Arc::new(Journal::open(&path).unwrap());
            let book = JobBook::restore(HashMap::new(), journal);
            book.insert(job_id, record(job_id, 1_000, 300_000)).unwrap();
            assert!(book
                .reassign(
                    job_id,
                    "operator-pubkey",
                    "second-operator",
                    "second-payout".into(),
                    Some("partner-b".into()),
                    45_000,
                )
                .unwrap());
        }

        let restored = Journal::load(&path).unwrap();
        let book = JobBook::restore(restored.jobs, Arc::new(Journal::open(&path).unwrap()));
        let r = book.get(job_id).unwrap();
        assert_eq!(r.operator_pubkey_b58, "second-operator");
        assert_eq!(r.payout_address, "second-payout");
        assert_eq!(r.referral_code, Some("partner-b".into()));
        assert_eq!(r.offered_at_ms, 45_000);
        assert_eq!(r.phase, JobPhase::Offered);
    }

    #[test]
    fn reassign_refuses_once_the_job_moved_on() {
        let book = JobBook::new();

        // The assignee's accept won the race: no write.
        let accepted = Uuid::new_v4();
        book.insert(accepted, record(accepted, 1_000, 300_000))
            .unwrap();
        book.set_phase(accepted, JobPhase::Accepted).unwrap();
        assert!(!book
            .reassign(
                accepted,
                "operator-pubkey",
                "second-operator",
                "second-payout".into(),
                None,
                45_000,
            )
            .unwrap());
        assert_eq!(
            book.get(accepted).unwrap().operator_pubkey_b58,
            "operator-pubkey"
        );

        // Another reassignment won: the expected assignee is stale.
        let moved = Uuid::new_v4();
        book.insert(moved, record(moved, 1_000, 300_000)).unwrap();
        assert!(!book
            .reassign(
                moved,
                "someone-else",
                "second-operator",
                "second-payout".into(),
                None,
                45_000,
            )
            .unwrap());

        assert!(matches!(
            book.reassign(
                Uuid::new_v4(),
                "operator-pubkey",
                "second-operator",
                "second-payout".into(),
                None,
                45_000,
            ),
            Err(JobError::NotFound(_))
        ));
    }

    #[test]
    fn touch_offered_restarts_the_stale_clock_only_while_assigned() {
        let book = JobBook::new();
        let job_id = Uuid::new_v4();
        book.insert(job_id, record(job_id, 1_000, 300_000)).unwrap();

        assert!(book
            .touch_offered(job_id, "operator-pubkey", 45_000)
            .unwrap());
        let r = book.get(job_id).unwrap();
        assert_eq!(r.offered_at_ms, 45_000);
        assert_eq!(r.payout_address, "operator-payout", "terms stay as won");

        assert!(!book.touch_offered(job_id, "someone-else", 50_000).unwrap());
        assert_eq!(book.get(job_id).unwrap().offered_at_ms, 45_000);
    }

    #[test]
    fn set_phase_if_assigned_refuses_a_reassigned_away_operator() {
        let book = JobBook::new();
        let job_id = Uuid::new_v4();
        book.insert(job_id, record(job_id, 1_000, 300_000)).unwrap();
        book.reassign(
            job_id,
            "operator-pubkey",
            "second-operator",
            "second-payout".into(),
            None,
            45_000,
        )
        .unwrap();

        // The old assignee's late accept must not land...
        assert!(!book
            .set_phase_if_assigned(job_id, "operator-pubkey", JobPhase::Accepted, None)
            .unwrap());
        assert_eq!(book.get(job_id).unwrap().phase, JobPhase::Offered);

        // ...while the new assignee's does.
        assert!(book
            .set_phase_if_assigned(job_id, "second-operator", JobPhase::Accepted, None)
            .unwrap());
        assert_eq!(book.get(job_id).unwrap().phase, JobPhase::Accepted);
    }

    #[test]
    fn set_phase_if_assigned_refuses_to_resurrect_a_concluded_job() {
        let book = JobBook::new();
        let job_id = Uuid::new_v4();
        book.insert(job_id, record(job_id, 1_000, 300_000)).unwrap();
        book.set_phase(job_id, JobPhase::Refunded).unwrap();

        // The assignee's late accept must not flip a refunded job back
        // to a live phase the ceilings and sweeps would count forever —
        // its escrow is already settled, so nothing would ever conclude
        // it again.
        assert!(!book
            .set_phase_if_assigned(job_id, "operator-pubkey", JobPhase::Accepted, None)
            .unwrap());
        assert_eq!(book.get(job_id).unwrap().phase, JobPhase::Refunded);

        // A live job still accepts, and an accept retry whose first ack
        // was lost lands idempotently.
        let live = Uuid::new_v4();
        book.insert(live, record(live, 1_000, 300_000)).unwrap();
        assert!(book
            .set_phase_if_assigned(live, "operator-pubkey", JobPhase::Accepted, None)
            .unwrap());
        assert!(book
            .set_phase_if_assigned(live, "operator-pubkey", JobPhase::Accepted, None)
            .unwrap());
    }

    #[test]
    fn a_preposterous_deadline_saturates_instead_of_overflowing() {
        let book = JobBook::new();
        // issued_at + deadline would overflow u64: the sum saturates to
        // u64::MAX (never before `now`), so the job is not expired and
        // the check never panics on the buyer-supplied numbers.
        let huge = Uuid::new_v4();
        book.insert(huge, record(huge, u64::MAX, u64::MAX)).unwrap();
        assert!(book.expired(u64::MAX).is_empty());
    }

    #[test]
    fn partner_accruals_sum_both_sides_and_count_each_job_once() {
        let book = JobBook::new();

        // Job 1: operator referred by a, buyer by b.
        let two_sided = Uuid::new_v4();
        let mut r = record(two_sided, 0, 5_000);
        r.referral_code = Some("a".into());
        r.partner_share_micro_usdc = 500;
        r.buyer_referral_code = Some("b".into());
        r.buyer_partner_share_micro_usdc = 300;
        book.insert(two_sided, r).unwrap();

        // Job 2: the same partner on both sides — one job, both shares.
        let same_code = Uuid::new_v4();
        let mut r = record(same_code, 0, 5_000);
        r.referral_code = Some("a".into());
        r.partner_share_micro_usdc = 40;
        r.buyer_referral_code = Some("a".into());
        r.buyer_partner_share_micro_usdc = 10;
        book.insert(same_code, r).unwrap();

        // Job 3: attributed but nothing accrued (no fee configured).
        let zero = Uuid::new_v4();
        let mut r = record(zero, 0, 5_000);
        r.buyer_referral_code = Some("b".into());
        book.insert(zero, r).unwrap();

        let accruals = book.partner_accruals();
        assert_eq!(accruals.get("a"), Some(&(550, 2)));
        assert_eq!(accruals.get("b"), Some(&(300, 1)));
    }

    #[test]
    fn a_restored_book_journals_mutations_and_replays_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let job_id = Uuid::new_v4();

        {
            let journal = Arc::new(Journal::open(&path).unwrap());
            let book = JobBook::restore(HashMap::new(), journal);
            book.insert(job_id, record(job_id, 0, 5_000)).unwrap();
            book.set_phase(job_id, JobPhase::Accepted).unwrap();
        }

        let restored = Journal::load(&path).unwrap();
        let book = JobBook::restore(restored.jobs, Arc::new(Journal::open(&path).unwrap()));
        assert_eq!(book.get(job_id).unwrap().phase, JobPhase::Accepted);
    }

    #[test]
    fn by_operator_filters_to_the_assignee_newest_first() {
        let book = JobBook::new();
        let old = Uuid::new_v4();
        let new = Uuid::new_v4();
        let other = Uuid::new_v4();
        book.insert(old, record(old, 1_000, 5_000)).unwrap();
        book.insert(new, record(new, 2_000, 5_000)).unwrap();
        let mut foreign = record(other, 3_000, 5_000);
        foreign.operator_pubkey_b58 = "someone-else".into();
        book.insert(other, foreign).unwrap();

        let rows = book.by_operator("operator-pubkey");
        assert_eq!(
            rows.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![new, old]
        );
    }

    #[test]
    fn set_payout_survives_a_journal_replay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let job_id = Uuid::new_v4();
        let outcome = PayoutOutcome {
            amount_micro_usdc: 95,
            tx_signature: Some("devnet-sig".into()),
            recorded_at_ms: 42,
        };

        {
            let journal = Arc::new(Journal::open(&path).unwrap());
            let book = JobBook::restore(HashMap::new(), journal);
            book.insert(job_id, record(job_id, 0, 5_000)).unwrap();
            book.set_payout(job_id, outcome.clone()).unwrap();
        }

        let restored = Journal::load(&path).unwrap();
        let book = JobBook::restore(restored.jobs, Arc::new(Journal::open(&path).unwrap()));
        assert_eq!(book.get(job_id).unwrap().payout, Some(outcome));
    }

    #[test]
    fn unpaid_conclusions_pin_their_reason_and_it_survives_replay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let swept = Uuid::new_v4();
        let cancelled = Uuid::new_v4();

        {
            let journal = Arc::new(Journal::open(&path).unwrap());
            let book = JobBook::restore(HashMap::new(), journal);
            book.insert(swept, record(swept, 0, 5_000)).unwrap();
            book.insert(cancelled, record(cancelled, 0, 5_000)).unwrap();
            book.conclude_unpaid(swept, JobPhase::Refunded, RefundReason::DeadlineExpired)
                .unwrap();
            assert!(book.cancel_if_offered(cancelled).unwrap());
        }

        let restored = Journal::load(&path).unwrap();
        let book = JobBook::restore(restored.jobs, Arc::new(Journal::open(&path).unwrap()));
        let swept_record = book.get(swept).unwrap();
        assert_eq!(swept_record.phase, JobPhase::Refunded);
        assert_eq!(
            swept_record.refund_reason,
            Some(RefundReason::DeadlineExpired)
        );
        let cancelled_record = book.get(cancelled).unwrap();
        assert_eq!(cancelled_record.phase, JobPhase::Refunded);
        assert_eq!(
            cancelled_record.refund_reason,
            Some(RefundReason::BuyerCancelled)
        );
    }

    #[test]
    fn a_journal_row_from_before_the_reason_field_reads_as_none() {
        let job_id = Uuid::new_v4();
        let mut row = serde_json::to_value(record(job_id, 0, 5_000)).unwrap();
        row.as_object_mut().unwrap().remove("refund_reason");
        let old: JobRecord = serde_json::from_value(row).unwrap();
        assert_eq!(old.refund_reason, None);
    }
}
