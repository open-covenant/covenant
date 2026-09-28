//! The operator bond book (C5 phase 2): rail-verified stake in,
//! coordinator-proven slashes, and matured unbond refunds out — the
//! enforcement backbone reputation, redundancy and disputes each name
//! as their missing piece. Same journal-then-commit, idempotent-by-id
//! posture as [`crate::accounts`], and the same derived-balance rule:
//! every fund transition is a single journal line.
//!
//! The one bond-specific wrinkle is the unbonding window. A requested
//! unbond stays slashable until its transfer actually leaves — an
//! operator must not outrun a fault by unbonding — so a request
//! reserves nothing. Instead the refund clamps at push time: what a
//! matured unbond pays is its requested amount capped by the stake
//! still standing, so a slash that landed while the request matured
//! shrinks (or zeroes) the refund, never the other way around.
//!
//! Three per-operator quantities fall out, and each consumer reads a
//! different one: `at_stake` (posted minus slashed minus refunded) is
//! what a slash can still take; `committed` (at_stake minus pending
//! unbond requests) is what the matcher may floor on and what a new
//! unbond may draw from; `posted` is monotonic history.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::journal::{Journal, JournalError};

/// One coordinator-proven slash, journaled whole. `slash_id` is
/// deterministic at the call site (fault kind + job + operator), so a
/// verdict replayed across restarts slashes exactly once. The evidence
/// is the audit row the fault site already writes, findable by
/// `job_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashRecord {
    pub slash_id: String,
    pub operator_pubkey_b58: String,
    /// What was actually taken — the requested amount capped by the
    /// stake that was standing when the fault landed.
    pub amount_micro_usdc: u64,
    pub job_id: Uuid,
    pub reason: String,
    pub slashed_at_ms: u64,
}

/// The push half of an [`UnbondState`]: set once a backend transfer
/// honored the matured request. `paid_micro_usdc` is what actually
/// left, clamped at push time — a slash landing during maturation
/// shrinks the refund, down to a zero-payment push that closes the
/// request without a transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BondRefundPush {
    pub tx_signature: Option<String>,
    pub recorded_at_ms: u64,
    pub paid_micro_usdc: u64,
}

/// One unbond obligation, journaled whole on every transition:
/// requested against the operator's committed stake, matured after the
/// unbonding window, then honored by a transfer whose outcome lands
/// back on the record. `pushed: None` past `matures_at_ms` is the
/// retry sweep's worklist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnbondState {
    pub unbond_id: Uuid,
    pub operator_pubkey_b58: String,
    pub recipient_address_b58: String,
    pub amount_micro_usdc: u64,
    pub requested_at_ms: u64,
    pub matures_at_ms: u64,
    #[serde(default)]
    pub pushed: Option<BondRefundPush>,
}

/// Every view of one operator's stake, each field the number one
/// consumer needs (see the module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BondStatus {
    pub posted_micro_usdc: u64,
    pub slashed_micro_usdc: u64,
    /// Requested but not yet pushed — still slashable, no longer
    /// committable.
    pub unbonding_micro_usdc: u64,
    /// Actually paid back out by matured refunds.
    pub refunded_micro_usdc: u64,
    /// posted − slashed − refunded: what a fault can still take.
    pub at_stake_micro_usdc: u64,
    /// at_stake − unbonding: what the matcher floors on and what a new
    /// unbond may draw from.
    pub committed_micro_usdc: u64,
}

/// What crediting a rail-verified bond post did.
#[derive(Debug, PartialEq, Eq)]
pub enum BondPostOutcome {
    Credited { posted_total_micro_usdc: u64 },
    Duplicate { posted_total_micro_usdc: u64 },
}

/// What a slash did. `Slashed.amount_micro_usdc` is the clamped take;
/// `NoStake` means nothing was standing to take (recorded nowhere —
/// there is no fact to journal); `Duplicate` means this `slash_id`
/// already landed.
#[derive(Debug, PartialEq, Eq)]
pub enum SlashOutcome {
    Slashed {
        amount_micro_usdc: u64,
        at_stake_micro_usdc: u64,
    },
    Duplicate,
    NoStake,
}

/// What an unbond request did. `Insufficient` reports the committed
/// stake the request could have drawn from.
#[derive(Debug, PartialEq, Eq)]
pub enum UnbondOutcome {
    Requested(UnbondState),
    Duplicate(UnbondState),
    Insufficient { committed_micro_usdc: u64 },
}

