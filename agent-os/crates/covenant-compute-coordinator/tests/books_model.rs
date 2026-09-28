//! Model-based differential tests over the coordinator's money books.
//!
//! Every other test in this crate drives a chosen scenario; these
//! drive hundreds of seeded-random operation sequences against the
//! real books — the same `restore`/`with_accounts`/`with_withdrawals`/
//! `with_subsidy_policy` wiring `CoordinatorState::with_journal`
//! builds — and check after every step that the books agree with a
//! deliberately-trivial reference model, and that the algebraic
//! invariants no example-based test can exhaustively pin still hold:
//!
//! - a buyer's deposits always cover their non-refunded organic holds
//!   plus their withdrawals (the property whose violation is an
//!   overdraft);
//! - committed bootstrap subsidy never exceeds the policy ceiling
//!   (the anti-faucet kill-switch);
//! - `Journal::load` reconstructs the live books exactly at any
//!   point, with restarts and compactions interleaved into the
//!   sequence, not just at a hand-picked end state.
//!
//! Restarts here rebuild the books the way `with_journal` does but
//! stop before `recover::reconcile_books`: that boot pass heals
//! books↔job-record disagreements and has its own tests; the property
//! under test is that restore is exact.
//!
//! Deterministic on purpose: a hand-rolled splitmix64 walks a fixed
//! seed range, so a failure names its seed and op index and reproduces
//! byte-for-byte — no flaky CI, no property-test dependency.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use covenant_a2a::A2ATaskStatus;
use covenant_compute_coordinator::accounts::{BuyerWithdrawals, WithdrawalPush, WithdrawalState};
use covenant_compute_coordinator::escrow::WithdrawOutcome;
use covenant_compute_coordinator::{
    BuyerAccounts, CustodialEscrow, EscrowHoldState, Journal, PartnerPayoutOutcome, PartnerPayouts,
    SubsidyPolicy,
};
use covenant_compute_protocol::{
    EscrowError, EscrowStatus, FederationEscrow, FundingSource, JobMeter, RefundReason,
    SignedWorkReceipt, WorkReceiptPayload,
};
use covenant_identity::LocalIdentity;
use uuid::Uuid;

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }

    fn coin(&mut self) -> bool {
        self.below(2) == 0
    }
}

fn receipt_with_status(
    job_id: Uuid,
    operator: &LocalIdentity,
    status: A2ATaskStatus,
) -> SignedWorkReceipt {
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
            price_micro_usdc: 1,
            status,
            executed_at_ms: 1,
            node_audit_root_hex: "cc".repeat(32),
        },
        operator,
    )
    .expect("receipt signs")
}

const SUBSIDY_FLOOR: u64 = 2_000;
const SUBSIDY_RATIO_BPS: u32 = 5_000;

/// The policy's ceiling formula, restated independently so a drift in
/// the escrow's arithmetic fails against this, not against itself.
fn subsidy_ceiling(organic_released: u64) -> u64 {
    SUBSIDY_FLOOR
        + u64::try_from(u128::from(organic_released) * u128::from(SUBSIDY_RATIO_BPS) / 10_000)
            .expect("ceiling fits")
}

/// The reference model: the same facts the journal persists, kept in
/// plain maps, with every derived number recomputed from scratch on
/// each ask. No locks, no journals, no derivation caching — if the
/// real books and this ever disagree, the books are wrong or the spec
/// changed.
#[derive(Default)]
struct MoneyModel {
    deposited: BTreeMap<String, u64>,
    deposit_ids: BTreeSet<String>,
    holds: BTreeMap<Uuid, EscrowHoldState>,
    withdrawals: BTreeMap<Uuid, WithdrawalState>,
}

impl MoneyModel {
    fn charged(&self, buyer: &str) -> u64 {
        self.holds
            .values()
            .filter(|h| {
                h.buyer_pubkey_b58 == buyer
                    && h.funding_source == FundingSource::Organic
                    && h.status != EscrowStatus::Refunded
            })
            .map(|h| h.amount_micro_usdc)
            .sum()
    }

