//! Model-based differential tests over the operator bond book — the
//! books_model.rs treatment for the supply side's money. The
//! example-based tests in `bond.rs` pin each transition once; these
//! drive seeded-random sequences of rail-verified posts, fault
//! slashes, unbond requests and sweep-shaped refund pushes against
//! the real journaled book — the same `restore` wiring
//! `CoordinatorState::with_journal` builds — and check after every
//! step that it agrees with a deliberately-trivial reference model,
//! plus the algebra no single example can pin:
//!
//! - outflow conservation: slashed plus refunded never exceeds
//!   posted, across any interleaving of slashes landing while unbonds
//!   mature (the invariant `record_refunded`'s reconciliation alarm
//!   watches for);
//! - the clamp chain: a slash takes at most what is standing, a
//!   matured refund pays at most what is left after every slash that
//!   landed during maturation, and a new request draws only committed
//!   stake — while a pending request shields nothing from a slash;
//! - `Journal::load` reconstructs the book exactly at any point, with
//!   restarts and compactions interleaved into the sequence.
//!
//! Refund pushes here follow `sweep_matured_unbonds`' exact shape —
//! re-read `payable` at push time, pin that amount verbatim, close a
//! slashed-to-nothing request with a zero-payment push — because the
//! sweep is the only pusher in the product.
//!
//! Deterministic on purpose: the same hand-rolled splitmix64 as the
//! buyer-books model, so a failure names its seed and op index and
//! reproduces byte-for-byte.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use covenant_compute_coordinator::bond::{
    BondPostOutcome, BondRefundPush, BondStatus, OperatorBonds, SlashOutcome, SlashRecord,
    UnbondOutcome, UnbondState,
};
use covenant_compute_coordinator::Journal;
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

#[derive(Default)]
struct BondModel {
    posted: BTreeMap<String, u64>,
    post_ids: BTreeSet<String>,
    slashes: BTreeMap<String, SlashRecord>,
    unbonds: BTreeMap<Uuid, UnbondState>,
}

impl BondModel {
    fn posted_for(&self, operator: &str) -> u64 {
        self.posted.get(operator).copied().unwrap_or(0)
    }

    /// The stake algebra from `bond.rs`' module doc, restated
    /// independently so a drift in the book's arithmetic fails against
    /// this, not against itself. Conservation is asserted here, on
    /// every read: with sweep-shaped pushes, outflow can never exceed
    /// what was posted.
    fn status(&self, operator: &str, ctx: &str) -> BondStatus {
        let posted = self.posted_for(operator);
        let slashed = self
            .slashes
            .values()
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
        assert!(
            slashed + refunded <= posted,
            "{ctx}: operator {operator} outflow exceeds stake: \
             slashed {slashed} + refunded {refunded} > posted {posted}"
        );
        let at_stake = posted - slashed - refunded;
        BondStatus {
            posted_micro_usdc: posted,
            slashed_micro_usdc: slashed,
            unbonding_micro_usdc: unbonding,
            refunded_micro_usdc: refunded,
            at_stake_micro_usdc: at_stake,
            committed_micro_usdc: at_stake.saturating_sub(unbonding),
        }
    }

    fn payable(&self, unbond_id: Uuid, now_ms: u64, ctx: &str) -> Option<u64> {
        let unbond = self.unbonds.get(&unbond_id)?;
        if unbond.pushed.is_some() || now_ms < unbond.matures_at_ms {
            return None;
        }
        let standing = self
            .status(&unbond.operator_pubkey_b58, ctx)
            .at_stake_micro_usdc;
        Some(unbond.amount_micro_usdc.min(standing))
    }

    fn matured_unpushed_ids(&self, now_ms: u64) -> BTreeSet<Uuid> {
        self.unbonds
            .values()
            .filter(|u| u.pushed.is_none() && now_ms >= u.matures_at_ms)
            .map(|u| u.unbond_id)
            .collect()
    }
}