#[derive(Default)]
struct Ledger {
    posted: HashMap<String, u64>,
    seen_posts: HashSet<String>,
    slashes: Vec<SlashRecord>,
    seen_slashes: HashSet<String>,
    unbonds: HashMap<Uuid, UnbondState>,
    /// Refunds whose transfer is in flight right now: unbond_id →
    /// (operator, amount). A slash landing while the sidecar holds the
    /// money mid-air must not draw the same stake the refund is already
    /// spending — the reserved amount is fenced off from
    /// [`OperatorBonds::slash`]'s clamp until the push books or fails.
    reserved: HashMap<Uuid, (String, u64)>,
}

impl Ledger {
    fn reserved_for(&self, operator: &str) -> u64 {
        self.reserved
            .values()
            .filter(|(op, _)| op == operator)
            .map(|(_, amount)| amount)
            .sum()
    }
}

impl Ledger {
    fn status(&self, operator: &str) -> BondStatus {
        let posted = self.posted.get(operator).copied().unwrap_or(0);
        let slashed = self
            .slashes
            .iter()
            .filter(|s| s.operator_pubkey_b58 == operator)
            .map(|s| s.amount_micro_usdc)
            .sum::<u64>();
        let (unbonding, refunded) = self
            .unbonds
            .values()
            .filter(|u| u.operator_pubkey_b58 == operator)
            .fold((0u64, 0u64), |(pending, paid), u| match &u.pushed {
                None => (pending + u.amount_micro_usdc, paid),
                Some(push) => (pending, paid + push.paid_micro_usdc),
            });
        let at_stake = posted.saturating_sub(slashed).saturating_sub(refunded);
        BondStatus {
            posted_micro_usdc: posted,
            slashed_micro_usdc: slashed,
            unbonding_micro_usdc: unbonding,
            refunded_micro_usdc: refunded,
            at_stake_micro_usdc: at_stake,
            committed_micro_usdc: at_stake.saturating_sub(unbonding),
        }
    }
}

/// In-memory unless built with [`OperatorBonds::restore`], in which
/// case every transition journals before it commits.
#[derive(Default)]
pub struct OperatorBonds {
    ledger: Mutex<Ledger>,
    journal: Option<Arc<Journal>>,
}

impl OperatorBonds {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seeds the book with journal-recovered facts and journals every
    /// transition from here on.
    pub fn restore(
        posted: HashMap<String, u64>,
        seen_posts: HashSet<String>,
        slashes: Vec<SlashRecord>,
        unbonds: HashMap<Uuid, UnbondState>,
        journal: Arc<Journal>,
    ) -> Self {
        let seen_slashes = slashes.iter().map(|s| s.slash_id.clone()).collect();
        Self {
            ledger: Mutex::new(Ledger {
                posted,
                seen_posts,
                slashes,
                seen_slashes,
                unbonds,
                reserved: HashMap::new(),
            }),
            journal: Some(journal),
        }
    }

    /// Credits a rail-verified bond post, exactly once per `bond_id`.
    /// Journal-then-commit, mirroring
    /// [`crate::accounts::BuyerAccounts::credit_deposit`].
    pub fn credit_post(
        &self,
        bond_id: &str,
        operator_pubkey_b58: &str,
        amount_micro_usdc: u64,
    ) -> Result<BondPostOutcome, JournalError> {
        let mut ledger = self.ledger.lock();
        if ledger.seen_posts.contains(bond_id) {
            return Ok(BondPostOutcome::Duplicate {
                posted_total_micro_usdc: ledger
                    .posted
                    .get(operator_pubkey_b58)
                    .copied()
                    .unwrap_or(0),
            });
        }
        if let Some(journal) = &self.journal {
            journal.record_bond_post(bond_id, operator_pubkey_b58, amount_micro_usdc)?;
        }
        ledger.seen_posts.insert(bond_id.into());
        let total = ledger.posted.entry(operator_pubkey_b58.into()).or_default();
        *total = total.saturating_add(amount_micro_usdc);
        Ok(BondPostOutcome::Credited {
            posted_total_micro_usdc: *total,
        })
    }

