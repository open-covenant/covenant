//! The coordinator's money ledgers outside escrow: the buyer deposit
//! book, the buyer withdrawal book, and the partner-payout book, all
//! journal-backed, monotonic, and idempotent by an external id.
//!
//! [`BuyerAccounts`] records what each buyer has paid in (A3 — "a
//! buyer funds once and goes"). It holds only the *inflow* side:
//! monotonically growing per-buyer deposit totals plus the seen
//! deposit-id set that makes crediting idempotent. The *outflow* side is deliberately not tracked here —
//! a buyer's available balance is derived as deposits minus the
//! escrow's non-refunded organic holds (see
//! [`crate::escrow::CustodialEscrow::organic_charged`]), so every fund
//! transition remains a single atomic journal line and there is no
//! two-entry crash window between "hold settled" and "balance
//! adjusted".
//!
//! Verifying that a claimed deposit really happened is the
//! [`crate::deposit::InboundRail`]'s job; by the time an amount reaches
//! [`BuyerAccounts::credit_deposit`] it is trusted.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::journal::{Journal, JournalError};

/// What a deposit claim credited: `Credited` carries the buyer's new
/// deposited total; `Duplicate` means the deposit id was already
/// applied (an honest retry or a replayed claim) and nothing moved.
#[derive(Debug, PartialEq, Eq)]
pub enum DepositOutcome {
    Credited { deposited_total_micro_usdc: u64 },
    Duplicate { deposited_total_micro_usdc: u64 },
}

#[derive(Default)]
struct Ledger {
    totals: HashMap<String, u64>,
    seen: HashSet<String>,
}

/// In-memory unless built with [`BuyerAccounts::restore`], in which
/// case every credit journals its deposit event before it commits.
#[derive(Default)]
pub struct BuyerAccounts {
    ledger: Mutex<Ledger>,
    journal: Option<Arc<Journal>>,
}

impl BuyerAccounts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seeds the ledger with journal-recovered deposits and journals
    /// every credit from here on.
    pub fn restore(
        totals: HashMap<String, u64>,
        seen: HashSet<String>,
        journal: Arc<Journal>,
    ) -> Self {
        Self {
            ledger: Mutex::new(Ledger { totals, seen }),
            journal: Some(journal),
        }
    }

    /// Credits a rail-verified deposit, exactly once per `deposit_id`.
    /// Journal-then-commit: a credit that can't be made durable fails
    /// without moving the in-memory total.
    pub fn credit_deposit(
        &self,
        deposit_id: &str,
        buyer_pubkey_b58: &str,
        amount_micro_usdc: u64,
    ) -> Result<DepositOutcome, JournalError> {
        let mut ledger = self.ledger.lock();
        if ledger.seen.contains(deposit_id) {
            return Ok(DepositOutcome::Duplicate {
                deposited_total_micro_usdc: ledger
                    .totals
                    .get(buyer_pubkey_b58)
                    .copied()
                    .unwrap_or(0),
            });
        }
        if let Some(journal) = &self.journal {
            journal.record_deposit(deposit_id, buyer_pubkey_b58, amount_micro_usdc)?;
        }
        ledger.seen.insert(deposit_id.into());
        let total = ledger.totals.entry(buyer_pubkey_b58.into()).or_default();
        *total = total.saturating_add(amount_micro_usdc);
        Ok(DepositOutcome::Credited {
            deposited_total_micro_usdc: *total,
        })
    }

    /// Everything this buyer has ever deposited. Monotonic — spend
    /// never subtracts from it, so a concurrent reader can only see a
    /// value at most briefly *lower* than true, never higher, which
    /// keeps the escrow's funds check conservative.
    pub fn deposited(&self, buyer_pubkey_b58: &str) -> u64 {
        self.ledger
            .lock()
            .totals
            .get(buyer_pubkey_b58)
            .copied()
            .unwrap_or(0)
    }

    /// Everything every buyer has ever deposited — the inflow side of
    /// the whole-system conservation law `/metrics` exposes.
    pub fn total_deposited(&self) -> u64 {
        self.ledger.lock().totals.values().sum()
    }
}

/// The push half of a [`WithdrawalState`]: set once a backend transfer
/// honored the debit. `tx_signature` is `None` on the mock backend
/// (nothing was submitted), `Some` for a real on-chain transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WithdrawalPush {
    pub tx_signature: Option<String>,
    pub recorded_at_ms: u64,
}

