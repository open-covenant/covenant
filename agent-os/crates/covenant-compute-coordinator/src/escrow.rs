//! `FederationEscrow` v1: a coordinator-custodial ledger
//! (design-02-federation.md §2.1/§6, MASTER-PLAN.md).
//!
//! USDC-micro accounting only — no real inbound buyer payment
//! collection and no real outbound transfer happen anywhere in this
//! crate. This type's job is proving the hold/release/refund state
//! machine and the `funding_source` tag; moving money is the separate,
//! not-built-here [`crate::payout::Payout`] seam. Built via `restore`,
//! the ledger journals every mutation so holds survive a coordinator
//! restart (see [`crate::journal`]).
//!
//! `release` is receipt-gated — mechanical, never discretionary
//! (build-notes-phase1-foundation.md §1.6 step 6): it re-verifies the
//! [`SignedWorkReceipt`] itself rather than trusting an already-verified
//! caller, the same belt-and-suspenders posture
//! `SignedJobEnvelope::verify` already takes toward its own
//! `payload_json`. This is the deliberate inverse of the unilateral
//! `has_one = client` release at `programs/settlement/src/lib.rs:931`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use covenant_a2a::A2ATaskStatus;
use covenant_compute_protocol::{
    EscrowError, EscrowHoldAttestation, EscrowStatus, FederationEscrow, FundingSource,
    RefundReason, SignedWorkReceipt,
};
use covenant_identity::LocalIdentity;
use covenant_types::AgentId;
use parking_lot::Mutex;
use uuid::Uuid;

use crate::accounts::{
    BuyerAccounts, BuyerWithdrawals, WithdrawalDebit, WithdrawalPush, WithdrawalState,
};
use crate::journal::{EscrowHoldState, Journal};

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The anti-faucet discipline (C6, MASTER-PLAN.md): total bootstrap
/// subsidy may never outrun real revenue. A bootstrap hold is refused
/// once committed subsidy would exceed
/// `floor + organic_released × max_ratio_bps / 10_000`. The floor is
/// the explicit, operator-budgeted cold-start allowance (with zero
/// organic revenue a pure ratio would forbid the first subsidized job
/// ever); the ratio caps growth so subsidy scales only with real
/// buyer spend. Integer basis points, never floats — this is money.
#[derive(Debug, Clone, Copy)]
pub struct SubsidyPolicy {
    max_ratio_bps: u32,
    floor_micro_usdc: u64,
}

impl SubsidyPolicy {
    /// Refuses a ratio above 10_000 bps: subsidy exceeding 1× organic
    /// revenue is the faucet pattern this type exists to prevent, so
    /// it is unrepresentable, not just discouraged.
    pub fn new(max_ratio_bps: u32, floor_micro_usdc: u64) -> Result<Self, String> {
        if max_ratio_bps > 10_000 {
            return Err(format!(
                "subsidy max_ratio_bps {max_ratio_bps} exceeds 10_000 (1:1 with organic revenue)"
            ));
        }
        Ok(Self {
            max_ratio_bps,
            floor_micro_usdc,
        })
    }

    pub fn max_ratio_bps(&self) -> u32 {
        self.max_ratio_bps
    }

    pub fn floor_micro_usdc(&self) -> u64 {
        self.floor_micro_usdc
    }

    fn ceiling(&self, organic_released_micro_usdc: u64) -> u64 {
        let earned =
            u128::from(organic_released_micro_usdc) * u128::from(self.max_ratio_bps) / 10_000;
        self.floor_micro_usdc
            .saturating_add(u64::try_from(earned).unwrap_or(u64::MAX))
    }
}

/// A point-in-time view of the subsidy books, derived from the holds
/// ledger on read — what `GET /federation/subsidy` serves so a
/// bootstrap deployment's discipline is checkable with zero reader
/// homework.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct SubsidyStatus {
    /// False means no policy is in effect: every bootstrap hold is
    /// refused (the kill-switch's off position), organic is unaffected.
    pub enforced: bool,
    /// True once an admin closed the subsidy at runtime. The close is
    /// a journaled one-way latch: `enforced` goes false and stays
    /// false across restarts even if the boot environment still
    /// carries a policy. Distinguishes "killed" from "never armed" for
    /// anyone watching the books.
    pub closed: bool,
    pub max_ratio_bps: u32,
    pub floor_micro_usdc: u64,
    /// Non-refunded bootstrap holds — pending subsidy counts against
    /// the ceiling too, so a burst of in-flight jobs can't overshoot.
    pub bootstrap_committed_micro_usdc: u64,
    /// Only `Released` organic holds: revenue that actually settled.
    pub organic_released_micro_usdc: u64,
    pub ceiling_micro_usdc: u64,
    pub remaining_micro_usdc: u64,
}

/// One funding source's holds summed by settlement state — the escrow
/// side of the conservation law `/metrics` exposes: for organic money,
/// deposits split exactly into available + withdrawn + held +
/// released, and refunded holds are the ones that went back to being
/// available.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EscrowMoneyTotals {
    pub held_micro_usdc: u64,
    pub released_micro_usdc: u64,
    pub refunded_micro_usdc: u64,
}

/// What [`CustodialEscrow::withdraw`] did with a sufficient-funds
/// request: `Requested` debited the book; `Duplicate` means the
/// withdrawal id was already debited (an honest retry) and nothing
/// moved. Insufficient funds surface as
/// [`EscrowError::InsufficientFunds`] instead.
#[derive(Debug)]
pub enum WithdrawOutcome {
    Requested(WithdrawalState),
    Duplicate(WithdrawalState),
}

/// In-memory unless built with [`CustodialEscrow::restore`], in which
/// case every hold/release/refund journals its new state before it
/// commits — fund-state mutations that can't be made durable fail
/// loudly (see [`crate::journal`]).
///
/// With buyer accounts attached (see [`CustodialEscrow::with_accounts`])
/// an `Organic` hold is only minted if the buyer's deposits cover it:
/// available = deposited − non-refunded organic holds, derived under
/// the holds lock so two concurrent holds can't both spend the same
/// deposit. A refund frees the funds by the status flip alone — no
/// separate balance credit exists to get lost. Without accounts the
/// hold is a custodial promise, the pre-A3 open mode.
pub struct CustodialEscrow {
    identity: LocalIdentity,
    default_funding_source: FundingSource,
    holds: Mutex<HashMap<Uuid, EscrowHoldState>>,
    journal: Option<Arc<Journal>>,
    accounts: Option<Arc<BuyerAccounts>>,
    withdrawals: Option<Arc<BuyerWithdrawals>>,
    subsidy: Option<SubsidyPolicy>,
    /// One-way runtime latch over `subsidy`. Set, the policy is dead:
    /// re-arming takes a fresh data directory, not a restart — see
    /// [`CustodialEscrow::close_subsidy`].
    subsidy_closed: AtomicBool,
}