    /// Takes up to `amount_micro_usdc` from the operator's standing
    /// stake, exactly once per `slash_id`. Pending unbonds do not
    /// shield — the take clamps only at what is still at stake. Only a
    /// coordinator-proven fault may reach this (canary wrong-answers
    /// and redundancy minorities); a buyer dispute never does — an
    /// unadjudicated accusation must not move money.
    pub fn slash(
        &self,
        slash_id: &str,
        operator_pubkey_b58: &str,
        amount_micro_usdc: u64,
        job_id: Uuid,
        reason: &str,
        now_ms: u64,
    ) -> Result<SlashOutcome, JournalError> {
        let mut ledger = self.ledger.lock();
        if ledger.seen_slashes.contains(slash_id) {
            return Ok(SlashOutcome::Duplicate);
        }
        // Stake a refund transfer is spending right now is not
        // available to a fault: without this fence a slash adjudicated
        // in the transfer window reads the full stake, and the same
        // principal pays both the refund and the slash.
        let at_stake = ledger
            .status(operator_pubkey_b58)
            .at_stake_micro_usdc
            .saturating_sub(ledger.reserved_for(operator_pubkey_b58));
        let take = amount_micro_usdc.min(at_stake);
        if take == 0 {
            return Ok(SlashOutcome::NoStake);
        }
        let record = SlashRecord {
            slash_id: slash_id.into(),
            operator_pubkey_b58: operator_pubkey_b58.into(),
            amount_micro_usdc: take,
            job_id,
            reason: reason.into(),
            slashed_at_ms: now_ms,
        };
        if let Some(journal) = &self.journal {
            journal.record_bond_slash(&record)?;
        }
        ledger.seen_slashes.insert(slash_id.into());
        ledger.slashes.push(record);
        Ok(SlashOutcome::Slashed {
            amount_micro_usdc: take,
            at_stake_micro_usdc: at_stake - take,
        })
    }

    /// Registers an unbond request against the operator's committed
    /// stake, exactly once per `unbond_id`. The amount stays slashable
    /// until the matured push actually pays it.
    pub fn request_unbond(&self, state: UnbondState) -> Result<UnbondOutcome, JournalError> {
        let mut ledger = self.ledger.lock();
        if let Some(existing) = ledger.unbonds.get(&state.unbond_id) {
            return Ok(UnbondOutcome::Duplicate(existing.clone()));
        }
        let committed = ledger
            .status(&state.operator_pubkey_b58)
            .committed_micro_usdc;
        if state.amount_micro_usdc > committed {
            return Ok(UnbondOutcome::Insufficient {
                committed_micro_usdc: committed,
            });
        }
        if let Some(journal) = &self.journal {
            journal.record_unbond(&state)?;
        }
        ledger.unbonds.insert(state.unbond_id, state.clone());
        Ok(UnbondOutcome::Requested(state))
    }

    /// What a matured, unpushed unbond would pay right now: its
    /// requested amount capped by the stake still standing. `None` for
    /// unknown, not-yet-matured, or already-pushed requests.
    pub fn payable(&self, unbond_id: Uuid, now_ms: u64) -> Option<u64> {
        let ledger = self.ledger.lock();
        let unbond = ledger.unbonds.get(&unbond_id)?;
        if unbond.pushed.is_some() || now_ms < unbond.matures_at_ms {
            return None;
        }
        let at_stake = ledger
            .status(&unbond.operator_pubkey_b58)
            .at_stake_micro_usdc;
        Some(unbond.amount_micro_usdc.min(at_stake))
    }

    /// Fences a matured refund's payable off from the slash clamp and
    /// returns it, atomically with the payable read — the amount the
    /// caller may now transfer. `None` for anything
    /// [`OperatorBonds::payable`] would refuse, plus a refund already
    /// reserved. The reservation holds until
    /// [`OperatorBonds::record_refunded`] books the push or
    /// [`OperatorBonds::release_reservation`] frees a definitively
    /// failed one; a transfer whose outcome is unknown keeps it, so an
    /// in-doubt refund can neither retry nor be slashed into a
    /// double-draw.
    pub fn reserve_refund(&self, unbond_id: Uuid, now_ms: u64) -> Option<u64> {
        let mut ledger = self.ledger.lock();
        if ledger.reserved.contains_key(&unbond_id) {
            return None;
        }
        let unbond = ledger.unbonds.get(&unbond_id)?;
        if unbond.pushed.is_some() || now_ms < unbond.matures_at_ms {
            return None;
        }
        let operator = unbond.operator_pubkey_b58.clone();
        let at_stake = ledger
            .status(&operator)
            .at_stake_micro_usdc
            .saturating_sub(ledger.reserved_for(&operator));
        let payable = unbond.amount_micro_usdc.min(at_stake);
        if payable > 0 {
            ledger.reserved.insert(unbond_id, (operator, payable));
        }
        Some(payable)
    }

    /// Frees a reservation whose transfer definitively did not happen.
    pub fn release_reservation(&self, unbond_id: Uuid) {
        self.ledger.lock().reserved.remove(&unbond_id);
    }

    /// Re-fences stake for a refund whose outcome a restart left
    /// unknown — the boot half of [`OperatorBonds::reserve_refund`],
    /// seeded from the suspended transfer bracket, so a post-restart
    /// slash still can't double-draw the in-doubt principal.
    pub fn reinstate_reservation(&self, unbond_id: Uuid, amount_micro_usdc: u64) {
        let mut ledger = self.ledger.lock();
        let Some(unbond) = ledger.unbonds.get(&unbond_id) else {
            return;
        };
        if unbond.pushed.is_some() {
            return;
        }
        let operator = unbond.operator_pubkey_b58.clone();
        ledger
            .reserved
            .insert(unbond_id, (operator, amount_micro_usdc));
    }

