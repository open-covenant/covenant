//! Model-based differential tests over the node's three durable books
//! — the earnings ledger, the result outbox, and the accepted-jobs
//! book — the supply-side sibling of the coordinator's books_model.rs
//! and bond_model.rs. The example-based tests in each module pin
//! single scenarios; these drive seeded-random op sequences with
//! reopens and crash-torn tails interleaved, checking after every step
//! that the book agrees with a deliberately-trivial reference model,
//! plus the properties that differ on purpose between the books and
//! that no single example can pin:
//!
//! - an earnings job id is refused forever — paid or unpaid, the
//!   payment dedupe — while an outbox or accepted id frees on settle:
//!   the coordinator's documented double-execution window can
//!   legitimately re-queue a job whose tombstone is already on disk,
//!   and the tombstone must not resurrect over the new booking;
//! - replay reconstructs exactly the live state — statuses, lives,
//!   amounts, signed payloads — through any interleaving of reopens
//!   and torn final lines;
//! - the file carries one line per fact: a no-op re-confirmation, a
//!   stranger settle, and a stranger life-bump append nothing, and
//!   boot compaction folds outbox/accepted history down to the live
//!   entries (the earnings ledger deliberately never compacts — its
//!   ids never free, its history IS the book).
//!
//! Deterministic on purpose: the same hand-rolled splitmix64 as the
//! coordinator models, so a failure names its seed and op index.

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;