    fn withdrawn(&self, buyer: &str) -> u64 {
        self.withdrawals
            .values()
            .filter(|w| w.buyer_pubkey_b58 == buyer)
            .map(|w| w.amount_micro_usdc)
            .sum()
    }

    fn deposited(&self, buyer: &str) -> u64 {
        self.deposited.get(buyer).copied().unwrap_or(0)
    }

    /// Funds conservation is asserted here, on every read: deposits
    /// must cover charges plus withdrawals, or some operation
    /// overdrew the buyer.
    fn available(&self, buyer: &str, ctx: &str) -> u64 {
        let (d, c, w) = (
            self.deposited(buyer),
            self.charged(buyer),
            self.withdrawn(buyer),
        );
        assert!(
            d >= c + w,
            "{ctx}: buyer {buyer} overdrawn: deposited {d} < charged {c} + withdrawn {w}"
        );
        d - c - w
    }

    fn bootstrap_committed(&self) -> u64 {
        self.holds
            .values()
            .filter(|h| {
                h.funding_source == FundingSource::Bootstrap && h.status != EscrowStatus::Refunded
            })
            .map(|h| h.amount_micro_usdc)
            .sum()
    }

    fn organic_released(&self) -> u64 {
        self.holds
            .values()
            .filter(|h| {
                h.funding_source == FundingSource::Organic && h.status == EscrowStatus::Released
            })
            .map(|h| h.amount_micro_usdc)
            .sum()
    }
}

struct Books {
    escrow: CustodialEscrow,
    accounts: Arc<BuyerAccounts>,
    withdrawals: Arc<BuyerWithdrawals>,
}

/// The `with_journal` restore path, books only: load, open, optional
/// boot compaction, then restore each book from the loaded state. A
/// fresh coordinator identity per boot is fine — hold state carries no
/// identity linkage, only new attestations use it.
fn open_books(path: &Path, boot_compact: bool) -> (Books, Arc<Journal>) {
    let restored = Journal::load(path).expect("journal replays");
    let journal = Arc::new(Journal::open(path).expect("journal opens"));
    if boot_compact {
        journal.compact().expect("boot compaction");
    }
    let accounts = Arc::new(BuyerAccounts::restore(
        restored.deposit_totals,
        restored.deposit_ids,
        journal.clone(),
    ));
    let withdrawals = Arc::new(BuyerWithdrawals::restore(
        restored.withdrawals,
        journal.clone(),
    ));
    let escrow = CustodialEscrow::restore(
        LocalIdentity::generate("coordinator@books-model"),
        FundingSource::Organic,
        restored.holds,
        journal.clone(),
    )
    .with_accounts(accounts.clone())
    .with_withdrawals(withdrawals.clone())
    .with_subsidy_policy(
        SubsidyPolicy::new(SUBSIDY_RATIO_BPS, SUBSIDY_FLOOR).expect("ratio within bounds"),
    );
    (
        Books {
            escrow,
            accounts,
            withdrawals,
        },
        journal,
    )
}

/// The full checkpoint: what the journal replays, what the live books
/// hold, and what the model says must all be one state.
fn assert_books_match_model(path: &Path, books: &Books, model: &MoneyModel, ctx: &str) {
    let model_holds: HashMap<Uuid, EscrowHoldState> =
        model.holds.iter().map(|(k, v)| (*k, v.clone())).collect();
    let restored = Journal::load(path).expect("journal replays");
    assert_eq!(restored.holds, model_holds, "{ctx}: replayed holds diverge");
    let live_holds: HashMap<Uuid, EscrowHoldState> =
        books.escrow.holds_snapshot().into_iter().collect();
    assert_eq!(live_holds, model_holds, "{ctx}: live holds diverge");

    let model_deposits: HashMap<String, u64> = model
        .deposited
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    assert_eq!(
        restored.deposit_totals, model_deposits,
        "{ctx}: replayed deposit totals diverge"
    );
    let model_ids: HashSet<String> = model.deposit_ids.iter().cloned().collect();
    assert_eq!(
        restored.deposit_ids, model_ids,
        "{ctx}: replayed deposit ids diverge"
    );

    let model_withdrawals: HashMap<Uuid, WithdrawalState> = model
        .withdrawals
        .iter()
        .map(|(k, v)| (*k, v.clone()))
        .collect();
    assert_eq!(
        restored.withdrawals, model_withdrawals,
        "{ctx}: replayed withdrawals diverge"
    );

    let status = books.escrow.subsidy_status();
    assert!(status.enforced, "{ctx}: policy went missing");
    assert_eq!(
        status.bootstrap_committed_micro_usdc,
        model.bootstrap_committed(),
        "{ctx}: committed subsidy diverges"
    );
    assert_eq!(
        status.organic_released_micro_usdc,
        model.organic_released(),
        "{ctx}: organic revenue diverges"
    );
    assert_eq!(
        status.ceiling_micro_usdc,
        subsidy_ceiling(model.organic_released()),
        "{ctx}: subsidy ceiling diverges"
    );
    assert_eq!(
        status.remaining_micro_usdc,
        status
            .ceiling_micro_usdc
            .saturating_sub(status.bootstrap_committed_micro_usdc),
        "{ctx}: subsidy remaining is not ceiling minus committed"
    );
}