    /// Pins a completed refund push onto the request. Idempotent — an
    /// already-pushed record is returned unchanged; `Ok(None)` means no
    /// such request exists (a push for a request the books never saw is
    /// a bug at the call site, not a fact to invent). The paid amount
    /// is recorded verbatim — the transfer already happened — with a
    /// loud reconciliation alarm if it left the books claiming more
    /// out than ever came in. Releases any in-flight reservation for
    /// the request: the spend is now booked where the slash clamp
    /// already sees it.
    pub fn record_refunded(
        &self,
        unbond_id: Uuid,
        push: BondRefundPush,
    ) -> Result<Option<UnbondState>, JournalError> {
        let mut ledger = self.ledger.lock();
        let Some(existing) = ledger.unbonds.get(&unbond_id) else {
            return Ok(None);
        };
        if existing.pushed.is_some() {
            return Ok(Some(existing.clone()));
        }
        let updated = UnbondState {
            pushed: Some(push),
            ..existing.clone()
        };
        if let Some(journal) = &self.journal {
            journal.record_unbond(&updated)?;
        }
        let operator = updated.operator_pubkey_b58.clone();
        ledger.unbonds.insert(unbond_id, updated.clone());
        ledger.reserved.remove(&unbond_id);
        let status = ledger.status(&operator);
        if status.slashed_micro_usdc + status.refunded_micro_usdc > status.posted_micro_usdc {
            tracing::error!(
                operator = %operator,
                posted = status.posted_micro_usdc,
                slashed = status.slashed_micro_usdc,
                refunded = status.refunded_micro_usdc,
                "bond books reconciliation: outflow exceeds posted stake"
            );
        }
        Ok(Some(updated))
    }

    /// Matured requests no transfer ever honored — the retry sweep's
    /// worklist. The sweep re-reads [`OperatorBonds::payable`] per item
    /// so the paid amount reflects any slash that landed since.
    pub fn matured_unpushed(&self, now_ms: u64) -> Vec<UnbondState> {
        let ledger = self.ledger.lock();
        ledger
            .unbonds
            .values()
            .filter(|u| {
                u.pushed.is_none()
                    && now_ms >= u.matures_at_ms
                    && !ledger.reserved.contains_key(&u.unbond_id)
            })
            .cloned()
            .collect()
    }

    /// Every operator's stake summed into one [`BondStatus`] — the
    /// aggregate `/metrics` serves. Slashes clamp at the standing
    /// stake and unbonds draw only from committed, so the per-operator
    /// identities (posted = slashed + refunded + at_stake) survive the
    /// summation exactly. Only operators with a post can hold slashes
    /// or unbonds, so walking the posted keys covers the whole book.
    pub fn totals(&self) -> BondStatus {
        let ledger = self.ledger.lock();
        let operators: Vec<String> = ledger.posted.keys().cloned().collect();
        let mut totals = BondStatus {
            posted_micro_usdc: 0,
            slashed_micro_usdc: 0,
            unbonding_micro_usdc: 0,
            refunded_micro_usdc: 0,
            at_stake_micro_usdc: 0,
            committed_micro_usdc: 0,
        };
        for operator in operators {
            let status = ledger.status(&operator);
            totals.posted_micro_usdc += status.posted_micro_usdc;
            totals.slashed_micro_usdc += status.slashed_micro_usdc;
            totals.unbonding_micro_usdc += status.unbonding_micro_usdc;
            totals.refunded_micro_usdc += status.refunded_micro_usdc;
            totals.at_stake_micro_usdc += status.at_stake_micro_usdc;
            totals.committed_micro_usdc += status.committed_micro_usdc;
        }
        totals
    }

    pub fn status(&self, operator_pubkey_b58: &str) -> BondStatus {
        self.ledger.lock().status(operator_pubkey_b58)
    }

    pub fn get_unbond(&self, unbond_id: Uuid) -> Option<UnbondState> {
        self.ledger.lock().unbonds.get(&unbond_id).cloned()
    }

    /// This operator's unbond requests, newest first.
    pub fn unbonds_for(&self, operator_pubkey_b58: &str) -> Vec<UnbondState> {
        let mut rows: Vec<UnbondState> = self
            .ledger
            .lock()
            .unbonds
            .values()
            .filter(|u| u.operator_pubkey_b58 == operator_pubkey_b58)
            .cloned()
            .collect();
        rows.sort_by(|a, b| {
            b.requested_at_ms
                .cmp(&a.requested_at_ms)
                .then(a.unbond_id.cmp(&b.unbond_id))
        });
        rows
    }