use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency, A2ATaskStatus};
use covenant_compute_node::accepted::{AcceptedBook, AcceptedEntry, AcceptedError};
use covenant_compute_node::earnings::{
    EarningsEntry, EarningsError, EarningsLedger, EarningsStatus, JsonlEarningsLedger,
};
use covenant_compute_node::outbox::{OutboxEntry, OutboxError, ResultOutbox};
use covenant_compute_protocol::{
    CapabilityRequirement, EscrowHoldAttestation, FundingSource, JobEnvelopePayload, JobKind,
    JobMeter, JobResultMessage, SignedJobEnvelope, SignedWorkReceipt, WorkReceiptPayload,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
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

fn lines(path: &Path) -> usize {
    match std::fs::read_to_string(path) {
        Ok(s) => s.lines().filter(|l| !l.trim().is_empty()).count(),
        Err(_) => 0,
    }
}

/// A crash mid-append: trailing bytes with no newline. Every book must
/// drop and truncate these at open — the mutation they belonged to was
/// reported failed, so no model op ever recorded it.
fn torn_tail(path: &Path) {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    f.write_all(b"{\"job_id\":\"torn").unwrap();
}

#[tokio::test]
async fn random_earnings_sequences_conserve_the_operators_claim_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    for seed in 0..12u64 {
        let path = dir.path().join(format!("earnings-{seed}.jsonl"));
        let mut rng = Rng::new(0xEA51 ^ (seed << 8));
        let jobs: Vec<Uuid> = (0..8).map(|_| Uuid::new_v4()).collect();

        let mut ledger = JsonlEarningsLedger::open(&path).unwrap();
        // The model is the ledger's whole contract: entries in credit
        // order, updated in place by payout confirmations.
        let mut model: Vec<EarningsEntry> = Vec::new();
        let mut file_lines = 0usize;

        for op in 0..90u64 {
            let ctx = format!("seed {seed} op {op}");
            match rng.below(100) {
                // Credit: refused forever for a known job — paid or
                // unpaid — so a retried receipt submission can't
                // double the operator's claim.
                0..=39 => {
                    let job = *rng.pick(&jobs);
                    let entry = EarningsEntry {
                        job_id: job,
                        amount_micro_usdc: *rng.pick(&[0, 250, 1_000, 4_000]),
                        fee_micro_usdc: *rng.pick(&[0, 50]),
                        funding_source: if rng.coin() {
                            FundingSource::Organic
                        } else {
                            FundingSource::Bootstrap
                        },
                        status: EarningsStatus::Unpaid,
                        earned_at_ms: op,
                        paid_tx_signature: None,
                        paid_at_ms: None,
                        receipt_signature_b58: rng.coin().then(|| format!("receipt-sig-{op}")),
                    };
                    let got = ledger.credit(entry.clone()).await;
                    if model.iter().any(|e| e.job_id == job) {
                        assert!(
                            matches!(got, Err(EarningsError::AlreadyCredited(id)) if id == job),
                            "{ctx}: duplicate credit not refused: {got:?}"
                        );
                    } else {
                        got.unwrap_or_else(|e| panic!("{ctx}: fresh credit refused: {e}"));
                        model.push(entry);
                        file_lines += 1;
                    }
                }
                // Payout confirmation: flips once and pins the first
                // sighting; the reconcile loop's re-reads are no-ops
                // that journal nothing; a stranger is an error.
                40..=69 => {
                    let job = if rng.below(10) == 0 {
                        Uuid::new_v4()
                    } else {
                        *rng.pick(&jobs)
                    };
                    let sig = rng.coin().then(|| format!("tx-{op}"));
                    let got = ledger.mark_paid(job, sig.clone(), op).await;
                    match model.iter_mut().find(|e| e.job_id == job) {
                        None => assert!(
                            matches!(got, Err(EarningsError::NotFound(id)) if id == job),
                            "{ctx}: stranger payout accepted: {got:?}"
                        ),
                        Some(e) if e.status == EarningsStatus::Paid => assert!(
                            matches!(got, Ok(false)),
                            "{ctx}: re-confirmation flipped again: {got:?}"
                        ),
                        Some(e) => {
                            assert!(
                                matches!(got, Ok(true)),
                                "{ctx}: first confirmation refused: {got:?}"
                            );
                            e.status = EarningsStatus::Paid;
                            e.paid_tx_signature = sig;
                            e.paid_at_ms = Some(op);
                            file_lines += 1;
                        }
                    }
                }
                // Reopen, sometimes onto a crash-torn tail. Replay
                // must reconstruct the model exactly — order included:
                // credit order, payouts updated in place.
                _ => {
                    drop(ledger);
                    if rng.coin() {
                        torn_tail(&path);
                    }
                    ledger = JsonlEarningsLedger::open(&path).unwrap();
                }
            }

            let expect_unpaid: u64 = model
                .iter()
                .filter(|e| e.status == EarningsStatus::Unpaid)
                .map(|e| e.amount_micro_usdc)
                .sum();
            assert_eq!(
                ledger.unpaid_total_micro_usdc().await,
                expect_unpaid,
                "{ctx}: unpaid total diverges"
            );
            let newest_first: Vec<EarningsEntry> = model.iter().rev().cloned().collect();
            assert_eq!(
                ledger.recent(usize::MAX).await,
                newest_first,
                "{ctx}: entries diverge"
            );
            assert_eq!(
                ledger.recent(3).await,
                newest_first.iter().take(3).cloned().collect::<Vec<_>>(),
                "{ctx}: recent window diverges"
            );
            assert_eq!(
                lines(&path),
                file_lines,
                "{ctx}: the file must hold one line per credit and per \
                 first confirmation, nothing else"
            );
        }
    }
}

fn receipt_for(job_id: Uuid, operator: &LocalIdentity) -> SignedWorkReceipt {
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
            price_micro_usdc: 100,
            status: A2ATaskStatus::Ok,
            executed_at_ms: 1,
            node_audit_root_hex: "cc".repeat(32),
        },
        operator,
    )
    .expect("receipt signs")
}