impl CustodialEscrow {
    pub fn new(identity: LocalIdentity, default_funding_source: FundingSource) -> Self {
        Self {
            identity,
            default_funding_source,
            holds: Mutex::new(HashMap::new()),
            journal: None,
            accounts: None,
            withdrawals: None,
            subsidy: None,
            subsidy_closed: AtomicBool::new(false),
        }
    }

    /// Seeds the ledger with journal-recovered holds and journals every
    /// mutation from here on.
    pub fn restore(
        identity: LocalIdentity,
        default_funding_source: FundingSource,
        holds: HashMap<Uuid, EscrowHoldState>,
        journal: Arc<Journal>,
    ) -> Self {
        Self {
            identity,
            default_funding_source,
            holds: Mutex::new(holds),
            journal: Some(journal),
            accounts: None,
            withdrawals: None,
            subsidy: None,
            subsidy_closed: AtomicBool::new(false),
        }
    }

    /// Turns on prefunded-buyer enforcement: from here on an `Organic`
    /// hold must be covered by `accounts`' deposits.
    pub fn with_accounts(mut self, accounts: Arc<BuyerAccounts>) -> Self {
        self.accounts = Some(accounts);
        self
    }

    /// Attaches the withdrawal book. Withdrawals debit through
    /// [`CustodialEscrow::withdraw`] under the holds lock, and the
    /// prefunding check treats debited amounts as gone.
    pub fn with_withdrawals(mut self, withdrawals: Arc<BuyerWithdrawals>) -> Self {
        self.withdrawals = Some(withdrawals);
        self
    }

    /// Opens the bootstrap kill-switch this far: without a policy every
    /// `Bootstrap` hold is refused outright.
    pub fn with_subsidy_policy(mut self, policy: SubsidyPolicy) -> Self {
        self.subsidy = Some(policy);
        self
    }

    /// Restores a journal-replayed runtime close — how a restart keeps
    /// the latch shut even though its environment re-supplied a policy.
    pub fn with_subsidy_closed(self, closed: bool) -> Self {
        self.subsidy_closed.store(closed, Ordering::SeqCst);
        self
    }

    /// Closes the bootstrap subsidy for good: journal first, then
    /// latch, so a close that can't be made durable fails loudly with
    /// the subsidy still armed rather than dying with the process. One
    /// way on purpose — a leaked admin token can stop subsidy spend
    /// but never start it, and re-arming demands the full boot
    /// ceremony against a journal that carries no close. Returns
    /// whether this call did the closing (false for an honest retry of
    /// an already-closed switch, which re-journals harmlessly — replay
    /// keeps the first close).
    pub fn close_subsidy(&self) -> Result<bool, EscrowError> {
        if let Some(journal) = &self.journal {
            journal
                .record_subsidy_closed(epoch_ms())
                .map_err(|e| EscrowError::Backend(e.to_string()))?;
        }
        Ok(!self.subsidy_closed.swap(true, Ordering::SeqCst))
    }

    /// The policy currently in force: the configured one until the
    /// runtime latch closes it. Every subsidy decision reads through
    /// here so the gate and the public books can never disagree.
    fn effective_subsidy(&self) -> Option<SubsidyPolicy> {
        if self.subsidy_closed.load(Ordering::SeqCst) {
            return None;
        }
        self.subsidy
    }

    /// Journal-then-commit for a settled (released/refunded) status.
    fn settle(&self, job_id: Uuid, status: EscrowStatus) -> Result<(), EscrowError> {
        let mut guard = self.holds.lock();
        let hold = guard
            .get_mut(&job_id)
            .ok_or(EscrowError::NotFound(job_id))?;
        if hold.status != EscrowStatus::Held {
            return Err(EscrowError::AlreadySettled(job_id));
        }
        let settled = EscrowHoldState {
            status,
            ..hold.clone()
        };
        if let Some(journal) = &self.journal {
            journal
                .record_escrow(job_id, &settled)
                .map_err(|e| EscrowError::Backend(e.to_string()))?;
        }
        *hold = settled;
        Ok(())
    }

    /// A metered settlement: releases only `used_micro_usdc` of the
    /// hold and hands the remainder straight back to the buyer, in one
    /// journaled transition — the fund shape of a lease, where the
    /// buyer escrows a window's ceiling and pays for the seconds the
    /// session actually ran. The hold's own amount is written down to
    /// `used` as it releases: the balance derivation (deposits minus
    /// non-refunded holds) then frees the remainder with no second
    /// row, the subsidy books count only what was spent, and
    /// [`CustodialEscrow::hold_info`] answers the settled figure every
    /// downstream split (fee, payout, audit) must use. Same
    /// receipt-gating as [`FederationEscrow::release`]; `used` of zero
    /// settles as a plain refund; `used` above the hold is refused —
    /// a meter can clamp, never grow.
    pub async fn release_metered(
        &self,
        job_id: Uuid,
        receipt: &SignedWorkReceipt,
        used_micro_usdc: u64,
    ) -> Result<(), EscrowError> {
        receipt
            .verify()
            .map_err(|e| EscrowError::Backend(format!("receipt does not verify: {e}")))?;
        if receipt.receipt.job_id != job_id {
            return Err(EscrowError::Backend(format!(
                "receipt job_id {} does not match the hold being released {job_id}",
                receipt.receipt.job_id
            )));
        }
        if receipt.receipt.status != A2ATaskStatus::Ok {
            return Err(EscrowError::Backend(format!(
                "receipt status {:?} is not payable; only ok receipts release a hold",
                receipt.receipt.status
            )));
        }
        let mut guard = self.holds.lock();
        let hold = guard
            .get_mut(&job_id)
            .ok_or(EscrowError::NotFound(job_id))?;
        if hold.status != EscrowStatus::Held {
            return Err(EscrowError::AlreadySettled(job_id));
        }
        if used_micro_usdc > hold.amount_micro_usdc {
            return Err(EscrowError::Backend(format!(
                "metered release of {used_micro_usdc} micro-USDC exceeds the {} held for \
                 job {job_id}",
                hold.amount_micro_usdc
            )));
        }
        let settled = if used_micro_usdc == 0 {
            EscrowHoldState {
                status: EscrowStatus::Refunded,
                ..hold.clone()
            }
        } else {
            EscrowHoldState {
                amount_micro_usdc: used_micro_usdc,
                status: EscrowStatus::Released,
                ..hold.clone()
            }
        };
        if let Some(journal) = &self.journal {
            journal
                .record_escrow(job_id, &settled)
                .map_err(|e| EscrowError::Backend(e.to_string()))?;
        }
        *hold = settled;
        Ok(())
    }