/// One withdrawal obligation, journaled whole on every transition:
/// debited from the buyer's available balance at request time, then
/// honored by a backend transfer whose outcome lands back on the
/// record. `pushed: None` means debited but not yet honored — the
/// crash window the retry sweep re-pushes. A debit never returns to
/// the available balance; the money is owed to `recipient_address_b58`
/// from the moment it is journaled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WithdrawalState {
    pub withdrawal_id: Uuid,
    pub buyer_pubkey_b58: String,
    pub recipient_address_b58: String,
    pub amount_micro_usdc: u64,
    pub requested_at_ms: u64,
    #[serde(default)]
    pub pushed: Option<WithdrawalPush>,
}

/// What a withdrawal debit did. `Requested` locked the amount away
/// from holds; `Duplicate` means this `withdrawal_id` was already
/// debited (an honest retry) and nothing moved; `Insufficient` refused
/// outright — a withdrawal may never overdraw deposits minus charges
/// minus prior withdrawals.
#[derive(Debug, PartialEq, Eq)]
pub enum WithdrawalDebit {
    Requested(WithdrawalState),
    Duplicate(WithdrawalState),
    Insufficient {
        available_micro_usdc: u64,
    },
    /// The withdrawal id is already held by a *different* buyer. Dedup is
    /// per (buyer, id), so this is an id collision, never a retry: returning
    /// the existing record would disclose another buyer's recipient, amount,
    /// and push — and could trigger its transfer. Refused without disclosing
    /// anything about it.
    IdHeldByAnotherBuyer,
}

/// The withdrawal book. In-memory unless built with
/// [`BuyerWithdrawals::restore`], in which case every transition
/// journals before it commits. The funds arbitration lives in
/// [`crate::escrow::CustodialEscrow::withdraw`], which calls
/// [`BuyerWithdrawals::debit`] under the escrow's holds lock so a
/// concurrent hold and withdrawal can never both spend the same
/// deposit.
#[derive(Default)]
pub struct BuyerWithdrawals {
    ledger: Mutex<HashMap<Uuid, WithdrawalState>>,
    journal: Option<Arc<Journal>>,
}

impl BuyerWithdrawals {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seeds the book with journal-recovered withdrawals and journals
    /// every transition from here on.
    pub fn restore(withdrawals: HashMap<Uuid, WithdrawalState>, journal: Arc<Journal>) -> Self {
        Self {
            ledger: Mutex::new(withdrawals),
            journal: Some(journal),
        }
    }

    /// Debits `state` against `available_micro_usdc` — the buyer's
    /// deposits minus non-refunded organic charges, computed by the
    /// caller — minus everything this book already debited for the
    /// buyer. Exactly once per `withdrawal_id`; journal-then-commit.
    pub(crate) fn debit(
        &self,
        state: WithdrawalState,
        available_micro_usdc: u64,
    ) -> Result<WithdrawalDebit, JournalError> {
        let mut ledger = self.ledger.lock();
        if let Some(existing) = ledger.get(&state.withdrawal_id) {
            if existing.buyer_pubkey_b58 != state.buyer_pubkey_b58 {
                return Ok(WithdrawalDebit::IdHeldByAnotherBuyer);
            }
            return Ok(WithdrawalDebit::Duplicate(existing.clone()));
        }
        let withdrawn = Self::withdrawn_of(&ledger, &state.buyer_pubkey_b58);
        let available = available_micro_usdc.saturating_sub(withdrawn);
        if state.amount_micro_usdc > available {
            return Ok(WithdrawalDebit::Insufficient {
                available_micro_usdc: available,
            });
        }
        if let Some(journal) = &self.journal {
            journal.record_withdrawal(&state)?;
        }
        ledger.insert(state.withdrawal_id, state.clone());
        Ok(WithdrawalDebit::Requested(state))
    }

    /// Pins a completed backend transfer onto the debit. Idempotent —
    /// an already-pushed record is returned unchanged. `Ok(None)`
    /// means no such debit exists (a push for a withdrawal the books
    /// never saw is a bug at the call site, not a fact to invent).
    pub fn record_pushed(
        &self,
        withdrawal_id: Uuid,
        push: WithdrawalPush,
    ) -> Result<Option<WithdrawalState>, JournalError> {
        let mut ledger = self.ledger.lock();
        let Some(existing) = ledger.get_mut(&withdrawal_id) else {
            return Ok(None);
        };
        if existing.pushed.is_some() {
            return Ok(Some(existing.clone()));
        }
        let updated = WithdrawalState {
            pushed: Some(push),
            ..existing.clone()
        };
        if let Some(journal) = &self.journal {
            journal.record_withdrawal(&updated)?;
        }
        *existing = updated.clone();
        Ok(Some(updated))
    }