#[test]
fn random_outbox_sequences_redeliver_exactly_the_results_still_owed() {
    let dir = tempfile::tempdir().unwrap();
    for seed in 0..12u64 {
        let path = dir.path().join(format!("outbox-{seed}.jsonl"));
        let mut rng = Rng::new(0x0B0C ^ (seed << 8));
        let operator = LocalIdentity::generate("operator@ledgers-model");
        let jobs: Vec<Uuid> = (0..6).map(|_| Uuid::new_v4()).collect();

        let mut outbox = ResultOutbox::open(&path).unwrap();
        // Live queue only: a settled result is disposed of, and its id
        // may legitimately queue again.
        let mut live: BTreeMap<Uuid, u64> = BTreeMap::new();
        let mut file_lines = 0usize;

        for op in 0..90u64 {
            let ctx = format!("seed {seed} op {op}");
            match rng.below(100) {
                // Enqueue: one live entry per job; an id whose
                // tombstone is on disk queues again cleanly.
                0..=39 => {
                    let job = *rng.pick(&jobs);
                    let entry = OutboxEntry {
                        job_id: job,
                        message: JobResultMessage {
                            receipt: receipt_for(job, &operator),
                            output: vec![Content::text("queued work")],
                        },
                        funding_source: FundingSource::Organic,
                        queued_at_ms: op,
                        settled: false,
                    };
                    let got = outbox.enqueue(entry);
                    match live.entry(job) {
                        Entry::Occupied(_) => assert!(
                            matches!(got, Err(OutboxError::AlreadyQueued(id)) if id == job),
                            "{ctx}: double queue not refused: {got:?}"
                        ),
                        Entry::Vacant(slot) => {
                            got.unwrap_or_else(|e| panic!("{ctx}: queue refused: {e}"));
                            slot.insert(op);
                            file_lines += 1;
                        }
                    }
                }
                // Settle: tombstones a queued result; a stranger is a
                // no-op that journals nothing (a drain racing a settle
                // must not fail).
                40..=69 => {
                    let job = if rng.below(8) == 0 {
                        Uuid::new_v4()
                    } else {
                        *rng.pick(&jobs)
                    };
                    outbox
                        .settle(job)
                        .unwrap_or_else(|e| panic!("{ctx}: settle failed: {e}"));
                    if live.remove(&job).is_some() {
                        file_lines += 1;
                    }
                }
                // Reopen, sometimes onto a crash-torn tail. Boot
                // compaction folds the file down to the results still
                // owed, and every survivor's signed receipt must still
                // verify — bytes intact, not just ids.
                _ => {
                    drop(outbox);
                    if rng.coin() {
                        torn_tail(&path);
                    }
                    outbox = ResultOutbox::open(&path).unwrap();
                    if file_lines > live.len() {
                        file_lines = live.len();
                    }
                    for entry in outbox.pending() {
                        entry
                            .message
                            .receipt
                            .verify()
                            .unwrap_or_else(|e| panic!("{ctx}: receipt corrupted: {e}"));
                        assert_eq!(
                            entry.message.receipt.receipt.job_id, entry.job_id,
                            "{ctx}: receipt swapped onto another entry"
                        );
                    }
                }
            }

            let got: BTreeMap<Uuid, u64> = outbox
                .pending()
                .into_iter()
                .inspect(|e| assert!(!e.settled, "{ctx}: a settled entry stayed pending"))
                .map(|e| (e.job_id, e.queued_at_ms))
                .collect();
            assert_eq!(got, live, "{ctx}: pending queue diverges");
            assert_eq!(
                lines(&path),
                file_lines,
                "{ctx}: the file must hold one line per queue and per \
                 tombstone, folded to the live entries at boot"
            );
        }
    }
}

fn accepted_entry(
    job_id: Uuid,
    buyer: &LocalIdentity,
    coordinator: &LocalIdentity,
    accepted_at_ms: u64,
) -> AcceptedEntry {
    let envelope = SignedJobEnvelope::sign(
        JobEnvelopePayload {
            job_id,
            buyer: buyer.agent_id(),
            kind: JobKind::BatchJob,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: None,
                kind: JobKind::BatchJob,
                max_duration_secs: 30,
                min_reputation_bps: None,
            },
            input: vec![Content::text("work worth remembering")],
            price_micro_usdc: 5_000,
            deadline_ms: 30_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "ledgers-model"),
            issued_at_ms: 1,
            referral_code: None,
            stream: false,
        },
        buyer,
    )
    .expect("envelope signs");
    let escrow_hold =
        EscrowHoldAttestation::sign(job_id, 5_000, FundingSource::Organic, 1, coordinator)
            .expect("attestation signs");
    AcceptedEntry {
        job_id,
        envelope,
        escrow_hold,
        accepted_at_ms,
        lives: 1,
        settled: false,
    }
}