    /// This operator's slashes, newest first.
    pub fn slashes_for(&self, operator_pubkey_b58: &str) -> Vec<SlashRecord> {
        let mut rows: Vec<SlashRecord> = self
            .ledger
            .lock()
            .slashes
            .iter()
            .filter(|s| s.operator_pubkey_b58 == operator_pubkey_b58)
            .cloned()
            .collect();
        rows.sort_by(|a, b| {
            b.slashed_at_ms
                .cmp(&a.slashed_at_ms)
                .then(a.slash_id.cmp(&b.slash_id))
        });
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OP: &str = "operator-a";
    const RECIPIENT: &str = "recipient";

    fn unbond(operator: &str, amount: u64, matures_at_ms: u64) -> UnbondState {
        UnbondState {
            unbond_id: Uuid::new_v4(),
            operator_pubkey_b58: operator.into(),
            recipient_address_b58: RECIPIENT.into(),
            amount_micro_usdc: amount,
            requested_at_ms: 1,
            matures_at_ms,
            pushed: None,
        }
    }

    #[test]
    fn posts_credit_once_per_bond_id() {
        let bonds = OperatorBonds::new();
        assert_eq!(
            bonds.credit_post("sig-1", OP, 5_000).unwrap(),
            BondPostOutcome::Credited {
                posted_total_micro_usdc: 5_000
            }
        );
        assert_eq!(
            bonds.credit_post("sig-1", OP, 5_000).unwrap(),
            BondPostOutcome::Duplicate {
                posted_total_micro_usdc: 5_000
            }
        );
        assert_eq!(
            bonds.credit_post("sig-2", OP, 1_000).unwrap(),
            BondPostOutcome::Credited {
                posted_total_micro_usdc: 6_000
            }
        );
        let status = bonds.status(OP);
        assert_eq!(status.posted_micro_usdc, 6_000);
        assert_eq!(status.at_stake_micro_usdc, 6_000);
        assert_eq!(status.committed_micro_usdc, 6_000);
        assert_eq!(bonds.status("operator-unknown").posted_micro_usdc, 0);
    }

    #[test]
    fn slashes_clamp_at_standing_stake_and_dedup_by_id() {
        let bonds = OperatorBonds::new();
        bonds.credit_post("sig-1", OP, 1_000).unwrap();

        assert_eq!(
            bonds
                .slash(
                    "canary:j1",
                    OP,
                    700,
                    Uuid::new_v4(),
                    "canary wrong-answer",
                    10
                )
                .unwrap(),
            SlashOutcome::Slashed {
                amount_micro_usdc: 700,
                at_stake_micro_usdc: 300
            }
        );
        // A replayed verdict moves nothing.
        assert_eq!(
            bonds
                .slash(
                    "canary:j1",
                    OP,
                    700,
                    Uuid::new_v4(),
                    "canary wrong-answer",
                    11
                )
                .unwrap(),
            SlashOutcome::Duplicate
        );
        // A second fault takes what is left, not what it asked for.
        assert_eq!(
            bonds
                .slash("redundancy:j2", OP, 900, Uuid::new_v4(), "minority", 12)
                .unwrap(),
            SlashOutcome::Slashed {
                amount_micro_usdc: 300,
                at_stake_micro_usdc: 0
            }
        );
        // Nothing standing: no fact, no journal line.
        assert_eq!(
            bonds
                .slash(
                    "canary:j3",
                    OP,
                    100,
                    Uuid::new_v4(),
                    "canary wrong-answer",
                    13
                )
                .unwrap(),
            SlashOutcome::NoStake
        );
        assert_eq!(bonds.status(OP).slashed_micro_usdc, 1_000);
        assert_eq!(bonds.slashes_for(OP).len(), 2);
    }

    #[test]
    fn unbonds_draw_from_committed_stake_only() {
        let bonds = OperatorBonds::new();
        bonds.credit_post("sig-1", OP, 1_000).unwrap();
        bonds
            .slash("canary:j1", OP, 200, Uuid::new_v4(), "canary", 5)
            .unwrap();

        let first = unbond(OP, 600, 100);
        assert_eq!(
            bonds.request_unbond(first.clone()).unwrap(),
            UnbondOutcome::Requested(first.clone())
        );
        assert_eq!(
            bonds.request_unbond(first.clone()).unwrap(),
            UnbondOutcome::Duplicate(first)
        );
        // 1000 posted − 200 slashed − 600 pending = 200 committed.
        assert_eq!(
            bonds.request_unbond(unbond(OP, 300, 100)).unwrap(),
            UnbondOutcome::Insufficient {
                committed_micro_usdc: 200
            }
        );
        let drain = unbond(OP, 200, 100);
        assert_eq!(
            bonds.request_unbond(drain.clone()).unwrap(),
            UnbondOutcome::Requested(drain)
        );
        let status = bonds.status(OP);
        assert_eq!(status.unbonding_micro_usdc, 800);
        assert_eq!(status.committed_micro_usdc, 0);
        // Still fully slashable — requests reserve nothing.
        assert_eq!(status.at_stake_micro_usdc, 800);
    }

    #[test]
    fn a_slash_during_maturation_shrinks_the_refund() {
        let bonds = OperatorBonds::new();
        bonds.credit_post("sig-1", OP, 1_000).unwrap();
        let request = unbond(OP, 1_000, 100);
        bonds.request_unbond(request.clone()).unwrap();

        // Not matured yet.
        assert_eq!(bonds.payable(request.unbond_id, 50), None);
        // The fault lands while the request matures.
        bonds
            .slash("canary:j1", OP, 400, Uuid::new_v4(), "canary", 60)
            .unwrap();
        // Matured: the refund is the remainder, not the request.
        assert_eq!(bonds.payable(request.unbond_id, 100), Some(600));

        let pushed = bonds
            .record_refunded(
                request.unbond_id,
                BondRefundPush {
                    tx_signature: Some("sig-r".into()),
                    recorded_at_ms: 101,
                    paid_micro_usdc: 600,
                },
            )
            .unwrap()
            .expect("known request");
        assert_eq!(pushed.pushed.as_ref().unwrap().paid_micro_usdc, 600);
        // Pushed requests stop being payable and the books balance.
        assert_eq!(bonds.payable(request.unbond_id, 200), None);
        let status = bonds.status(OP);
        assert_eq!(status.refunded_micro_usdc, 600);
        assert_eq!(status.slashed_micro_usdc, 400);
        assert_eq!(status.at_stake_micro_usdc, 0);
        assert_eq!(status.unbonding_micro_usdc, 0);
    }

    #[test]
    fn refund_pushes_pin_once_and_unknown_ids_are_never_invented() {
        let bonds = OperatorBonds::new();
        bonds.credit_post("sig-1", OP, 500).unwrap();
        let request = unbond(OP, 500, 10);
        bonds.request_unbond(request.clone()).unwrap();
        assert_eq!(bonds.matured_unpushed(9).len(), 0);
        assert_eq!(bonds.matured_unpushed(10).len(), 1);

        bonds
            .record_refunded(
                request.unbond_id,
                BondRefundPush {
                    tx_signature: Some("sig-a".into()),
                    recorded_at_ms: 11,
                    paid_micro_usdc: 500,
                },
            )
            .unwrap()
            .expect("known request");
        // The sweep racing the first-chance push keeps the original.
        let again = bonds
            .record_refunded(
                request.unbond_id,
                BondRefundPush {
                    tx_signature: Some("sig-b".into()),
                    recorded_at_ms: 12,
                    paid_micro_usdc: 500,
                },
            )
            .unwrap()
            .expect("still known");
        assert_eq!(
            again.pushed.as_ref().unwrap().tx_signature.as_deref(),
            Some("sig-a")
        );
        assert!(bonds.matured_unpushed(20).is_empty());
        assert!(bonds
            .record_refunded(
                Uuid::new_v4(),
                BondRefundPush {
                    tx_signature: None,
                    recorded_at_ms: 13,
                    paid_micro_usdc: 0,
                },
            )
            .unwrap()
            .is_none());
    }

    #[test]
    fn restored_books_replay_posts_slashes_and_unbonds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let (request, job_id) = {
            let journal = Arc::new(Journal::open(&path).unwrap());
            let bonds = OperatorBonds::restore(
                HashMap::new(),
                HashSet::new(),
                Vec::new(),
                HashMap::new(),
                journal,
            );
            bonds.credit_post("sig-1", OP, 2_000).unwrap();
            let job_id = Uuid::new_v4();
            bonds
                .slash("canary:j1", OP, 300, job_id, "canary wrong-answer", 5)
                .unwrap();
            let request = unbond(OP, 700, 50);
            bonds.request_unbond(request.clone()).unwrap();
            (request, job_id)
        };

        let restored = crate::journal::Journal::load(&path).unwrap();
        let journal = Arc::new(Journal::open(&path).unwrap());
        let bonds = OperatorBonds::restore(
            restored.bond_totals,
            restored.bond_ids,
            restored.bond_slashes,
            restored.unbonds,
            journal,
        );
        let status = bonds.status(OP);
        assert_eq!(status.posted_micro_usdc, 2_000);
        assert_eq!(status.slashed_micro_usdc, 300);
        assert_eq!(status.unbonding_micro_usdc, 700);
        assert_eq!(status.committed_micro_usdc, 1_000);
        assert_eq!(bonds.slashes_for(OP)[0].job_id, job_id);
        // Replayed ids still dedup.
        assert_eq!(
            bonds.credit_post("sig-1", OP, 2_000).unwrap(),
            BondPostOutcome::Duplicate {
                posted_total_micro_usdc: 2_000
            }
        );
        assert_eq!(
            bonds
                .slash("canary:j1", OP, 300, job_id, "replay", 6)
                .unwrap(),
            SlashOutcome::Duplicate
        );
        assert_eq!(
            bonds.request_unbond(request.clone()).unwrap(),
            UnbondOutcome::Duplicate(request)
        );
    }