    pub fn coordinator_pubkey_b58(&self) -> String {
        bs58::encode(self.identity.pubkey_bytes()).into_string()
    }

    pub fn agent_id(&self) -> AgentId {
        self.identity.agent_id()
    }

    /// The amount and funding tag a still-held (or already-settled)
    /// hold carries — what the HTTP layer needs after `release` to
    /// drive the audit row and the payout push. `None` for an unknown
    /// job id.
    pub fn hold_info(&self, job_id: Uuid) -> Option<(u64, FundingSource)> {
        self.holds
            .lock()
            .get(&job_id)
            .map(|h| (h.amount_micro_usdc, h.funding_source))
    }

    /// What a settled hold actually charged the buyer: the released amount
    /// (the whole price for a fixed job, the metered draw `release_metered`
    /// wrote the hold down to for a lease), or zero once refunded. `None`
    /// while the hold is still `Held` — nothing has settled to charge yet —
    /// and for an unknown job. Reads the escrow, the money authority, so a
    /// lease whose record lost its meter stamp to a crash still reports the
    /// charge the hold settled to, not the window ceiling the envelope names.
    pub fn settled_charge_micro_usdc(&self, job_id: Uuid) -> Option<u64> {
        self.holds.lock().get(&job_id).and_then(|h| match h.status {
            EscrowStatus::Released => Some(h.amount_micro_usdc),
            EscrowStatus::Refunded => Some(0),
            EscrowStatus::Held => None,
        })
    }

    /// Every hold with its durable state, cloned under the lock — the
    /// boot reconciliation's worklist (`crate::recover`), the same
    /// v1-small scan as every other book read.
    pub fn holds_snapshot(&self) -> Vec<(Uuid, EscrowHoldState)> {
        self.holds
            .lock()
            .iter()
            .map(|(id, h)| (*id, h.clone()))
            .collect()
    }

    /// What this buyer's deposits are currently consumed by: the sum of
    /// its non-refunded `Organic` holds. `Held` locks the funds,
    /// `Released` means they were spent (paid out to an operator);
    /// only `Refunded` gives them back. Available balance = deposited
    /// − this.
    pub fn organic_charged(&self, buyer_pubkey_b58: &str) -> u64 {
        Self::charged_of(&self.holds.lock(), buyer_pubkey_b58)
    }

    fn charged_of(holds: &HashMap<Uuid, EscrowHoldState>, buyer_pubkey_b58: &str) -> u64 {
        holds
            .values()
            .filter(|h| {
                h.buyer_pubkey_b58 == buyer_pubkey_b58
                    && h.funding_source == FundingSource::Organic
                    && h.status != EscrowStatus::Refunded
            })
            .map(|h| h.amount_micro_usdc)
            .sum()
    }

    fn withdrawn_of(&self, buyer_pubkey_b58: &str) -> u64 {
        self.withdrawals
            .as_ref()
            .map(|w| w.withdrawn(buyer_pubkey_b58))
            .unwrap_or(0)
    }

    /// Debits a verified withdrawal request against the buyer's
    /// available balance — deposits minus non-refunded organic holds
    /// minus prior withdrawals — and commits it to the withdrawal
    /// book. Runs under the holds lock, so a concurrent organic hold
    /// and a withdrawal can never both spend the same deposit: the
    /// hold's funds check and this debit serialize on the one lock
    /// that arbitrates funds.
    ///
    /// `accounts` comes from the caller because deposits are tracked
    /// even when prefunding enforcement (`with_accounts`) is off —
    /// what a buyer paid in is withdrawable regardless of whether
    /// holds are being checked against it.
    pub fn withdraw(
        &self,
        accounts: &BuyerAccounts,
        buyer_pubkey_b58: &str,
        withdrawal_id: Uuid,
        recipient_address_b58: &str,
        amount_micro_usdc: u64,
    ) -> Result<WithdrawOutcome, EscrowError> {
        let Some(withdrawals) = &self.withdrawals else {
            return Err(EscrowError::Backend(
                "no withdrawal book is attached to this escrow".into(),
            ));
        };
        let guard = self.holds.lock();
        let deposited = accounts.deposited(buyer_pubkey_b58);
        let charged = Self::charged_of(&guard, buyer_pubkey_b58);
        let debit = withdrawals
            .debit(
                WithdrawalState {
                    withdrawal_id,
                    buyer_pubkey_b58: buyer_pubkey_b58.into(),
                    recipient_address_b58: recipient_address_b58.into(),
                    amount_micro_usdc,
                    requested_at_ms: epoch_ms(),
                    pushed: None,
                },
                deposited.saturating_sub(charged),
            )
            .map_err(|e| EscrowError::Backend(e.to_string()))?;
        drop(guard);
        match debit {
            WithdrawalDebit::Requested(state) => Ok(WithdrawOutcome::Requested(state)),
            WithdrawalDebit::Duplicate(state) => Ok(WithdrawOutcome::Duplicate(state)),
            WithdrawalDebit::Insufficient {
                available_micro_usdc,
            } => Err(EscrowError::InsufficientFunds {
                buyer_pubkey_b58: buyer_pubkey_b58.into(),
                needed_micro_usdc: amount_micro_usdc,
                available_micro_usdc,
            }),
            // A random withdrawal id colliding with another buyer's is a
            // should-never-happen; refuse it without echoing that buyer's
            // record back to this one.
            WithdrawalDebit::IdHeldByAnotherBuyer => Err(EscrowError::Backend(
                "withdrawal id is already registered to another account".into(),
            )),
        }
    }

    /// Pins a completed backend transfer onto its withdrawal debit —
    /// thin passthrough to the attached book, here so callers hold one
    /// handle for the whole withdraw flow.
    pub fn record_withdrawal_pushed(
        &self,
        withdrawal_id: Uuid,
        push: WithdrawalPush,
    ) -> Result<Option<WithdrawalState>, EscrowError> {
        let Some(withdrawals) = &self.withdrawals else {
            return Err(EscrowError::Backend(
                "no withdrawal book is attached to this escrow".into(),
            ));
        };
        withdrawals
            .record_pushed(withdrawal_id, push)
            .map_err(|e| EscrowError::Backend(e.to_string()))
    }

    fn bootstrap_committed(holds: &HashMap<Uuid, EscrowHoldState>) -> u64 {
        holds
            .values()
            .filter(|h| {
                h.funding_source == FundingSource::Bootstrap && h.status != EscrowStatus::Refunded
            })
            .map(|h| h.amount_micro_usdc)
            .sum()
    }