#[tokio::test]
async fn random_money_op_sequences_conserve_buyer_funds_across_restart_and_compaction() {
    let dir = tempfile::tempdir().unwrap();
    for seed in 0..20u64 {
        let path = dir.path().join(format!("money-{seed}.jsonl"));
        let mut rng = Rng::new(0xC0FFEE ^ (seed << 8));

        let buyers: Vec<LocalIdentity> = (0..3)
            .map(|i| LocalIdentity::generate(format!("buyer-{i}@books-model")))
            .collect();
        let buyer_keys: Vec<String> = buyers
            .iter()
            .map(|b| b.agent_id().pubkey_base58())
            .collect();
        // Bootstrap holds are the coordinator's own probe spend; they
        // carry attribution but never charge a deposit.
        let probe = LocalIdentity::generate("probe@books-model");
        let probe_key = probe.agent_id().pubkey_base58();
        let operator = LocalIdentity::generate("operator@books-model");

        // Small pools so collisions — duplicate holds, settles of
        // settled jobs, replayed deposit and withdrawal ids — actually
        // happen instead of staying theoretical.
        let jobs: Vec<Uuid> = (0..10).map(|_| Uuid::new_v4()).collect();
        let withdrawal_ids: Vec<Uuid> = (0..6).map(|_| Uuid::new_v4()).collect();

        let (mut books, mut journal) = open_books(&path, false);
        let mut model = MoneyModel::default();

        for op in 0..110u64 {
            let ctx = format!("seed {seed} op {op}");
            match rng.below(100) {
                // Deposit: idempotent by id, whatever amount a replay
                // claims the second time.
                0..=14 => {
                    let id = format!("dep-{}", rng.below(6));
                    let buyer = rng.pick(&buyer_keys).clone();
                    let amount = *rng.pick(&[0, 500, 1_000, 2_500, 10_000]);
                    let outcome = books.accounts.credit_deposit(&id, &buyer, amount).unwrap();
                    if model.deposit_ids.contains(&id) {
                        assert_eq!(
                            outcome,
                            covenant_compute_coordinator::DepositOutcome::Duplicate {
                                deposited_total_micro_usdc: model.deposited(&buyer),
                            },
                            "{ctx}: duplicate deposit moved money"
                        );
                    } else {
                        model.deposit_ids.insert(id);
                        *model.deposited.entry(buyer.clone()).or_default() += amount;
                        assert_eq!(
                            outcome,
                            covenant_compute_coordinator::DepositOutcome::Credited {
                                deposited_total_micro_usdc: model.deposited(&buyer),
                            },
                            "{ctx}: deposit credited wrong"
                        );
                    }
                }
                // Organic hold: covered by the buyer's balance or
                // refused with the exact shortfall.
                15..=36 => {
                    let job = *rng.pick(&jobs);
                    let bi = rng.below(3) as usize;
                    let buyer = buyer_keys[bi].clone();
                    let amount = *rng.pick(&[0, 1, 300, 700, 1_500, 5_000]);
                    let got = books
                        .escrow
                        .hold_with_source(
                            job,
                            &buyers[bi].agent_id(),
                            amount,
                            FundingSource::Organic,
                        )
                        .await;
                    if model.holds.contains_key(&job) {
                        assert!(
                            matches!(got, Err(EscrowError::AlreadyHeld(id)) if id == job),
                            "{ctx}: duplicate hold not refused: {got:?}"
                        );
                    } else {
                        let available = model.available(&buyer, &ctx);
                        if amount > available {
                            match got {
                                Err(EscrowError::InsufficientFunds {
                                    buyer_pubkey_b58,
                                    needed_micro_usdc,
                                    available_micro_usdc,
                                }) => {
                                    assert_eq!(buyer_pubkey_b58, buyer, "{ctx}");
                                    assert_eq!(needed_micro_usdc, amount, "{ctx}");
                                    assert_eq!(
                                        available_micro_usdc, available,
                                        "{ctx}: shortfall reports the wrong balance"
                                    );
                                }
                                other => panic!("{ctx}: overdraft hold not refused: {other:?}"),
                            }
                        } else {
                            got.unwrap_or_else(|e| panic!("{ctx}: covered hold refused: {e}"));
                            model.holds.insert(
                                job,
                                EscrowHoldState {
                                    amount_micro_usdc: amount,
                                    funding_source: FundingSource::Organic,
                                    status: EscrowStatus::Held,
                                    buyer_pubkey_b58: buyer,
                                },
                            );
                        }
                    }
                }
                // Bootstrap hold: gated by the subsidy ceiling, never
                // by any buyer's deposits.
                37..=44 => {
                    let job = *rng.pick(&jobs);
                    let amount = *rng.pick(&[100, 500, 1_500, 3_000]);
                    let got = books
                        .escrow
                        .hold_with_source(job, &probe.agent_id(), amount, FundingSource::Bootstrap)
                        .await;
                    if model.holds.contains_key(&job) {
                        assert!(
                            matches!(got, Err(EscrowError::AlreadyHeld(id)) if id == job),
                            "{ctx}: duplicate hold not refused: {got:?}"
                        );
                    } else {
                        let spent = model.bootstrap_committed();
                        let ceiling = subsidy_ceiling(model.organic_released());
                        if amount > ceiling.saturating_sub(spent) {
                            match got {
                                Err(EscrowError::SubsidyExhausted {
                                    spent_micro_usdc,
                                    ceiling_micro_usdc,
                                }) => {
                                    assert_eq!(spent_micro_usdc, spent, "{ctx}");
                                    assert_eq!(ceiling_micro_usdc, ceiling, "{ctx}");
                                }
                                other => panic!("{ctx}: over-ceiling subsidy allowed: {other:?}"),
                            }
                        } else {
                            got.unwrap_or_else(|e| panic!("{ctx}: in-budget subsidy refused: {e}"));
                            model.holds.insert(
                                job,
                                EscrowHoldState {
                                    amount_micro_usdc: amount,
                                    funding_source: FundingSource::Bootstrap,
                                    status: EscrowStatus::Held,
                                    buyer_pubkey_b58: probe_key.clone(),
                                },
                            );
                        }
                    }
                }
                // Release against a valid receipt for this very job.
                45..=59 => {
                    let job = *rng.pick(&jobs);
                    let receipt = receipt_with_status(job, &operator, A2ATaskStatus::Ok);
                    let got = books.escrow.release(job, &receipt).await;
                    match model.holds.get_mut(&job) {
                        None => assert!(
                            matches!(got, Err(EscrowError::NotFound(id)) if id == job),
                            "{ctx}: release of unknown hold: {got:?}"
                        ),
                        Some(h) if h.status != EscrowStatus::Held => assert!(
                            matches!(got, Err(EscrowError::AlreadySettled(id)) if id == job),
                            "{ctx}: settled hold released again: {got:?}"
                        ),
                        Some(h) => {
                            got.unwrap_or_else(|e| panic!("{ctx}: held release refused: {e}"));
                            h.status = EscrowStatus::Released;
                        }
                    }
                }
                // Refund.
                60..=71 => {
                    let job = *rng.pick(&jobs);
                    let got = books
                        .escrow
                        .refund(job, RefundReason::DeadlineExpired)
                        .await;
                    match model.holds.get_mut(&job) {
                        None => assert!(
                            matches!(got, Err(EscrowError::NotFound(id)) if id == job),
                            "{ctx}: refund of unknown hold: {got:?}"
                        ),
                        Some(h) if h.status != EscrowStatus::Held => assert!(
                            matches!(got, Err(EscrowError::AlreadySettled(id)) if id == job),
                            "{ctx}: settled hold refunded again: {got:?}"
                        ),
                        Some(h) => {
                            got.unwrap_or_else(|e| panic!("{ctx}: held refund refused: {e}"));
                            h.status = EscrowStatus::Refunded;
                        }
                    }
                }
                // A receipt for some other job, or an unpayable one,
                // must never move a hold — whatever state it is in.
                72..=77 => {
                    let ji = rng.below(jobs.len() as u64) as usize;
                    let job = jobs[ji];
                    let receipt = if rng.coin() {
                        receipt_with_status(
                            jobs[(ji + 1) % jobs.len()],
                            &operator,
                            A2ATaskStatus::Ok,
                        )
                    } else {
                        receipt_with_status(job, &operator, A2ATaskStatus::Error)
                    };
                    let got = books.escrow.release(job, &receipt).await;
                    assert!(
                        matches!(got, Err(EscrowError::Backend(_))),
                        "{ctx}: bad receipt released a hold: {got:?}"
                    );
                }
                // Withdrawal: debits available balance exactly once
                // per id.
                78..=89 => {
                    let id = *rng.pick(&withdrawal_ids);
                    let buyer = rng.pick(&buyer_keys).clone();
                    let amount = *rng.pick(&[1, 250, 800, 2_000]);
                    let got = books.escrow.withdraw(
                        &books.accounts,
                        &buyer,
                        id,
                        "recipient@books-model",
                        amount,
                    );
                    match model.withdrawals.get(&id) {
                        Some(existing) if existing.buyer_pubkey_b58 == buyer => match got {
                            Ok(WithdrawOutcome::Duplicate(state)) => {
                                assert_eq!(&state, existing, "{ctx}: duplicate mutated the debit");
                            }
                            other => panic!("{ctx}: duplicate withdrawal not echoed: {other:?}"),
                        },
                        // A different buyer reusing an id is a collision, not a
                        // retry: the book refuses it without echoing or debiting
                        // the holder's record.
                        Some(_) => {
                            assert!(
                                matches!(got, Err(EscrowError::Backend(_))),
                                "{ctx}: cross-buyer id reuse must be refused: {got:?}"
                            );
                        }
                        None => {
                            let available = model.available(&buyer, &ctx);
                            if amount > available {
                                match got {
                                    Err(EscrowError::InsufficientFunds {
                                        available_micro_usdc,
                                        needed_micro_usdc,
                                        ..
                                    }) => {
                                        assert_eq!(needed_micro_usdc, amount, "{ctx}");
                                        assert_eq!(
                                            available_micro_usdc, available,
                                            "{ctx}: shortfall reports the wrong balance"
                                        );
                                    }
                                    other => {
                                        panic!("{ctx}: overdraft withdrawal allowed: {other:?}")
                                    }
                                }
                            } else {
                                match got {
                                    Ok(WithdrawOutcome::Requested(state)) => {
                                        assert_eq!(state.buyer_pubkey_b58, buyer, "{ctx}");
                                        assert_eq!(state.amount_micro_usdc, amount, "{ctx}");
                                        assert!(state.pushed.is_none(), "{ctx}");
                                        // The book stamps request time;
                                        // the model adopts the stamped
                                        // record as the fact to preserve.
                                        model.withdrawals.insert(id, state);
                                    }
                                    other => panic!("{ctx}: covered withdrawal refused: {other:?}"),
                                }
                            }
                        }
                    }
                }
                // Backend push lands on the debit exactly once.
                90..=95 => {
                    let id = *rng.pick(&withdrawal_ids);
                    let push = WithdrawalPush {
                        tx_signature: rng.coin().then(|| format!("tx-{op}")),
                        recorded_at_ms: op,
                    };
                    let got = books
                        .escrow
                        .record_withdrawal_pushed(id, push.clone())
                        .unwrap();
                    match model.withdrawals.get_mut(&id) {
                        None => assert!(got.is_none(), "{ctx}: push invented a debit: {got:?}"),
                        Some(state) if state.pushed.is_some() => {
                            assert_eq!(
                                got.as_ref(),
                                Some(&*state),
                                "{ctx}: second push rewrote the first"
                            );
                        }
                        Some(state) => {
                            state.pushed = Some(push);
                            assert_eq!(got.as_ref(), Some(&*state), "{ctx}: push recorded wrong");
                        }
                    }
                }
                96..=97 => {
                    journal
                        .compact()
                        .unwrap_or_else(|e| panic!("{ctx}: compaction failed: {e}"));
                    assert_books_match_model(&path, &books, &model, &format!("{ctx} post-compact"));
                }
                _ => {
                    drop(books);
                    drop(journal);
                    let boot_compact = rng.coin();
                    (books, journal) = open_books(&path, boot_compact);
                    assert_books_match_model(&path, &books, &model, &format!("{ctx} post-restart"));
                }
            }

            // The cheap continuous differential: every derived number
            // the books arbitrate funds with, recomputed by the model
            // from scratch. `available` also asserts conservation.
            for key in buyer_keys.iter().chain([&probe_key]) {
                model.available(key, &ctx);
                assert_eq!(
                    books.escrow.organic_charged(key),
                    model.charged(key),
                    "{ctx}: charged diverges for {key}"
                );
                assert_eq!(
                    books.withdrawals.withdrawn(key),
                    model.withdrawn(key),
                    "{ctx}: withdrawn diverges for {key}"
                );
                assert_eq!(
                    books.accounts.deposited(key),
                    model.deposited(key),
                    "{ctx}: deposited diverges for {key}"
                );
            }
            let committed = model.bootstrap_committed();
            let ceiling = subsidy_ceiling(model.organic_released());
            assert!(
                committed <= ceiling,
                "{ctx}: subsidy overshoot: committed {committed} > ceiling {ceiling}"
            );

            if op % 12 == 0 {
                assert_books_match_model(&path, &books, &model, &ctx);
            }
        }
        assert_books_match_model(&path, &books, &model, &format!("seed {seed} end"));
    }
}