/// The `with_journal` restore path, bond book only: load, open,
/// optional boot compaction, then seed the book from the loaded state.
fn open_bonds(path: &Path, boot_compact: bool) -> (OperatorBonds, Arc<Journal>) {
    let restored = Journal::load(path).expect("journal replays");
    let journal = Arc::new(Journal::open(path).expect("journal opens"));
    if boot_compact {
        journal.compact().expect("boot compaction");
    }
    let bonds = OperatorBonds::restore(
        restored.bond_totals,
        restored.bond_ids,
        restored.bond_slashes,
        restored.unbonds,
        journal.clone(),
    );
    (bonds, journal)
}

fn assert_bonds_match_model(path: &Path, model: &BondModel, ctx: &str) {
    let restored = Journal::load(path).expect("journal replays");
    let model_totals: HashMap<String, u64> =
        model.posted.iter().map(|(k, v)| (k.clone(), *v)).collect();
    assert_eq!(
        restored.bond_totals, model_totals,
        "{ctx}: replayed posted totals diverge"
    );
    let model_ids: HashSet<String> = model.post_ids.iter().cloned().collect();
    assert_eq!(
        restored.bond_ids, model_ids,
        "{ctx}: replayed post ids diverge"
    );
    // Compaction rewrites slashes keyed by id, so replay order is not
    // append order; nothing the book derives depends on it either.
    let mut replayed = restored.bond_slashes;
    replayed.sort_by(|a, b| a.slash_id.cmp(&b.slash_id));
    let expected: Vec<SlashRecord> = model.slashes.values().cloned().collect();
    assert_eq!(replayed, expected, "{ctx}: replayed slashes diverge");
    let model_unbonds: HashMap<Uuid, UnbondState> =
        model.unbonds.iter().map(|(k, v)| (*k, v.clone())).collect();
    assert_eq!(
        restored.unbonds, model_unbonds,
        "{ctx}: replayed unbonds diverge"
    );
}