    fn organic_released(holds: &HashMap<Uuid, EscrowHoldState>) -> u64 {
        holds
            .values()
            .filter(|h| {
                h.funding_source == FundingSource::Organic && h.status == EscrowStatus::Released
            })
            .map(|h| h.amount_micro_usdc)
            .sum()
    }

    /// Every hold of `funding_source` summed by settlement state, one
    /// pass under the holds lock — what `/metrics` serves so the
    /// conservation identity is checkable by a scraper.
    pub fn money_totals(&self, funding_source: FundingSource) -> EscrowMoneyTotals {
        let holds = self.holds.lock();
        let mut totals = EscrowMoneyTotals::default();
        for hold in holds
            .values()
            .filter(|h| h.funding_source == funding_source)
        {
            match hold.status {
                EscrowStatus::Held => {
                    totals.held_micro_usdc = totals
                        .held_micro_usdc
                        .saturating_add(hold.amount_micro_usdc)
                }
                EscrowStatus::Released => {
                    totals.released_micro_usdc = totals
                        .released_micro_usdc
                        .saturating_add(hold.amount_micro_usdc)
                }
                EscrowStatus::Refunded => {
                    totals.refunded_micro_usdc = totals
                        .refunded_micro_usdc
                        .saturating_add(hold.amount_micro_usdc)
                }
            }
        }
        totals
    }

    pub fn subsidy_status(&self) -> SubsidyStatus {
        let holds = self.holds.lock();
        let bootstrap_committed = Self::bootstrap_committed(&holds);
        let organic_released = Self::organic_released(&holds);
        let subsidy = self.effective_subsidy();
        let ceiling = subsidy.map(|p| p.ceiling(organic_released)).unwrap_or(0);
        SubsidyStatus {
            enforced: subsidy.is_some(),
            closed: self.subsidy_closed.load(Ordering::SeqCst),
            max_ratio_bps: subsidy.map(|p| p.max_ratio_bps()).unwrap_or(0),
            floor_micro_usdc: subsidy.map(|p| p.floor_micro_usdc()).unwrap_or(0),
            bootstrap_committed_micro_usdc: bootstrap_committed,
            organic_released_micro_usdc: organic_released,
            ceiling_micro_usdc: ceiling,
            remaining_micro_usdc: ceiling.saturating_sub(bootstrap_committed),
        }
    }

    /// [`FederationEscrow::hold`] with an explicit funding tag instead
    /// of the configured default. The canary prober is the one caller:
    /// probe spend is the coordinator's own money, so it is always
    /// `Bootstrap` — bounded by the subsidy kill-switch — whatever tag
    /// organic traffic gets.
    pub async fn hold_with_source(
        &self,
        job_id: Uuid,
        buyer: &AgentId,
        amount_micro_usdc: u64,
        funding_source: FundingSource,
    ) -> Result<EscrowHoldAttestation, EscrowError> {
        let attestation = EscrowHoldAttestation::sign(
            job_id,
            amount_micro_usdc,
            funding_source,
            epoch_ms(),
            &self.identity,
        )
        .map_err(|e| EscrowError::Backend(e.to_string()))?;
        let hold = EscrowHoldState {
            amount_micro_usdc,
            funding_source,
            status: EscrowStatus::Held,
            buyer_pubkey_b58: buyer.pubkey_base58(),
        };
        let mut guard = self.holds.lock();
        // Idempotency, checked under the same lock as the insert so
        // concurrent submissions of one job id can't both land: a hold
        // is keyed by job id and is never overwritten. Overwriting one
        // would let a replayed job envelope (its signed bytes reproduce
        // verbatim) or an honest retry re-hold a settled job, re-match
        // it, and pay a second operator for work already done. The
        // caller reads this as an idempotent duplicate, not a failure.
        if guard.contains_key(&job_id) {
            return Err(EscrowError::AlreadyHeld(job_id));
        }
        // Funds check and insert under the same lock hold: deposits
        // only grow, so the worst a concurrent deposit can cause is a
        // conservative refusal, never an overdraft.
        if let Some(accounts) = &self.accounts {
            if hold.funding_source == FundingSource::Organic {
                let deposited = accounts.deposited(&hold.buyer_pubkey_b58);
                let charged = Self::charged_of(&guard, &hold.buyer_pubkey_b58);
                let withdrawn = self.withdrawn_of(&hold.buyer_pubkey_b58);
                let available = deposited.saturating_sub(charged).saturating_sub(withdrawn);
                if amount_micro_usdc > available {
                    return Err(EscrowError::InsufficientFunds {
                        buyer_pubkey_b58: hold.buyer_pubkey_b58,
                        needed_micro_usdc: amount_micro_usdc,
                        available_micro_usdc: available,
                    });
                }
            }
        }
        // Subsidy gate, same lock: no policy in force — none configured
        // or the runtime latch closed it — means no bootstrap holds at
        // all (fail closed), and pending subsidy counts against the
        // ceiling so concurrent bootstrap jobs can't overshoot it.
        if hold.funding_source == FundingSource::Bootstrap {
            let spent = Self::bootstrap_committed(&guard);
            let ceiling = self
                .effective_subsidy()
                .map(|p| p.ceiling(Self::organic_released(&guard)))
                .unwrap_or(0);
            if amount_micro_usdc > ceiling.saturating_sub(spent) {
                return Err(EscrowError::SubsidyExhausted {
                    spent_micro_usdc: spent,
                    ceiling_micro_usdc: ceiling,
                });
            }
        }
        if let Some(journal) = &self.journal {
            journal
                .record_escrow(job_id, &hold)
                .map_err(|e| EscrowError::Backend(e.to_string()))?;
        }
        guard.insert(job_id, hold);
        Ok(attestation)
    }
}

#[async_trait]
impl FederationEscrow for CustodialEscrow {
    async fn hold(
        &self,
        job_id: Uuid,
        buyer: &AgentId,
        amount_micro_usdc: u64,
    ) -> Result<EscrowHoldAttestation, EscrowError> {
        self.hold_with_source(
            job_id,
            buyer,
            amount_micro_usdc,
            self.default_funding_source,
        )
        .await
    }