    /// Everything ever debited for this buyer, pushed or not — a debit
    /// is owed to its recipient from the moment it commits, so it
    /// never counts as available again.
    pub fn withdrawn(&self, buyer_pubkey_b58: &str) -> u64 {
        Self::withdrawn_of(&self.ledger.lock(), buyer_pubkey_b58)
    }

    /// Everything ever debited across all buyers, pushed or not — the
    /// outflow side of the conservation law `/metrics` exposes.
    pub fn total_withdrawn(&self) -> u64 {
        self.ledger
            .lock()
            .values()
            .map(|w| w.amount_micro_usdc)
            .sum()
    }

    fn withdrawn_of(ledger: &HashMap<Uuid, WithdrawalState>, buyer_pubkey_b58: &str) -> u64 {
        ledger
            .values()
            .filter(|w| w.buyer_pubkey_b58 == buyer_pubkey_b58)
            .map(|w| w.amount_micro_usdc)
            .sum()
    }

    pub fn get(&self, withdrawal_id: Uuid) -> Option<WithdrawalState> {
        self.ledger.lock().get(&withdrawal_id).cloned()
    }

    /// This buyer's withdrawals, newest first.
    pub fn for_buyer(&self, buyer_pubkey_b58: &str) -> Vec<WithdrawalState> {
        let mut rows: Vec<WithdrawalState> = self
            .ledger
            .lock()
            .values()
            .filter(|w| w.buyer_pubkey_b58 == buyer_pubkey_b58)
            .cloned()
            .collect();
        rows.sort_by(|a, b| {
            b.requested_at_ms
                .cmp(&a.requested_at_ms)
                .then(a.withdrawal_id.cmp(&b.withdrawal_id))
        });
        rows
    }

    /// Debits no backend transfer ever honored — the retry sweep's
    /// worklist after a crash between debit and push.
    pub fn unpushed(&self) -> Vec<WithdrawalState> {
        self.ledger
            .lock()
            .values()
            .filter(|w| w.pushed.is_none())
            .cloned()
            .collect()
    }
}

/// What recording a partner payout did. `Recorded` moved the paid
/// total; `Duplicate` means this `payout_id` was already applied (an
/// honest retry) and nothing moved; `ExceedsAccrued` refused outright —
/// the books may never claim more was paid out than ever accrued.
#[derive(Debug, PartialEq, Eq)]
pub enum PartnerPayoutOutcome {
    Recorded { paid_total_micro_usdc: u64 },
    Duplicate { paid_total_micro_usdc: u64 },
    ExceedsAccrued { paid_micro_usdc: u64 },
}

/// The outflow side of the rev-share books (C8): per referral code,
/// the cumulative amount an operator has recorded as actually paid
/// out — the high-water mark against the accruals derived from job
/// records. The coordinator moves no money here; payment happens with
/// the operator's own wallet tooling and gets *recorded*, idempotent
/// by the operator-supplied reference. Same journal-then-commit
/// posture as [`BuyerAccounts`].
#[derive(Default)]
pub struct PartnerPayouts {
    ledger: Mutex<Ledger>,
    journal: Option<Arc<Journal>>,
}