    #[test]
    fn operator_views_sort_newest_first() {
        let bonds = OperatorBonds::new();
        bonds.credit_post("sig-1", OP, 10_000).unwrap();
        let mut old = unbond(OP, 10, 100);
        old.requested_at_ms = 100;
        let mut new = unbond(OP, 20, 100);
        new.requested_at_ms = 200;
        bonds.request_unbond(old.clone()).unwrap();
        bonds.request_unbond(new.clone()).unwrap();
        bonds.request_unbond(unbond("operator-b", 5, 100)).unwrap();

        let rows = bonds.unbonds_for(OP);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].unbond_id, new.unbond_id);
        assert_eq!(rows[1].unbond_id, old.unbond_id);

        bonds
            .slash("a:j1", OP, 1, Uuid::new_v4(), "old", 100)
            .unwrap();
        bonds
            .slash("b:j2", OP, 1, Uuid::new_v4(), "new", 200)
            .unwrap();
        let slashes = bonds.slashes_for(OP);
        assert_eq!(slashes[0].reason, "new");
        assert_eq!(slashes[1].reason, "old");
    }

    #[test]
    fn totals_aggregate_every_operators_stake() {
        let bonds = OperatorBonds::new();
        bonds.credit_post("post-a", "operator-a", 1_000).unwrap();
        bonds.credit_post("post-b", "operator-b", 500).unwrap();
        bonds
            .slash("canary:j1", "operator-a", 300, Uuid::new_v4(), "fault", 1)
            .unwrap();
        // operator-b heads for the exit: one refund pushed, one still
        // maturing.
        let pushed = unbond("operator-b", 200, 1);
        bonds.request_unbond(pushed.clone()).unwrap();
        bonds
            .record_refunded(
                pushed.unbond_id,
                BondRefundPush {
                    tx_signature: None,
                    recorded_at_ms: 2,
                    paid_micro_usdc: 200,
                },
            )
            .unwrap();
        bonds.request_unbond(unbond("operator-b", 100, 10)).unwrap();

        let totals = bonds.totals();
        assert_eq!(totals.posted_micro_usdc, 1_500);
        assert_eq!(totals.slashed_micro_usdc, 300);
        assert_eq!(totals.refunded_micro_usdc, 200);
        assert_eq!(totals.unbonding_micro_usdc, 100);
        assert_eq!(totals.at_stake_micro_usdc, 1_000);
        assert_eq!(totals.committed_micro_usdc, 900);
        // The aggregate holds the same identity every per-operator
        // status does.
        assert_eq!(
            totals.posted_micro_usdc,
            totals.slashed_micro_usdc + totals.refunded_micro_usdc + totals.at_stake_micro_usdc
        );
    }

    #[test]
    fn a_reserved_refund_is_fenced_from_a_slash_that_lands_mid_transfer() {
        // The double-draw the reservation exists to stop: once the
        // sidecar is spending a matured refund on the wire, that
        // principal is already leaving, so a fault adjudicated in the
        // same window must take only what is left — never the money the
        // transfer is carrying.
        let bonds = OperatorBonds::new();
        bonds.credit_post("sig-1", OP, 1_000).unwrap();
        let request = unbond(OP, 1_000, 100);
        bonds.request_unbond(request.clone()).unwrap();

        assert_eq!(bonds.reserve_refund(request.unbond_id, 100), Some(1_000));
        assert_eq!(
            bonds
                .slash("canary:j1", OP, 1_000, Uuid::new_v4(), "wrong-answer", 110)
                .unwrap(),
            SlashOutcome::NoStake
        );

        bonds
            .record_refunded(
                request.unbond_id,
                BondRefundPush {
                    tx_signature: Some("sig-r".into()),
                    recorded_at_ms: 120,
                    paid_micro_usdc: 1_000,
                },
            )
            .unwrap()
            .expect("known request");
        let status = bonds.status(OP);
        assert_eq!(status.refunded_micro_usdc, 1_000);
        assert_eq!(status.slashed_micro_usdc, 0);
        assert!(
            status.slashed_micro_usdc + status.refunded_micro_usdc <= status.posted_micro_usdc,
            "no principal pays both a refund and a slash"
        );
    }

    #[test]
    fn a_released_reservation_is_slashable_again() {
        // A refund whose transfer definitively did not happen frees its
        // fence: the stake never left, so a pending fault can still
        // reach it, and the request now pays nothing on retry.
        let bonds = OperatorBonds::new();
        bonds.credit_post("sig-1", OP, 1_000).unwrap();
        let request = unbond(OP, 1_000, 100);
        bonds.request_unbond(request.clone()).unwrap();

        assert_eq!(bonds.reserve_refund(request.unbond_id, 100), Some(1_000));
        bonds.release_reservation(request.unbond_id);

        assert_eq!(
            bonds
                .slash("canary:j1", OP, 1_000, Uuid::new_v4(), "wrong-answer", 110)
                .unwrap(),
            SlashOutcome::Slashed {
                amount_micro_usdc: 1_000,
                at_stake_micro_usdc: 0
            }
        );
        assert_eq!(bonds.payable(request.unbond_id, 120), Some(0));
    }

    #[test]
    fn a_reinstated_reservation_refences_in_doubt_stake_after_a_restart() {
        // A restart caught a refund transfer with an unknown outcome.
        // Boot re-fences the in-doubt principal from the suspended
        // attempt so a post-restart slash still cannot double-draw it.
        let bonds = OperatorBonds::new();
        bonds.credit_post("sig-1", OP, 1_000).unwrap();
        let request = unbond(OP, 1_000, 100);
        bonds.request_unbond(request.clone()).unwrap();

        bonds.reinstate_reservation(request.unbond_id, 1_000);
        assert_eq!(
            bonds
                .slash("canary:j1", OP, 1_000, Uuid::new_v4(), "wrong-answer", 110)
                .unwrap(),
            SlashOutcome::NoStake
        );
    }

    #[test]
    fn a_slash_takes_only_the_stake_a_reserved_refund_does_not_cover() {
        // Two matured refunds and a fault racing all three. The reserved
        // one is protected, the fault takes exactly the remainder, and
        // the second refund finds nothing standing. Every micro-usdc out
        // is accounted once.
        let bonds = OperatorBonds::new();
        bonds.credit_post("sig-1", OP, 1_000).unwrap();
        let a = unbond(OP, 500, 100);
        let b = unbond(OP, 500, 100);
        bonds.request_unbond(a.clone()).unwrap();
        bonds.request_unbond(b.clone()).unwrap();

        assert_eq!(bonds.reserve_refund(a.unbond_id, 100), Some(500));
        assert_eq!(
            bonds
                .slash("canary:j1", OP, 1_000, Uuid::new_v4(), "minority", 110)
                .unwrap(),
            SlashOutcome::Slashed {
                amount_micro_usdc: 500,
                at_stake_micro_usdc: 0
            }
        );
        bonds
            .record_refunded(
                a.unbond_id,
                BondRefundPush {
                    tx_signature: Some("sig-a".into()),
                    recorded_at_ms: 120,
                    paid_micro_usdc: 500,
                },
            )
            .unwrap()
            .expect("known request");
        assert_eq!(bonds.reserve_refund(b.unbond_id, 100), Some(0));

        let status = bonds.status(OP);
        assert_eq!(status.slashed_micro_usdc, 500);
        assert_eq!(status.refunded_micro_usdc, 500);
        assert_eq!(
            status.slashed_micro_usdc + status.refunded_micro_usdc,
            status.posted_micro_usdc
        );
    }

    #[test]
    fn reserve_refund_refuses_the_unpayable_and_the_already_reserved() {
        let bonds = OperatorBonds::new();
        bonds.credit_post("sig-1", OP, 1_000).unwrap();
        let request = unbond(OP, 1_000, 100);
        bonds.request_unbond(request.clone()).unwrap();

        assert_eq!(bonds.reserve_refund(Uuid::new_v4(), 100), None);
        assert_eq!(bonds.reserve_refund(request.unbond_id, 50), None);
        assert_eq!(bonds.reserve_refund(request.unbond_id, 100), Some(1_000));
        // One transfer, one fence: a second reservation is refused.
        assert_eq!(bonds.reserve_refund(request.unbond_id, 100), None);
        bonds
            .record_refunded(
                request.unbond_id,
                BondRefundPush {
                    tx_signature: Some("sig-r".into()),
                    recorded_at_ms: 110,
                    paid_micro_usdc: 1_000,
                },
            )
            .unwrap();
        // Nothing left to reserve once the push has booked.
        assert_eq!(bonds.reserve_refund(request.unbond_id, 120), None);
    }
}