    async fn release(&self, job_id: Uuid, receipt: &SignedWorkReceipt) -> Result<(), EscrowError> {
        receipt
            .verify()
            .map_err(|e| EscrowError::Backend(format!("receipt does not verify: {e}")))?;
        if receipt.receipt.job_id != job_id {
            return Err(EscrowError::Backend(format!(
                "receipt job_id {} does not match the hold being released {job_id}",
                receipt.receipt.job_id
            )));
        }
        // Only an `Ok` receipt is payable. This is checked here, at the
        // fund boundary, not just in the HTTP layer: pay-for-failure is
        // a drain vector (register, accept, fail instantly, collect), so
        // no caller — present or future — gets to release a hold against
        // a failure receipt. `Partial` is conservatively not payable
        // until metering defines what a partial job is worth.
        if receipt.receipt.status != A2ATaskStatus::Ok {
            return Err(EscrowError::Backend(format!(
                "receipt status {:?} is not payable; only ok receipts release a hold",
                receipt.receipt.status
            )));
        }
        self.settle(job_id, EscrowStatus::Released)
    }

    async fn refund(&self, job_id: Uuid, _reason: RefundReason) -> Result<(), EscrowError> {
        self.settle(job_id, EscrowStatus::Refunded)
    }

    async fn status(&self, job_id: Uuid) -> Result<EscrowStatus, EscrowError> {
        self.holds
            .lock()
            .get(&job_id)
            .map(|h| h.status)
            .ok_or(EscrowError::NotFound(job_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_compute_protocol::{JobMeter, WorkReceiptPayload};

    fn receipt_for(job_id: Uuid, operator: &LocalIdentity, price: u64) -> SignedWorkReceipt {
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

    #[tokio::test]
    async fn hold_then_release_with_a_verified_receipt() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let buyer = LocalIdentity::generate("buyer@local").agent_id();
        let operator = LocalIdentity::generate("operator@local");
        let escrow = CustodialEscrow::new(coordinator, FundingSource::Organic);

        let job_id = Uuid::new_v4();
        let attestation = escrow.hold(job_id, &buyer, 5_000).await.unwrap();
        assert_eq!(attestation.amount_micro_usdc, 5_000);
        assert_eq!(escrow.status(job_id).await.unwrap(), EscrowStatus::Held);

        let receipt = receipt_for(job_id, &operator, 5_000);
        escrow.release(job_id, &receipt).await.unwrap();
        assert_eq!(escrow.status(job_id).await.unwrap(), EscrowStatus::Released);
        assert_eq!(
            escrow.hold_info(job_id),
            Some((5_000, FundingSource::Organic))
        );
    }

    #[tokio::test]
    async fn settled_charge_reads_the_hold_not_the_ceiling() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let buyer = LocalIdentity::generate("buyer@local").agent_id();
        let operator = LocalIdentity::generate("operator@local");
        let escrow = CustodialEscrow::new(coordinator, FundingSource::Organic);

        // Nothing has settled for an unknown or a still-held job.
        assert_eq!(escrow.settled_charge_micro_usdc(Uuid::new_v4()), None);
        let metered = Uuid::new_v4();
        escrow.hold(metered, &buyer, 3_600_000).await.unwrap(); // a lease's window ceiling
        assert_eq!(escrow.settled_charge_micro_usdc(metered), None);

        // A metered release writes the hold down to the seconds served, so the
        // charge is that draw — not the window ceiling the buyer escrowed.
        let receipt = receipt_for(metered, &operator, 3_600_000);
        escrow
            .release_metered(metered, &receipt, 60_000)
            .await
            .unwrap();
        assert_eq!(escrow.settled_charge_micro_usdc(metered), Some(60_000));

        // A whole release charges the whole price.
        let whole = Uuid::new_v4();
        escrow.hold(whole, &buyer, 5_000).await.unwrap();
        escrow
            .release(whole, &receipt_for(whole, &operator, 5_000))
            .await
            .unwrap();
        assert_eq!(escrow.settled_charge_micro_usdc(whole), Some(5_000));

        // A refund charged nothing.
        let refunded = Uuid::new_v4();
        escrow.hold(refunded, &buyer, 5_000).await.unwrap();
        escrow
            .refund(refunded, RefundReason::DeadlineExpired)
            .await
            .unwrap();
        assert_eq!(escrow.settled_charge_micro_usdc(refunded), Some(0));
    }

    #[tokio::test]
    async fn release_rejects_a_receipt_that_does_not_verify() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let buyer = LocalIdentity::generate("buyer@local").agent_id();
        let operator = LocalIdentity::generate("operator@local");
        let escrow = CustodialEscrow::new(coordinator, FundingSource::Organic);

        let job_id = Uuid::new_v4();
        escrow.hold(job_id, &buyer, 5_000).await.unwrap();

        let mut receipt = receipt_for(job_id, &operator, 5_000);
        receipt.receipt.price_micro_usdc = 1; // tampered post-signing
        assert!(escrow.release(job_id, &receipt).await.is_err());
        assert_eq!(
            escrow.status(job_id).await.unwrap(),
            EscrowStatus::Held,
            "a receipt-less (invalid-receipt) release must never move funds"
        );
    }

    #[tokio::test]
    async fn release_refuses_a_non_ok_receipt() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let buyer = LocalIdentity::generate("buyer@local").agent_id();
        let operator = LocalIdentity::generate("operator@local");
        let escrow = CustodialEscrow::new(coordinator, FundingSource::Organic);

        let job_id = Uuid::new_v4();
        escrow.hold(job_id, &buyer, 5_000).await.unwrap();

        for status in [A2ATaskStatus::Error, A2ATaskStatus::Partial] {
            let payload = WorkReceiptPayload {
                status,
                ..receipt_for(job_id, &operator, 5_000).receipt
            };
            let receipt = SignedWorkReceipt::sign(payload, &operator).unwrap();
            assert!(escrow.release(job_id, &receipt).await.is_err());
            assert_eq!(
                escrow.status(job_id).await.unwrap(),
                EscrowStatus::Held,
                "a failure receipt must never move funds"
            );
        }
    }

    #[tokio::test]
    async fn release_rejects_a_receipt_for_a_different_job() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let buyer = LocalIdentity::generate("buyer@local").agent_id();
        let operator = LocalIdentity::generate("operator@local");
        let escrow = CustodialEscrow::new(coordinator, FundingSource::Organic);