/// The `with_journal` restore path, partner ledger only.
fn open_partner_books(path: &Path, boot_compact: bool) -> (PartnerPayouts, Arc<Journal>) {
    let restored = Journal::load(path).expect("journal replays");
    let journal = Arc::new(Journal::open(path).expect("journal opens"));
    if boot_compact {
        journal.compact().expect("boot compaction");
    }
    let payouts = PartnerPayouts::restore(
        restored.partner_paid_totals,
        restored.partner_payout_ids,
        journal.clone(),
    );
    (payouts, journal)
}

fn assert_partner_books_match_model(
    path: &Path,
    paid: &BTreeMap<String, u64>,
    ids: &BTreeSet<String>,
    ctx: &str,
) {
    let restored = Journal::load(path).expect("journal replays");
    let model_totals: HashMap<String, u64> = paid.iter().map(|(k, v)| (k.clone(), *v)).collect();
    assert_eq!(
        restored.partner_paid_totals, model_totals,
        "{ctx}: replayed paid totals diverge"
    );
    let model_ids: HashSet<String> = ids.iter().cloned().collect();
    assert_eq!(
        restored.partner_payout_ids, model_ids,
        "{ctx}: replayed payout ids diverge"
    );
}

/// The rev-share outflow ledger, same treatment: random mark-paid
/// sequences — replayed references, stale accrual reads, restarts,
/// compactions — against a trivial model. The property that matters is
/// the high-water discipline: every accepted record was checked
/// against an accrual read that only grows, so the books can never
/// claim more partner money left than partners ever earned — even when
/// the operator's tooling retries, re-orders, or reads stale.
#[test]
fn random_partner_payout_sequences_never_outrun_accruals_across_restart_and_compaction() {
    let dir = tempfile::tempdir().unwrap();
    for seed in 0..20u64 {
        let path = dir.path().join(format!("partner-{seed}.jsonl"));
        let mut rng = Rng::new(0x9A47 ^ (seed << 8));

        let codes: Vec<String> = vec!["code-a".into(), "code-b".into()];
        let stranger = "code-unknown".to_string();
        // Small pools so replayed references land on both codes.
        let payout_ids: Vec<String> = (0..8).map(|i| format!("payout-{i}")).collect();

        let (mut payouts, mut journal) = open_partner_books(&path, false);
        let mut paid: BTreeMap<String, u64> = BTreeMap::new();
        let mut ids: BTreeSet<String> = BTreeSet::new();
        // What each code has actually earned — lives in the job
        // records outside this ledger, only ever grows, and every
        // mark-paid check runs against a read of it (sometimes stale,
        // which may only refuse more, never allow more).
        let mut accrued: BTreeMap<String, u64> = BTreeMap::new();

        for op in 0..110u64 {
            let ctx = format!("seed {seed} op {op}");
            match rng.below(100) {
                0..=24 => {
                    let code = rng.pick(&codes).clone();
                    *accrued.entry(code).or_default() += *rng.pick(&[500, 1_000, 3_000]);
                }
                // Record: exactly once per reference, refused past the
                // accrual it was checked against; a duplicate echoes
                // the passed code's total, whatever code the original
                // named.
                25..=79 => {
                    let id = rng.pick(&payout_ids).clone();
                    let code = if rng.below(8) == 0 {
                        stranger.clone()
                    } else {
                        rng.pick(&codes).clone()
                    };
                    let amount = *rng.pick(&[0, 250, 800, 2_000]);
                    let earned = accrued.get(&code).copied().unwrap_or(0);
                    let read = if rng.coin() { earned } else { earned / 2 };
                    let got = payouts.mark_paid(&id, &code, amount, read).unwrap();
                    let paid_now = paid.get(&code).copied().unwrap_or(0);
                    if ids.contains(&id) {
                        assert_eq!(
                            got,
                            PartnerPayoutOutcome::Duplicate {
                                paid_total_micro_usdc: paid_now,
                            },
                            "{ctx}: replayed reference moved money"
                        );
                    } else if paid_now.saturating_add(amount) > read {
                        assert_eq!(
                            got,
                            PartnerPayoutOutcome::ExceedsAccrued {
                                paid_micro_usdc: paid_now,
                            },
                            "{ctx}: payout past the accrual read allowed"
                        );
                    } else {
                        ids.insert(id);
                        let total = paid.entry(code).or_default();
                        *total += amount;
                        assert_eq!(
                            got,
                            PartnerPayoutOutcome::Recorded {
                                paid_total_micro_usdc: *total,
                            },
                            "{ctx}: record moved the wrong amount"
                        );
                    }
                }
                80..=87 => {
                    journal
                        .compact()
                        .unwrap_or_else(|e| panic!("{ctx}: compaction failed: {e}"));
                    assert_partner_books_match_model(
                        &path,
                        &paid,
                        &ids,
                        &format!("{ctx} post-compact"),
                    );
                }
                _ => {
                    drop(payouts);
                    drop(journal);
                    let boot_compact = rng.coin();
                    (payouts, journal) = open_partner_books(&path, boot_compact);
                    assert_partner_books_match_model(
                        &path,
                        &paid,
                        &ids,
                        &format!("{ctx} post-restart"),
                    );
                }
            }

            for code in codes.iter().chain([&stranger]) {
                let live = payouts.paid(code);
                assert_eq!(
                    live,
                    paid.get(code).copied().unwrap_or(0),
                    "{ctx}: paid total diverges for {code}"
                );
                assert!(
                    live <= accrued.get(code).copied().unwrap_or(0),
                    "{ctx}: {code} paid {live} past everything it ever accrued"
                );
            }

            if op % 12 == 0 {
                assert_partner_books_match_model(&path, &paid, &ids, &ctx);
            }
        }
        assert_partner_books_match_model(&path, &paid, &ids, &format!("seed {seed} end"));
    }
}