impl PartnerPayouts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seeds the ledger with journal-recovered payouts and journals
    /// every new record from here on.
    pub fn restore(
        totals: HashMap<String, u64>,
        seen: HashSet<String>,
        journal: Arc<Journal>,
    ) -> Self {
        Self {
            ledger: Mutex::new(Ledger { totals, seen }),
            journal: Some(journal),
        }
    }

    /// Records one payout against `referral_code`, exactly once per
    /// `payout_id`, refusing any record that would push the code's paid
    /// total past `accrued_micro_usdc`. The accrual number is read
    /// outside this ledger and only ever grows, so a stale read can
    /// only make this check more conservative, never overdraw.
    pub fn mark_paid(
        &self,
        payout_id: &str,
        referral_code: &str,
        amount_micro_usdc: u64,
        accrued_micro_usdc: u64,
    ) -> Result<PartnerPayoutOutcome, JournalError> {
        let mut ledger = self.ledger.lock();
        let paid = ledger.totals.get(referral_code).copied().unwrap_or(0);
        if ledger.seen.contains(payout_id) {
            return Ok(PartnerPayoutOutcome::Duplicate {
                paid_total_micro_usdc: paid,
            });
        }
        if paid.saturating_add(amount_micro_usdc) > accrued_micro_usdc {
            return Ok(PartnerPayoutOutcome::ExceedsAccrued {
                paid_micro_usdc: paid,
            });
        }
        if let Some(journal) = &self.journal {
            journal.record_partner_payout(payout_id, referral_code, amount_micro_usdc)?;
        }
        ledger.seen.insert(payout_id.into());
        let total = ledger.totals.entry(referral_code.into()).or_default();
        *total = total.saturating_add(amount_micro_usdc);
        Ok(PartnerPayoutOutcome::Recorded {
            paid_total_micro_usdc: *total,
        })
    }

    pub fn paid(&self, referral_code: &str) -> u64 {
        self.ledger
            .lock()
            .totals
            .get(referral_code)
            .copied()
            .unwrap_or(0)
    }

    /// Everything ever recorded as paid out across all partners —
    /// bounded by the accrual total by construction, and exposed next
    /// to it on `/metrics`.
    pub fn total_paid(&self) -> u64 {
        self.ledger.lock().totals.values().sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credits_once_per_deposit_id() {
        let accounts = BuyerAccounts::new();
        assert_eq!(
            accounts.credit_deposit("sig-1", "buyer-a", 5_000).unwrap(),
            DepositOutcome::Credited {
                deposited_total_micro_usdc: 5_000
            }
        );
        assert_eq!(
            accounts.credit_deposit("sig-1", "buyer-a", 5_000).unwrap(),
            DepositOutcome::Duplicate {
                deposited_total_micro_usdc: 5_000
            }
        );
        assert_eq!(
            accounts.credit_deposit("sig-2", "buyer-a", 1_000).unwrap(),
            DepositOutcome::Credited {
                deposited_total_micro_usdc: 6_000
            }
        );
        assert_eq!(accounts.deposited("buyer-a"), 6_000);
        assert_eq!(accounts.deposited("buyer-unknown"), 0);
    }

    #[test]
    fn a_running_total_saturates_instead_of_wrapping() {
        // The deposited total is documented monotonic; a pathological
        // cumulative sum past u64::MAX must cap there, never wrap back to
        // a small value that would read as the buyer's funds vanishing.
        let accounts = BuyerAccounts::new();
        accounts
            .credit_deposit("sig-1", "buyer-a", u64::MAX - 1)
            .unwrap();
        assert_eq!(
            accounts.credit_deposit("sig-2", "buyer-a", 100).unwrap(),
            DepositOutcome::Credited {
                deposited_total_micro_usdc: u64::MAX
            }
        );
        assert_eq!(accounts.deposited("buyer-a"), u64::MAX);
    }

    #[test]
    fn restored_accounts_journal_new_credits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");

        {
            let journal = Arc::new(Journal::open(&path).unwrap());
            let accounts = BuyerAccounts::restore(HashMap::new(), HashSet::new(), journal);
            accounts.credit_deposit("sig-1", "buyer-a", 700).unwrap();
        }

        let restored = crate::journal::Journal::load(&path).unwrap();
        let journal = Arc::new(Journal::open(&path).unwrap());
        let accounts =
            BuyerAccounts::restore(restored.deposit_totals, restored.deposit_ids, journal);
        assert_eq!(accounts.deposited("buyer-a"), 700);
        // The replayed id still dedups.
        assert_eq!(
            accounts.credit_deposit("sig-1", "buyer-a", 700).unwrap(),
            DepositOutcome::Duplicate {
                deposited_total_micro_usdc: 700
            }
        );
    }

    #[test]
    fn partner_payouts_record_once_and_never_exceed_accruals() {
        let payouts = PartnerPayouts::new();
        // 800 accrued: a 500 record fits...
        assert_eq!(
            payouts.mark_paid("tx-1", "partner-a", 500, 800).unwrap(),
            PartnerPayoutOutcome::Recorded {
                paid_total_micro_usdc: 500
            }
        );
        // ...an honest retry of the same reference moves nothing...
        assert_eq!(
            payouts.mark_paid("tx-1", "partner-a", 500, 800).unwrap(),
            PartnerPayoutOutcome::Duplicate {
                paid_total_micro_usdc: 500
            }
        );
        // ...and a record that would push paid past accrued is refused.
        assert_eq!(
            payouts.mark_paid("tx-2", "partner-a", 301, 800).unwrap(),
            PartnerPayoutOutcome::ExceedsAccrued {
                paid_micro_usdc: 500
            }
        );
        // Exactly draining the remainder is fine.
        assert_eq!(
            payouts.mark_paid("tx-2", "partner-a", 300, 800).unwrap(),
            PartnerPayoutOutcome::Recorded {
                paid_total_micro_usdc: 800
            }
        );
        assert_eq!(payouts.paid("partner-a"), 800);
        assert_eq!(payouts.paid("partner-unknown"), 0);
    }

    fn withdrawal(buyer: &str, amount: u64) -> WithdrawalState {
        WithdrawalState {
            withdrawal_id: Uuid::new_v4(),
            buyer_pubkey_b58: buyer.into(),
            recipient_address_b58: "recipient".into(),
            amount_micro_usdc: amount,
            requested_at_ms: 1,
            pushed: None,
        }
    }

    #[test]
    fn withdrawals_debit_once_and_never_overdraw() {
        let book = BuyerWithdrawals::new();
        let first = withdrawal("buyer-a", 600);

        assert_eq!(
            book.debit(first.clone(), 1_000).unwrap(),
            WithdrawalDebit::Requested(first.clone())
        );
        // An honest retry of the same id moves nothing...
        assert_eq!(
            book.debit(first.clone(), 1_000).unwrap(),
            WithdrawalDebit::Duplicate(first.clone())
        );
        // ...and a second withdrawal sees the first as already gone.
        assert_eq!(
            book.debit(withdrawal("buyer-a", 500), 1_000).unwrap(),
            WithdrawalDebit::Insufficient {
                available_micro_usdc: 400
            }
        );
        // Exactly draining the remainder is fine.
        let drain = withdrawal("buyer-a", 400);
        assert_eq!(
            book.debit(drain.clone(), 1_000).unwrap(),
            WithdrawalDebit::Requested(drain)
        );
        assert_eq!(book.withdrawn("buyer-a"), 1_000);
        assert_eq!(book.withdrawn("buyer-b"), 0);
    }

    #[test]
    fn a_withdrawal_id_held_by_another_buyer_is_refused_without_leaking_it() {
        let book = BuyerWithdrawals::new();
        let a = withdrawal("buyer-a", 100);
        assert_eq!(
            book.debit(a.clone(), 1_000).unwrap(),
            WithdrawalDebit::Requested(a.clone())
        );
        // Buyer B reuses A's id. The book must not hand back A's record (its
        // recipient, amount, push) or debit against it — only refuse.
        let collision = WithdrawalState {
            buyer_pubkey_b58: "buyer-b".into(),
            recipient_address_b58: "attacker".into(),
            amount_micro_usdc: 999,
            ..a.clone()
        };
        assert_eq!(
            book.debit(collision, 1_000).unwrap(),
            WithdrawalDebit::IdHeldByAnotherBuyer
        );
        // A's own retry of the id still dedups to A's own record.
        assert_eq!(
            book.debit(a.clone(), 1_000).unwrap(),
            WithdrawalDebit::Duplicate(a)
        );
    }

    #[test]
    fn a_push_pins_once_and_unknown_ids_are_never_invented() {
        let book = BuyerWithdrawals::new();
        let state = withdrawal("buyer-a", 100);
        book.debit(state.clone(), 100).unwrap();
        assert_eq!(book.unpushed().len(), 1);

        let pushed = book
            .record_pushed(
                state.withdrawal_id,
                WithdrawalPush {
                    tx_signature: Some("sig-1".into()),
                    recorded_at_ms: 2,
                },
            )
            .unwrap()
            .expect("known debit");
        assert_eq!(
            pushed.pushed.as_ref().unwrap().tx_signature.as_deref(),
            Some("sig-1")
        );
        assert!(book.unpushed().is_empty());

        // A second push (the sweep racing the first-chance push) keeps
        // the original fact.
        let again = book
            .record_pushed(
                state.withdrawal_id,
                WithdrawalPush {
                    tx_signature: Some("sig-2".into()),
                    recorded_at_ms: 3,
                },
            )
            .unwrap()
            .expect("still known");
        assert_eq!(
            again.pushed.as_ref().unwrap().tx_signature.as_deref(),
            Some("sig-1")
        );

        assert!(book
            .record_pushed(
                Uuid::new_v4(),
                WithdrawalPush {
                    tx_signature: None,
                    recorded_at_ms: 4,
                },
            )
            .unwrap()
            .is_none());
    }

    #[test]
    fn restored_withdrawals_replay_debits_and_pushes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let (debited, pushed);

        {
            let journal = Arc::new(Journal::open(&path).unwrap());
            let book = BuyerWithdrawals::restore(HashMap::new(), journal);
            debited = withdrawal("buyer-a", 700);
            pushed = withdrawal("buyer-a", 300);
            book.debit(debited.clone(), 10_000).unwrap();
            book.debit(pushed.clone(), 10_000).unwrap();
            book.record_pushed(
                pushed.withdrawal_id,
                WithdrawalPush {
                    tx_signature: Some("sig-x".into()),
                    recorded_at_ms: 9,
                },
            )
            .unwrap();
        }

        let restored = crate::journal::Journal::load(&path).unwrap();
        let journal = Arc::new(Journal::open(&path).unwrap());
        let book = BuyerWithdrawals::restore(restored.withdrawals, journal);
        assert_eq!(book.withdrawn("buyer-a"), 1_000);
        // The unpushed debit is still on the worklist; the pushed one
        // kept its transaction.
        let unpushed = book.unpushed();
        assert_eq!(unpushed.len(), 1);
        assert_eq!(unpushed[0].withdrawal_id, debited.withdrawal_id);
        assert_eq!(
            book.get(pushed.withdrawal_id)
                .unwrap()
                .pushed
                .unwrap()
                .tx_signature
                .as_deref(),
            Some("sig-x")
        );
        // The replayed id still dedups.
        assert_eq!(
            book.debit(debited.clone(), 10_000).unwrap(),
            WithdrawalDebit::Duplicate(debited)
        );
    }

    #[test]
    fn buyer_withdrawal_views_sort_newest_first() {
        let book = BuyerWithdrawals::new();
        let mut old = withdrawal("buyer-a", 10);
        old.requested_at_ms = 100;
        let mut new = withdrawal("buyer-a", 20);
        new.requested_at_ms = 200;
        book.debit(old.clone(), 1_000).unwrap();
        book.debit(new.clone(), 1_000).unwrap();
        book.debit(withdrawal("buyer-b", 5), 1_000).unwrap();

        let rows = book.for_buyer("buyer-a");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].withdrawal_id, new.withdrawal_id);
        assert_eq!(rows[1].withdrawal_id, old.withdrawal_id);
    }

    #[test]
    fn book_totals_sum_across_owners() {
        let accounts = BuyerAccounts::new();
        accounts.credit_deposit("sig-1", "buyer-a", 5_000).unwrap();
        accounts.credit_deposit("sig-2", "buyer-b", 1_000).unwrap();
        assert_eq!(accounts.total_deposited(), 6_000);

        let book = BuyerWithdrawals::new();
        book.debit(withdrawal("buyer-a", 600), 5_000).unwrap();
        book.debit(withdrawal("buyer-b", 400), 1_000).unwrap();
        assert_eq!(book.total_withdrawn(), 1_000);

        let payouts = PartnerPayouts::new();
        payouts.mark_paid("tx-1", "partner-a", 500, 800).unwrap();
        payouts.mark_paid("tx-2", "partner-b", 250, 300).unwrap();
        assert_eq!(payouts.total_paid(), 750);
    }

    #[test]
    fn restored_partner_payouts_replay_and_still_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");

        {
            let journal = Arc::new(Journal::open(&path).unwrap());
            let payouts = PartnerPayouts::restore(HashMap::new(), HashSet::new(), journal);
            payouts.mark_paid("tx-1", "partner-a", 250, 1_000).unwrap();
        }

        let restored = crate::journal::Journal::load(&path).unwrap();
        let journal = Arc::new(Journal::open(&path).unwrap());
        let payouts = PartnerPayouts::restore(
            restored.partner_paid_totals,
            restored.partner_payout_ids,
            journal,
        );
        assert_eq!(payouts.paid("partner-a"), 250);
        assert_eq!(
            payouts.mark_paid("tx-1", "partner-a", 250, 1_000).unwrap(),
            PartnerPayoutOutcome::Duplicate {
                paid_total_micro_usdc: 250
            }
        );
    }
}