        let job_id = Uuid::new_v4();
        escrow.hold(job_id, &buyer, 5_000).await.unwrap();
        let other_receipt = receipt_for(Uuid::new_v4(), &operator, 5_000);
        assert!(escrow.release(job_id, &other_receipt).await.is_err());
        assert_eq!(escrow.status(job_id).await.unwrap(), EscrowStatus::Held);
    }

    #[tokio::test]
    async fn refund_then_release_is_rejected_already_settled() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let buyer = LocalIdentity::generate("buyer@local").agent_id();
        let operator = LocalIdentity::generate("operator@local");
        let escrow = CustodialEscrow::new(coordinator, FundingSource::Organic);

        let job_id = Uuid::new_v4();
        escrow.hold(job_id, &buyer, 5_000).await.unwrap();
        escrow
            .refund(job_id, RefundReason::DeadlineExpired)
            .await
            .unwrap();
        assert_eq!(escrow.status(job_id).await.unwrap(), EscrowStatus::Refunded);

        let receipt = receipt_for(job_id, &operator, 5_000);
        assert!(matches!(
            escrow.release(job_id, &receipt).await,
            Err(EscrowError::AlreadySettled(id)) if id == job_id
        ));
    }

    #[tokio::test]
    async fn a_second_hold_on_a_job_id_is_refused_in_every_state() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let buyer = LocalIdentity::generate("buyer@local").agent_id();
        let operator = LocalIdentity::generate("operator@local");
        let accounts = Arc::new(crate::accounts::BuyerAccounts::new());
        let escrow = CustodialEscrow::new(coordinator, FundingSource::Organic)
            .with_accounts(accounts.clone());
        accounts
            .credit_deposit("sig-1", &buyer.pubkey_base58(), 30_000)
            .unwrap();

        // Held: a replay of a live job must not re-hold it.
        let held = Uuid::new_v4();
        escrow.hold(held, &buyer, 5_000).await.unwrap();
        assert!(matches!(
            escrow.hold(held, &buyer, 5_000).await,
            Err(EscrowError::AlreadyHeld(id)) if id == held
        ));

        // Refunded: the classic replay — a job that already resolved,
        // its escrow given back, re-submitted verbatim. The refund
        // stands; the ledger is not reopened.
        let refunded = Uuid::new_v4();
        escrow.hold(refunded, &buyer, 5_000).await.unwrap();
        escrow
            .refund(refunded, RefundReason::DeadlineExpired)
            .await
            .unwrap();
        assert!(matches!(
            escrow.hold(refunded, &buyer, 5_000).await,
            Err(EscrowError::AlreadyHeld(id)) if id == refunded
        ));
        assert_eq!(
            escrow.status(refunded).await.unwrap(),
            EscrowStatus::Refunded,
            "a refused re-hold must not flip a settled hold back to Held"
        );

        // Released: the worst case — re-holding a paid job would re-match
        // it and pay a second operator for work already delivered.
        let released = Uuid::new_v4();
        escrow.hold(released, &buyer, 5_000).await.unwrap();
        let receipt = receipt_for(released, &operator, 5_000);
        escrow.release(released, &receipt).await.unwrap();
        assert!(matches!(
            escrow.hold(released, &buyer, 5_000).await,
            Err(EscrowError::AlreadyHeld(id)) if id == released
        ));
        assert_eq!(
            escrow.status(released).await.unwrap(),
            EscrowStatus::Released
        );

        // Only three job ids ever consumed a deposit: 5_000 each. A
        // successful re-hold would have shown up as a fourth charge.
        assert_eq!(escrow.organic_charged(&buyer.pubkey_base58()), 10_000);
    }

    #[tokio::test]
    async fn organic_holds_require_a_covering_deposit_when_accounts_are_attached() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let buyer = LocalIdentity::generate("buyer@local").agent_id();
        let accounts = Arc::new(crate::accounts::BuyerAccounts::new());
        let escrow = CustodialEscrow::new(coordinator, FundingSource::Organic)
            .with_accounts(accounts.clone());

        let unfunded = escrow.hold(Uuid::new_v4(), &buyer, 5_000).await;
        assert!(matches!(
            unfunded,
            Err(EscrowError::InsufficientFunds {
                needed_micro_usdc: 5_000,
                available_micro_usdc: 0,
                ..
            })
        ));

        accounts
            .credit_deposit("sig-1", &buyer.pubkey_base58(), 6_000)
            .unwrap();
        escrow.hold(Uuid::new_v4(), &buyer, 5_000).await.unwrap();
        assert_eq!(escrow.organic_charged(&buyer.pubkey_base58()), 5_000);

        // 1_000 left: a 2_000 hold overdraws and is refused.
        assert!(matches!(
            escrow.hold(Uuid::new_v4(), &buyer, 2_000).await,
            Err(EscrowError::InsufficientFunds {
                needed_micro_usdc: 2_000,
                available_micro_usdc: 1_000,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn withdrawals_and_holds_arbitrate_over_the_same_deposit() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let buyer = LocalIdentity::generate("buyer@local").agent_id();
        let accounts = Arc::new(crate::accounts::BuyerAccounts::new());
        let withdrawals = Arc::new(crate::accounts::BuyerWithdrawals::new());
        let escrow = CustodialEscrow::new(coordinator, FundingSource::Organic)
            .with_accounts(accounts.clone())
            .with_withdrawals(withdrawals.clone());
        let buyer_b58 = buyer.pubkey_base58();
        accounts
            .credit_deposit("sig-1", &buyer_b58, 10_000)
            .unwrap();

        // 4_000 held: only 6_000 is withdrawable.
        escrow.hold(Uuid::new_v4(), &buyer, 4_000).await.unwrap();
        assert!(matches!(
            escrow.withdraw(&accounts, &buyer_b58, Uuid::new_v4(), "recipient", 7_000),
            Err(EscrowError::InsufficientFunds {
                needed_micro_usdc: 7_000,
                available_micro_usdc: 6_000,
                ..
            })
        ));

        let withdrawal_id = Uuid::new_v4();
        let outcome = escrow
            .withdraw(&accounts, &buyer_b58, withdrawal_id, "recipient", 6_000)
            .unwrap();
        assert!(matches!(outcome, WithdrawOutcome::Requested(ref s) if s.pushed.is_none()));
        // An honest retry of the same id moves nothing more.
        assert!(matches!(
            escrow
                .withdraw(&accounts, &buyer_b58, withdrawal_id, "recipient", 6_000)
                .unwrap(),
            WithdrawOutcome::Duplicate(_)
        ));
        assert_eq!(withdrawals.withdrawn(&buyer_b58), 6_000);

        // The withdrawn money is gone for holds too.
        assert!(matches!(
            escrow.hold(Uuid::new_v4(), &buyer, 1).await,
            Err(EscrowError::InsufficientFunds {
                available_micro_usdc: 0,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn a_withdrawal_never_touches_money_a_refund_has_not_freed() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let buyer = LocalIdentity::generate("buyer@local").agent_id();
        let accounts = Arc::new(crate::accounts::BuyerAccounts::new());
        let withdrawals = Arc::new(crate::accounts::BuyerWithdrawals::new());
        let escrow = CustodialEscrow::new(coordinator, FundingSource::Organic)
            .with_accounts(accounts.clone())
            .with_withdrawals(withdrawals);
        let buyer_b58 = buyer.pubkey_base58();
        accounts.credit_deposit("sig-1", &buyer_b58, 5_000).unwrap();

        let job_id = Uuid::new_v4();
        escrow.hold(job_id, &buyer, 5_000).await.unwrap();
        assert!(matches!(
            escrow.withdraw(&accounts, &buyer_b58, Uuid::new_v4(), "recipient", 1),
            Err(EscrowError::InsufficientFunds { .. })
        ));

        // The refund frees it; the whole deposit walks out.
        escrow
            .refund(job_id, RefundReason::DeadlineExpired)
            .await
            .unwrap();
        assert!(escrow
            .withdraw(&accounts, &buyer_b58, Uuid::new_v4(), "recipient", 5_000)
            .is_ok());
    }

    #[tokio::test]
    async fn a_refund_frees_the_deposit_but_a_release_spends_it() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let buyer = LocalIdentity::generate("buyer@local").agent_id();
        let operator = LocalIdentity::generate("operator@local");
        let accounts = Arc::new(crate::accounts::BuyerAccounts::new());
        let escrow = CustodialEscrow::new(coordinator, FundingSource::Organic)
            .with_accounts(accounts.clone());
        accounts
            .credit_deposit("sig-1", &buyer.pubkey_base58(), 5_000)
            .unwrap();

        let refunded_job = Uuid::new_v4();
        escrow.hold(refunded_job, &buyer, 5_000).await.unwrap();
        escrow
            .refund(refunded_job, RefundReason::DeadlineExpired)
            .await
            .unwrap();
        assert_eq!(escrow.organic_charged(&buyer.pubkey_base58()), 0);

        // The freed deposit funds a second job, which completes: spent
        // money stays charged forever.
        let released_job = Uuid::new_v4();
        escrow.hold(released_job, &buyer, 5_000).await.unwrap();
        let receipt = receipt_for(released_job, &operator, 5_000);
        escrow.release(released_job, &receipt).await.unwrap();
        assert_eq!(escrow.organic_charged(&buyer.pubkey_base58()), 5_000);
        assert!(matches!(
            escrow.hold(Uuid::new_v4(), &buyer, 1).await,
            Err(EscrowError::InsufficientFunds { .. })
        ));
    }

    #[tokio::test]
    async fn bootstrap_holds_bypass_the_funds_check() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let buyer = LocalIdentity::generate("buyer@local").agent_id();
        let accounts = Arc::new(crate::accounts::BuyerAccounts::new());
        let escrow = CustodialEscrow::new(coordinator, FundingSource::Bootstrap)
            .with_accounts(accounts)
            .with_subsidy_policy(SubsidyPolicy::new(10_000, 100_000).unwrap());

        // No deposit anywhere, but the subsidy bucket is the payer.
        let attestation = escrow.hold(Uuid::new_v4(), &buyer, 5_000).await.unwrap();
        assert_eq!(attestation.funding_source, FundingSource::Bootstrap);
        assert_eq!(escrow.organic_charged(&buyer.pubkey_base58()), 0);
    }

    #[tokio::test]
    async fn bootstrap_holds_are_refused_outright_without_a_subsidy_policy() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let buyer = LocalIdentity::generate("buyer@local").agent_id();
        let escrow = CustodialEscrow::new(coordinator, FundingSource::Bootstrap);

        assert!(matches!(
            escrow.hold(Uuid::new_v4(), &buyer, 1).await,
            Err(EscrowError::SubsidyExhausted {
                spent_micro_usdc: 0,
                ceiling_micro_usdc: 0,
            })
        ));
        let status = escrow.subsidy_status();
        assert!(!status.enforced);
        assert_eq!(status.remaining_micro_usdc, 0);
    }

    #[tokio::test]
    async fn the_subsidy_floor_bounds_cold_start_exactly() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let buyer = LocalIdentity::generate("buyer@local").agent_id();
        // 50% ratio, 1_000 floor, zero organic revenue on the books.
        let escrow = CustodialEscrow::new(coordinator, FundingSource::Bootstrap)
            .with_subsidy_policy(SubsidyPolicy::new(5_000, 1_000).unwrap());

        escrow.hold(Uuid::new_v4(), &buyer, 1_000).await.unwrap();
        assert!(matches!(
            escrow.hold(Uuid::new_v4(), &buyer, 1).await,
            Err(EscrowError::SubsidyExhausted {
                spent_micro_usdc: 1_000,
                ceiling_micro_usdc: 1_000,
            })
        ));
    }

    #[tokio::test]
    async fn organic_revenue_on_the_books_raises_the_subsidy_ceiling_by_the_ratio() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let buyer = LocalIdentity::generate("buyer@local").agent_id();
        // Books with 10_000 of settled organic revenue — what a
        // deployment that ran organic and later switched its default
        // funding source restarts with.
        let mut holds = HashMap::new();
        holds.insert(
            Uuid::new_v4(),
            EscrowHoldState {
                amount_micro_usdc: 10_000,
                funding_source: FundingSource::Organic,
                status: EscrowStatus::Released,
                buyer_pubkey_b58: "paying-buyer".into(),
            },
        );
        // A held (not yet settled) organic hold must NOT count as
        // revenue, and a refunded one never does.
        holds.insert(
            Uuid::new_v4(),
            EscrowHoldState {
                amount_micro_usdc: 7_000,
                funding_source: FundingSource::Organic,
                status: EscrowStatus::Held,
                buyer_pubkey_b58: "paying-buyer".into(),
            },
        );
        let dir = tempfile::tempdir().unwrap();
        let journal = Arc::new(Journal::open(&dir.path().join("journal.jsonl")).unwrap());
        let escrow =
            CustodialEscrow::restore(coordinator, FundingSource::Bootstrap, holds, journal)
                .with_subsidy_policy(SubsidyPolicy::new(5_000, 1_000).unwrap());

        // Ceiling = 1_000 floor + 50% of 10_000 released = 6_000.
        let status = escrow.subsidy_status();
        assert_eq!(status.organic_released_micro_usdc, 10_000);
        assert_eq!(status.ceiling_micro_usdc, 6_000);

        escrow.hold(Uuid::new_v4(), &buyer, 6_000).await.unwrap();
        assert!(matches!(
            escrow.hold(Uuid::new_v4(), &buyer, 1).await,
            Err(EscrowError::SubsidyExhausted {
                spent_micro_usdc: 6_000,
                ceiling_micro_usdc: 6_000,
            })
        ));
    }

    #[tokio::test]
    async fn a_refunded_bootstrap_hold_frees_its_subsidy_budget() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let buyer = LocalIdentity::generate("buyer@local").agent_id();
        let escrow = CustodialEscrow::new(coordinator, FundingSource::Bootstrap)
            .with_subsidy_policy(SubsidyPolicy::new(0, 1_000).unwrap());

        let first = Uuid::new_v4();
        escrow.hold(first, &buyer, 1_000).await.unwrap();
        assert!(matches!(
            escrow.hold(Uuid::new_v4(), &buyer, 1_000).await,
            Err(EscrowError::SubsidyExhausted { .. })
        ));

        escrow
            .refund(first, RefundReason::DeadlineExpired)
            .await
            .unwrap();
        assert_eq!(escrow.subsidy_status().remaining_micro_usdc, 1_000);
        escrow.hold(Uuid::new_v4(), &buyer, 1_000).await.unwrap();
    }

    #[tokio::test]
    async fn a_runtime_close_kills_the_armed_policy_for_bootstrap_only() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let buyer = LocalIdentity::generate("buyer@local").agent_id();
        let escrow = CustodialEscrow::new(coordinator, FundingSource::Bootstrap)
            .with_subsidy_policy(SubsidyPolicy::new(10_000, 100_000).unwrap());

        escrow.hold(Uuid::new_v4(), &buyer, 5_000).await.unwrap();
        assert!(escrow.close_subsidy().unwrap(), "the first close closes");
        assert!(
            !escrow.close_subsidy().unwrap(),
            "an honest retry reports nothing left to do"
        );

        // The armed policy is dead: the ceiling collapses to zero and
        // the next bootstrap hold refuses, spend-so-far named.
        assert!(matches!(
            escrow.hold(Uuid::new_v4(), &buyer, 1).await,
            Err(EscrowError::SubsidyExhausted {
                spent_micro_usdc: 5_000,
                ceiling_micro_usdc: 0,
            })
        ));
        let status = escrow.subsidy_status();
        assert!(!status.enforced);
        assert!(status.closed);
        assert_eq!(status.max_ratio_bps, 0);
        assert_eq!(status.ceiling_micro_usdc, 0);
        assert_eq!(status.remaining_micro_usdc, 0);
        // The committed books survive the close — the latch stops new
        // spend, it does not rewrite history.
        assert_eq!(status.bootstrap_committed_micro_usdc, 5_000);

        // Organic money is not the subsidy's to gate.
        escrow
            .hold_with_source(Uuid::new_v4(), &buyer, 700, FundingSource::Organic)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_journal_backed_close_is_durable_before_it_latches() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let buyer = LocalIdentity::generate("buyer@local").agent_id();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let journal = Arc::new(Journal::open(&path).unwrap());
        let escrow = CustodialEscrow::restore(
            coordinator,
            FundingSource::Bootstrap,
            HashMap::new(),
            journal,
        )
        .with_subsidy_policy(SubsidyPolicy::new(10_000, 100_000).unwrap());

        assert!(escrow.close_subsidy().unwrap());
        assert!(
            Journal::load(&path).unwrap().subsidy_closed_at_ms.is_some(),
            "the close is on disk, not just in memory"
        );

        // The boot path: a replayed close pins the latch shut even
        // though the environment re-supplied a policy.
        let rearmed = CustodialEscrow::restore(
            LocalIdentity::generate("coordinator@local"),
            FundingSource::Bootstrap,
            HashMap::new(),
            Arc::new(Journal::open(&path).unwrap()),
        )
        .with_subsidy_policy(SubsidyPolicy::new(10_000, 100_000).unwrap())
        .with_subsidy_closed(true);
        assert!(matches!(
            rearmed.hold(Uuid::new_v4(), &buyer, 1).await,
            Err(EscrowError::SubsidyExhausted { .. })
        ));
        assert!(rearmed.subsidy_status().closed);
    }

    #[tokio::test]
    async fn money_totals_sum_holds_by_source_and_status() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let buyer = LocalIdentity::generate("buyer@local").agent_id();
        let operator = LocalIdentity::generate("operator@local");
        let escrow = CustodialEscrow::new(coordinator, FundingSource::Organic)
            .with_subsidy_policy(SubsidyPolicy::new(10_000, 100_000).unwrap());

        // Organic: one still held, one released, one refunded.
        escrow.hold(Uuid::new_v4(), &buyer, 700).await.unwrap();
        let released = Uuid::new_v4();
        escrow.hold(released, &buyer, 300).await.unwrap();
        escrow
            .release(released, &receipt_for(released, &operator, 300))
            .await
            .unwrap();
        let refunded = Uuid::new_v4();
        escrow.hold(refunded, &buyer, 200).await.unwrap();
        escrow
            .refund(refunded, RefundReason::DeadlineExpired)
            .await
            .unwrap();
        // Bootstrap: one held, one refunded — never counted as organic.
        escrow
            .hold_with_source(Uuid::new_v4(), &buyer, 50, FundingSource::Bootstrap)
            .await
            .unwrap();
        let probe = Uuid::new_v4();
        escrow
            .hold_with_source(probe, &buyer, 40, FundingSource::Bootstrap)
            .await
            .unwrap();
        escrow
            .refund(probe, RefundReason::ExecutionFailed)
            .await
            .unwrap();

        assert_eq!(
            escrow.money_totals(FundingSource::Organic),
            EscrowMoneyTotals {
                held_micro_usdc: 700,
                released_micro_usdc: 300,
                refunded_micro_usdc: 200,
            }
        );
        assert_eq!(
            escrow.money_totals(FundingSource::Bootstrap),
            EscrowMoneyTotals {
                held_micro_usdc: 50,
                released_micro_usdc: 0,
                refunded_micro_usdc: 40,
            }
        );
    }

    #[test]
    fn a_subsidy_ratio_above_one_is_unrepresentable() {
        assert!(SubsidyPolicy::new(10_001, 0).is_err());
        assert!(SubsidyPolicy::new(10_000, 0).is_ok());
    }

    #[tokio::test]
    async fn release_or_refund_or_status_on_unknown_job_is_not_found() {
        let coordinator = LocalIdentity::generate("coordinator@local");
        let operator = LocalIdentity::generate("operator@local");
        let escrow = CustodialEscrow::new(coordinator, FundingSource::Organic);
        let job_id = Uuid::new_v4();

        assert!(matches!(
            escrow.status(job_id).await,
            Err(EscrowError::NotFound(id)) if id == job_id
        ));
        assert!(matches!(
            escrow.refund(job_id, RefundReason::AdmissionFailed).await,
            Err(EscrowError::NotFound(id)) if id == job_id
        ));
        let receipt = receipt_for(job_id, &operator, 100);
        assert!(matches!(
            escrow.release(job_id, &receipt).await,
            Err(EscrowError::NotFound(id)) if id == job_id
        ));
    }
}
