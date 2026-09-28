//! Model-based differential test over the buyer's purchase book — the
//! demand-side sibling of the node's ledgers_model.rs and the
//! coordinator's books_model.rs. The unit tests in `purchases.rs` pin
//! single scenarios; this drives seeded-random sequences of records,
//! settles, voids and reopens — crash-torn tails included — against
//! the real file-backed book, checking after every step that it agrees
//! with a trivial reference model, plus the key lifecycle no single
//! example pins across arbitrary interleavings:
//!
//! - a key is held by AT MOST ONE purchase: recording is refused while
//!   the key is in flight or settled (that refusal is what keeps a
//!   crashed buyer's retry from paying twice), and only a void — an
//!   unpaid conclusion — frees it for an honest re-buy;
//! - a re-recorded key serves its NEW signed envelope: the voided
//!   tombstone on disk never resurrects the old work order over it;
//! - the file carries one line per fact — stranger settles and voids
//!   append nothing — and boot compaction folds the history back to
//!   one line per live key.
//!
//! Deterministic on purpose: the same hand-rolled splitmix64 as the
//! other book models, so a failure names its seed and op index.

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;

use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
use covenant_compute_buyer::{PurchaseBook, PurchaseEntry, PurchaseError};
use covenant_compute_protocol::{
    CapabilityRequirement, JobEnvelopePayload, JobKind, SignedJobEnvelope,
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

fn torn_tail(path: &Path) {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    f.write_all(b"{\"key\":\"torn").unwrap();
}

fn purchase(key: &str, buyer: &LocalIdentity, opened_at_ms: u64) -> PurchaseEntry {
    let job_id = Uuid::new_v4();
    let envelope = SignedJobEnvelope::sign(
        JobEnvelopePayload {
            job_id,
            buyer: buyer.agent_id(),
            kind: JobKind::InferenceCall,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: None,
                kind: JobKind::InferenceCall,
                max_duration_secs: 30,
                min_reputation_bps: None,
            },
            input: vec![Content::text("one intent, one payment")],
            price_micro_usdc: 1_000,
            deadline_ms: 30_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "purchases-model"),
            issued_at_ms: 1,
            referral_code: None,
            stream: false,
        },
        buyer,
    )
    .expect("envelope signs");
    PurchaseEntry {
        key: key.into(),
        envelope,
        opened_at_ms,
        receipt_id: None,
        voided: false,
    }
}

/// What the model remembers per live key: when it opened, WHICH signed
/// work order it holds (a re-recorded key must serve its new envelope,
/// not the tombstoned one), and the settle receipt if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Held {
    opened_at_ms: u64,
    envelope_job_id: Uuid,
    receipt_id: Option<Uuid>,
}

#[test]
fn random_purchase_sequences_pay_once_per_key_until_voided_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    for seed in 0..12u64 {
        let path = dir.path().join(format!("purchases-{seed}.jsonl"));
        let mut rng = Rng::new(0xB0DE ^ (seed << 8));
        let buyer = LocalIdentity::generate("buyer@purchases-model");
        let keys: Vec<String> = (0..6).map(|i| format!("key-{i}")).collect();

        let mut book = PurchaseBook::open(&path).unwrap();
        let mut live: BTreeMap<String, Held> = BTreeMap::new();
        let mut file_lines = 0usize;

        for op in 0..90u64 {
            let ctx = format!("seed {seed} op {op}");
            match rng.below(100) {
                // Record: one purchase per key — in flight or settled,
                // the key refuses — and a voided key honestly re-buys
                // under a fresh envelope.
                0..=34 => {
                    let key = rng.pick(&keys).clone();
                    let entry = purchase(&key, &buyer, op);
                    let job_id = entry.envelope.payload.job_id;
                    let got = book.record(entry);
                    match live.entry(key.clone()) {
                        Entry::Occupied(_) => assert!(
                            matches!(&got, Err(PurchaseError::InFlight(k)) if *k == key),
                            "{ctx}: a held key took a second purchase: {got:?}"
                        ),
                        Entry::Vacant(slot) => {
                            got.unwrap_or_else(|e| panic!("{ctx}: fresh record refused: {e}"));
                            slot.insert(Held {
                                opened_at_ms: op,
                                envelope_job_id: job_id,
                                receipt_id: None,
                            });
                            file_lines += 1;
                        }
                    }
                }
                // Settle: the spend booked, the key answers replays
                // from here on. A stranger settle journals nothing.
                35..=54 => {
                    let key = if rng.below(8) == 0 {
                        format!("stranger-{op}")
                    } else {
                        rng.pick(&keys).clone()
                    };
                    let receipt_id = Uuid::new_v4();
                    book.settle(&key, receipt_id)
                        .unwrap_or_else(|e| panic!("{ctx}: settle failed: {e}"));
                    if let Some(held) = live.get_mut(&key) {
                        held.receipt_id = Some(receipt_id);
                        file_lines += 1;
                    }
                }
                // Void: the job concluded unpaid, the key frees. A
                // stranger void journals nothing.
                55..=69 => {
                    let key = if rng.below(8) == 0 {
                        format!("stranger-{op}")
                    } else {
                        rng.pick(&keys).clone()
                    };
                    book.void(&key)
                        .unwrap_or_else(|e| panic!("{ctx}: void failed: {e}"));
                    if live.remove(&key).is_some() {
                        file_lines += 1;
                    }
                }
                // Reopen, sometimes onto a crash-torn tail. Boot
                // compaction folds the file to the live keys, and each
                // survivor still carries its verifiable work order.
                _ => {
                    drop(book);
                    if rng.coin() {
                        torn_tail(&path);
                    }
                    book = PurchaseBook::open(&path).unwrap();
                    if file_lines > live.len() {
                        file_lines = live.len();
                    }
                    for key in &keys {
                        if let Some(entry) = book.lookup(key) {
                            entry
                                .envelope
                                .verify()
                                .unwrap_or_else(|e| panic!("{ctx}: envelope corrupted: {e}"));
                        }
                    }
                }
            }

            for key in &keys {
                let got = book.lookup(key).map(|e| {
                    assert!(!e.voided, "{ctx}: a voided purchase answered for {key}");
                    assert_eq!(&e.key, key, "{ctx}: lookup answered the wrong key");
                    assert_eq!(
                        e.settled(),
                        e.receipt_id.is_some(),
                        "{ctx}: settled() disagrees with the receipt"
                    );
                    Held {
                        opened_at_ms: e.opened_at_ms,
                        envelope_job_id: e.envelope.payload.job_id,
                        receipt_id: e.receipt_id,
                    }
                });
                assert_eq!(
                    got,
                    live.get(key).copied(),
                    "{ctx}: purchase under {key} diverges"
                );
            }
            assert_eq!(
                lines(&path),
                file_lines,
                "{ctx}: the file must hold one line per record, settle \
                 and void, folded to the live keys at boot"
            );
        }
    }
}