#[test]
fn random_bond_op_sequences_conserve_operator_stake_across_restart_and_compaction() {
    let dir = tempfile::tempdir().unwrap();
    for seed in 0..20u64 {
        let path = dir.path().join(format!("bonds-{seed}.jsonl"));
        let mut rng = Rng::new(0xB01D ^ (seed << 8));

        let operators: Vec<String> = vec![
            "operator-a@bond-model".into(),
            "operator-b@bond-model".into(),
        ];
        let stranger = "operator-unknown@bond-model".to_string();

        // Small pools so collisions — replayed post ids, replayed
        // verdicts, duplicate unbond requests, cross-operator id reuse
        // — actually happen instead of staying theoretical.
        let bond_ids: Vec<String> = (0..5).map(|i| format!("bond-{i}")).collect();
        let slash_ids: Vec<String> = (0..7).map(|i| format!("slash-{i}")).collect();
        let unbond_ids: Vec<Uuid> = (0..6).map(|_| Uuid::new_v4()).collect();
        let jobs: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
        let reasons = ["canary wrong-answer", "redundancy minority"];

        let (mut bonds, mut journal) = open_bonds(&path, false);
        let mut model = BondModel::default();

        for op in 0..130u64 {
            let now = op;
            let ctx = format!("seed {seed} op {op}");
            match rng.below(100) {
                // Post: idempotent by id; a duplicate echoes the total
                // of whichever operator the replay names.
                0..=17 => {
                    let id = rng.pick(&bond_ids).clone();
                    let operator = rng.pick(&operators).clone();
                    let amount = *rng.pick(&[0, 500, 1_000, 2_500, 10_000]);
                    let got = bonds.credit_post(&id, &operator, amount).unwrap();
                    if model.post_ids.contains(&id) {
                        assert_eq!(
                            got,
                            BondPostOutcome::Duplicate {
                                posted_total_micro_usdc: model.posted_for(&operator),
                            },
                            "{ctx}: duplicate post moved stake"
                        );
                    } else {
                        model.post_ids.insert(id);
                        *model.posted.entry(operator.clone()).or_default() += amount;
                        assert_eq!(
                            got,
                            BondPostOutcome::Credited {
                                posted_total_micro_usdc: model.posted_for(&operator),
                            },
                            "{ctx}: post credited wrong"
                        );
                    }
                }
                // Slash: exactly once per verdict id, takes at most
                // what is standing — pending unbonds shield nothing.
                18..=37 => {
                    let id = rng.pick(&slash_ids).clone();
                    let operator = rng.pick(&operators).clone();
                    let amount = *rng.pick(&[0, 1, 300, 700, 1_500, 5_000]);
                    let job = *rng.pick(&jobs);
                    let reason = *rng.pick(&reasons);
                    let got = bonds
                        .slash(&id, &operator, amount, job, reason, now)
                        .unwrap();
                    if model.slashes.contains_key(&id) {
                        assert_eq!(
                            got,
                            SlashOutcome::Duplicate,
                            "{ctx}: replayed verdict moved stake"
                        );
                    } else {
                        let standing = model.status(&operator, &ctx).at_stake_micro_usdc;
                        let take = amount.min(standing);
                        if take == 0 {
                            // Nothing standing (or nothing asked): no
                            // fact, so the journal must not grow one.
                            assert_eq!(
                                got,
                                SlashOutcome::NoStake,
                                "{ctx}: zero take recorded a slash"
                            );
                        } else {
                            assert_eq!(
                                got,
                                SlashOutcome::Slashed {
                                    amount_micro_usdc: take,
                                    at_stake_micro_usdc: standing - take,
                                },
                                "{ctx}: slash took the wrong amount"
                            );
                            model.slashes.insert(
                                id.clone(),
                                SlashRecord {
                                    slash_id: id,
                                    operator_pubkey_b58: operator,
                                    amount_micro_usdc: take,
                                    job_id: job,
                                    reason: reason.into(),
                                    slashed_at_ms: now,
                                },
                            );
                        }
                    }
                }
                // Unbond request: draws committed stake only, exactly
                // once per id.
                38..=56 => {
                    let id = *rng.pick(&unbond_ids);
                    let operator = rng.pick(&operators).clone();
                    let amount = *rng.pick(&[0, 100, 400, 1_200, 3_000]);
                    let state = UnbondState {
                        unbond_id: id,
                        operator_pubkey_b58: operator.clone(),
                        recipient_address_b58: "recipient@bond-model".into(),
                        amount_micro_usdc: amount,
                        requested_at_ms: now,
                        matures_at_ms: now + *rng.pick(&[0, 3, 15, 500]),
                        pushed: None,
                    };
                    let got = bonds.request_unbond(state.clone()).unwrap();
                    match model.unbonds.get(&id) {
                        Some(existing) => assert_eq!(
                            got,
                            UnbondOutcome::Duplicate(existing.clone()),
                            "{ctx}: duplicate request rewrote the obligation"
                        ),
                        None => {
                            let committed = model.status(&operator, &ctx).committed_micro_usdc;
                            if amount > committed {
                                assert_eq!(
                                    got,
                                    UnbondOutcome::Insufficient {
                                        committed_micro_usdc: committed,
                                    },
                                    "{ctx}: over-committed request allowed"
                                );
                            } else {
                                assert_eq!(
                                    got,
                                    UnbondOutcome::Requested(state.clone()),
                                    "{ctx}: covered request refused"
                                );
                                model.unbonds.insert(id, state);
                            }
                        }
                    }
                }
                // The retry sweep's shape: re-read what one matured
                // request pays NOW, pin exactly that. A request
                // slashed to nothing closes with a zero-payment push.
                57..=73 => {
                    let matured: Vec<Uuid> = model.matured_unpushed_ids(now).into_iter().collect();
                    if matured.is_empty() {
                        continue;
                    }
                    let id = *rng.pick(&matured);
                    let payable = bonds
                        .payable(id, now)
                        .unwrap_or_else(|| panic!("{ctx}: matured request not payable"));
                    assert_eq!(
                        Some(payable),
                        model.payable(id, now, &ctx),
                        "{ctx}: payable diverges at push time"
                    );
                    let push = BondRefundPush {
                        tx_signature: (payable > 0).then(|| format!("tx-{op}")),
                        recorded_at_ms: now,
                        paid_micro_usdc: payable,
                    };
                    let got = bonds
                        .record_refunded(id, push.clone())
                        .unwrap()
                        .unwrap_or_else(|| panic!("{ctx}: known request vanished"));
                    model.unbonds.get_mut(&id).unwrap().pushed = Some(push);
                    assert_eq!(
                        &got,
                        model.unbonds.get(&id).unwrap(),
                        "{ctx}: push recorded wrong"
                    );
                }
                // A replayed push keeps the first; an unknown id is
                // never invented into a fact.
                74..=79 => {
                    let pushed: Vec<Uuid> = model
                        .unbonds
                        .values()
                        .filter(|u| u.pushed.is_some())
                        .map(|u| u.unbond_id)
                        .collect();
                    if pushed.is_empty() || rng.coin() {
                        let got = bonds
                            .record_refunded(
                                Uuid::new_v4(),
                                BondRefundPush {
                                    tx_signature: None,
                                    recorded_at_ms: now,
                                    paid_micro_usdc: 0,
                                },
                            )
                            .unwrap();
                        assert!(got.is_none(), "{ctx}: push invented a request: {got:?}");
                    } else {
                        let id = *rng.pick(&pushed);
                        let got = bonds
                            .record_refunded(
                                id,
                                BondRefundPush {
                                    tx_signature: Some(format!("tx-replay-{op}")),
                                    recorded_at_ms: now,
                                    paid_micro_usdc: 12_345,
                                },
                            )
                            .unwrap()
                            .unwrap_or_else(|| panic!("{ctx}: pushed request vanished"));
                        assert_eq!(
                            &got,
                            model.unbonds.get(&id).unwrap(),
                            "{ctx}: second push rewrote the first"
                        );
                    }
                }
                80..=84 => {
                    journal
                        .compact()
                        .unwrap_or_else(|e| panic!("{ctx}: compaction failed: {e}"));
                    assert_bonds_match_model(&path, &model, &format!("{ctx} post-compact"));
                }
                85..=92 => {
                    drop(bonds);
                    drop(journal);
                    let boot_compact = rng.coin();
                    (bonds, journal) = open_bonds(&path, boot_compact);
                    assert_bonds_match_model(&path, &model, &format!("{ctx} post-restart"));
                }
                // Maturity-boundary probe at an arbitrary clock, past
                // or future.
                _ => {
                    let id = *rng.pick(&unbond_ids);
                    let probe_at = now.saturating_sub(5) + rng.below(40);
                    assert_eq!(
                        bonds.payable(id, probe_at),
                        model.payable(id, probe_at, &ctx),
                        "{ctx}: payable diverges at clock {probe_at}"
                    );
                }
            }

            // The cheap continuous differential: every derived number
            // a consumer reads — the matcher's floor, the sweep's
            // worklist, the operator's own standing view — recomputed
            // by the model from scratch. `status` also asserts
            // conservation.
            for operator in operators.iter().chain([&stranger]) {
                assert_eq!(
                    bonds.status(operator),
                    model.status(operator, &ctx),
                    "{ctx}: status diverges for {operator}"
                );
            }
            for id in &unbond_ids {
                assert_eq!(
                    bonds.payable(*id, now),
                    model.payable(*id, now, &ctx),
                    "{ctx}: payable diverges for {id}"
                );
            }
            let live_matured: BTreeSet<Uuid> = bonds
                .matured_unpushed(now)
                .into_iter()
                .map(|u| u.unbond_id)
                .collect();
            assert_eq!(
                live_matured,
                model.matured_unpushed_ids(now),
                "{ctx}: matured worklist diverges"
            );

            if op % 12 == 0 {
                assert_bonds_match_model(&path, &model, &ctx);
            }
        }
        assert_bonds_match_model(&path, &model, &format!("seed {seed} end"));
    }
}