#[test]
fn random_accepted_sequences_recover_exactly_the_unfinished_jobs() {
    let dir = tempfile::tempdir().unwrap();
    for seed in 0..12u64 {
        let path = dir.path().join(format!("accepted-{seed}.jsonl"));
        let mut rng = Rng::new(0xACC ^ (seed << 8));
        let buyer = LocalIdentity::generate("buyer@ledgers-model");
        let coordinator = LocalIdentity::generate("coordinator@ledgers-model");
        let jobs: Vec<Uuid> = (0..6).map(|_| Uuid::new_v4()).collect();

        let mut book = AcceptedBook::open(&path).unwrap();
        // Live bookings only: job id -> (accepted_at_ms, lives). A
        // settled job's id may book again — a fresh obligation with
        // fresh lives, not a resurrection of the tombstone.
        let mut live: BTreeMap<Uuid, (u64, u32)> = BTreeMap::new();
        let mut file_lines = 0usize;

        for op in 0..90u64 {
            let ctx = format!("seed {seed} op {op}");
            match rng.below(100) {
                0..=34 => {
                    let job = *rng.pick(&jobs);
                    let got = book.book(accepted_entry(job, &buyer, &coordinator, op));
                    match live.entry(job) {
                        Entry::Occupied(_) => assert!(
                            matches!(got, Err(AcceptedError::AlreadyBooked(id)) if id == job),
                            "{ctx}: double booking not refused: {got:?}"
                        ),
                        Entry::Vacant(slot) => {
                            got.unwrap_or_else(|e| panic!("{ctx}: booking refused: {e}"));
                            slot.insert((op, 1));
                            file_lines += 1;
                        }
                    }
                }
                // The crash-loop guard: a recovery run bumps the life
                // count durably before executing; a stranger is a
                // no-op that journals nothing.
                35..=54 => {
                    let job = if rng.below(8) == 0 {
                        Uuid::new_v4()
                    } else {
                        *rng.pick(&jobs)
                    };
                    book.record_life(job)
                        .unwrap_or_else(|e| panic!("{ctx}: life bump failed: {e}"));
                    if let Some((_, lives)) = live.get_mut(&job) {
                        *lives += 1;
                        file_lines += 1;
                    }
                }
                55..=74 => {
                    let job = if rng.below(8) == 0 {
                        Uuid::new_v4()
                    } else {
                        *rng.pick(&jobs)
                    };
                    book.settle(job)
                        .unwrap_or_else(|e| panic!("{ctx}: settle failed: {e}"));
                    if live.remove(&job).is_some() {
                        file_lines += 1;
                    }
                }
                // Reopen, sometimes onto a crash-torn tail. Whatever
                // is still booked is a job a previous life accepted
                // and never finished — exactly the model's live set,
                // with the life counts and the signed work order
                // intact.
                _ => {
                    drop(book);
                    if rng.coin() {
                        torn_tail(&path);
                    }
                    book = AcceptedBook::open(&path).unwrap();
                    if file_lines > live.len() {
                        file_lines = live.len();
                    }
                    for entry in book.pending() {
                        entry
                            .envelope
                            .verify()
                            .unwrap_or_else(|e| panic!("{ctx}: envelope corrupted: {e}"));
                        assert_eq!(
                            entry.envelope.payload.job_id, entry.job_id,
                            "{ctx}: work order swapped onto another booking"
                        );
                    }
                }
            }

            let got: BTreeMap<Uuid, (u64, u32)> = book
                .pending()
                .into_iter()
                .inspect(|e| assert!(!e.settled, "{ctx}: a settled booking stayed pending"))
                .map(|e| (e.job_id, (e.accepted_at_ms, e.lives)))
                .collect();
            assert_eq!(got, live, "{ctx}: unfinished bookings diverge");
            assert_eq!(
                lines(&path),
                file_lines,
                "{ctx}: the file must hold one line per booking, life \
                 and tombstone, folded to the live entries at boot"
            );
        }
    }
}
