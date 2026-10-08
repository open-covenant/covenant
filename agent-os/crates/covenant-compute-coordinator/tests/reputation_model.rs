//! Model-based differential test over reputation derivation — the
//! books_model.rs treatment for the last derived-numbers surface, the
//! one that is not a book at all: [`AuditReputationSource`] re-derives
//! every operator's standing from whatever audit rows currently
//! survive, and the matcher turns that number into an exclusion gate.
//!
//! The unit tests in `reputation.rs` pin each row kind once, with
//! symmetric counts (one passed + one failed canary, one agreed + one
//! disagreed sample) — symmetric enough that reading a polarity field
//! backwards moves nothing they assert. This drives random MIXED
//! histories — releases, attributed and orphaned refunds, canary
//! probes, disputes, redundancy verdicts, no-signal compute rows,
//! purges that forget a prefix of history — against a trivial fold,
//! for several operators sharing one log, so asymmetric counts make
//! every polarity and attribution arm load-bearing:
//!
//! - only concluded, attributed outcomes count: offers, admissions,
//!   completions, payout pushes and orphan refunds move nothing;
//! - the score is exactly `smoothed_score_bps(released, faults)` over
//!   the SURVIVING rows — purging history forgets it, and forgetting
//!   restores neutrality instead of sticking at the old score;
//! - the matcher admits at `score >= floor`, exactly at the boundary —
//!   a floor set to an operator's own score keeps it matchable, so a
//!   `<` misread as `<=` is a real exclusion bug the fixed-score unit
//!   tests never probe.
//!
//! Deterministic on purpose: the same hand-rolled splitmix64 as the
//! other book models, so a failure names its seed and op index.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use covenant_audit::{
    AuditError, AuditEvent, AuditIntegrityReport, AuditKind, AuditLog, InMemoryAuditLog,
};
use covenant_compute_coordinator::{
    select_operator, smoothed_score_bps, AuditReputationSource, BondFloor, OperatorBonds,
    OperatorRegistry, ReputationSource, ReputationStats,
};
use covenant_compute_protocol::{
    CapabilityProfile, CapabilityRequirement, HardwareClass, JobKind, PriceAsk, PriceUnit,
    RegisterRequest,
};
use covenant_identity::LocalIdentity;
use covenant_types::AgentId;
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
}

/// One audit row's reputation meaning, keyed by operator index. The
/// model never stores counters — it re-folds the surviving rows on
/// every probe, exactly the recomputation the source itself claims to
/// do, so purge semantics fall out instead of being modeled twice.
#[derive(Clone)]
enum Fact {
    Released(usize),
    AttributedRefund(usize),
    OrphanRefund,
    Canary(usize, bool),
    Disputed(usize),
    Redundancy(usize, Option<bool>),
    /// A compute-family row that must carry no reputation signal at
    /// all: 0 offered, 1 admitted, 2 completed, 3 payout-pushed.
    NoSignal(usize, u8),
}

fn kind_for(fact: &Fact, operators: &[String]) -> AuditKind {
    match fact {
        Fact::Released(i) => AuditKind::ComputeJobReleased {
            job_id: Uuid::new_v4(),
            operator_pubkey_b58: operators[*i].clone(),
            amount_micro_usdc: 100,
            funding_source: "organic".into(),
        },
        Fact::AttributedRefund(i) => AuditKind::ComputeJobRefunded {
            job_id: Uuid::new_v4(),
            reason: "execution_failed".into(),
            operator_pubkey_b58: Some(operators[*i].clone()),
        },
        Fact::OrphanRefund => AuditKind::ComputeJobRefunded {
            job_id: Uuid::new_v4(),
            reason: "admission_failed".into(),
            operator_pubkey_b58: None,
        },
        Fact::Canary(i, passed) => AuditKind::ComputeCanaryResult {
            job_id: Uuid::new_v4(),
            operator_pubkey_b58: operators[*i].clone(),
            passed: *passed,
            detail: "probe".into(),
        },
        Fact::Disputed(i) => AuditKind::ComputeJobDisputed {
            job_id: Uuid::new_v4(),
            operator_pubkey_b58: operators[*i].clone(),
            buyer_pubkey_b58: "buyer-pubkey".into(),
            reason: "output was unrelated to the prompt".into(),
        },
        Fact::Redundancy(i, agreed) => AuditKind::ComputeRedundancyResult {
            source_job_id: Uuid::new_v4(),
            operator_pubkey_b58: operators[*i].clone(),
            agreed: *agreed,
            detail: "sample".into(),
        },
        Fact::NoSignal(i, which) => {
            let operator_pubkey_b58 = operators[*i].clone();
            match which {
                0 => AuditKind::ComputeJobOffered {
                    job_id: Uuid::new_v4(),
                    operator_pubkey_b58,
                    price_micro_usdc: 100,
                    funding_source: "organic".into(),
                },
                1 => AuditKind::ComputeJobAdmitted {
                    job_id: Uuid::new_v4(),
                    operator_pubkey_b58,
                    passed: true,
                    reason: "capability satisfied".into(),
                },
                2 => AuditKind::ComputeJobCompleted {
                    job_id: Uuid::new_v4(),
                    operator_pubkey_b58,
                    status: "ok".into(),
                    result_hash_hex: "aa".into(),
                    price_micro_usdc: 100,
                },
                _ => AuditKind::ComputePayoutPushed {
                    job_id: Uuid::new_v4(),
                    operator_pubkey_b58,
                    amount_micro_usdc: 100,
                    tx_signature: None,
                },
            }
        }
    }
}

fn random_fact(rng: &mut Rng, operator_count: usize) -> Fact {
    let i = rng.below(operator_count as u64) as usize;
    match rng.below(12) {
        0..=2 => Fact::Released(i),
        3..=4 => Fact::AttributedRefund(i),
        5 => Fact::OrphanRefund,
        6..=7 => Fact::Canary(i, rng.below(2) == 0),
        8 => Fact::Disputed(i),
        9..=10 => Fact::Redundancy(i, *rng.pick(&[Some(true), Some(false), None])),
        _ => Fact::NoSignal(i, rng.below(4) as u8),
    }
}

fn derive(rows: &[(u64, Fact)], operator: usize) -> ReputationStats {
    let (mut released, mut faults) = (0u64, 0u64);
    let (mut canary_passed, mut canary_failed) = (0u64, 0u64);
    let mut disputed = 0u64;
    let (mut redundancy_agreed, mut redundancy_disagreed) = (0u64, 0u64);
    for (_, fact) in rows {
        match fact {
            Fact::Released(i) if *i == operator => released += 1,
            Fact::AttributedRefund(i) if *i == operator => faults += 1,
            Fact::Canary(i, passed) if *i == operator => {
                if *passed {
                    canary_passed += 1;
                } else {
                    canary_failed += 1;
                    faults += 1;
                }
            }
            Fact::Disputed(i) if *i == operator => {
                disputed += 1;
                faults += 1;
            }
            Fact::Redundancy(i, Some(agreed)) if *i == operator => {
                if *agreed {
                    redundancy_agreed += 1;
                } else {
                    redundancy_disagreed += 1;
                    faults += 1;
                }
            }
            _ => {}
        }
    }
    ReputationStats {
        released,
        faults,
        canary_passed,
        canary_failed,
        disputed,
        redundancy_agreed,
        redundancy_disagreed,
        score_bps: smoothed_score_bps(released, faults),
    }
}

fn issuer() -> AgentId {
    AgentId::new("coordinator@local", [7u8; 32])
}

async fn record(audit: &InMemoryAuditLog, timestamp_ms: u64, kind: AuditKind) {
    audit
        .record(AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms,
            issuer: issuer(),
            kind,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn random_audit_histories_derive_exact_reputation_for_every_operator() {
    for seed in 0..20u64 {
        let mut rng = Rng::new(0x5C0E ^ (seed << 8));
        let audit = Arc::new(InMemoryAuditLog::new());
        let source = AuditReputationSource::new(audit.clone());
        let operators: Vec<String> = ["op-alpha", "op-beta", "op-gamma"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut rows: Vec<(u64, Fact)> = Vec::new();

        for op in 0..140u64 {
            let now = op;
            let ctx = format!("seed {seed} op {op}");
            match rng.below(100) {
                0..=69 => {
                    let fact = random_fact(&mut rng, operators.len());
                    record(&audit, now, kind_for(&fact, &operators)).await;
                    rows.push((now, fact));
                }
                // Retention forgets a prefix of history — and the score
                // must forget with it, back toward neutral, not stick.
                70..=84 => {
                    let cutoff = rng.below(now + 2);
                    audit.purge_older_than(cutoff).await.unwrap();
                    rows.retain(|(ts, _)| *ts >= cutoff);
                }
                // An operator no surviving row names is exactly the
                // neutral prior, not a remembered ghost.
                _ => {
                    let stats = source.stats("op-stranger").await;
                    assert_eq!(
                        stats,
                        derive(&rows, usize::MAX),
                        "{ctx}: stranger not neutral"
                    );
                    assert_eq!(stats.score_bps, 5_000, "{ctx}: neutral prior moved");
                }
            }

            for (i, key) in operators.iter().enumerate() {
                let got = source.stats(key).await;
                assert_eq!(got, derive(&rows, i), "{ctx}: stats for {key} diverge");
            }
            let i = rng.below(operators.len() as u64) as usize;
            assert_eq!(
                source.score(&operators[i]).await,
                derive(&rows, i).score_bps,
                "{ctx}: score and stats disagree"
            );
        }
    }
}

fn requirement() -> CapabilityRequirement {
    CapabilityRequirement {
        gpu_class: None,
        min_vram_gb: None,
        model_id: None,
        kind: JobKind::BatchJob,
        max_duration_secs: 30,
        min_reputation_bps: None,
    }
}

/// A payable payout address derived from the operator's display name —
/// registration refuses anything that doesn't decode to a 32-byte key.
fn payout_for(display: &str) -> String {
    let mut key = [0u8; 32];
    for (i, b) in display.bytes().take(32).enumerate() {
        key[i] = b;
    }
    bs58::encode(key).into_string()
}

fn register(registry: &OperatorRegistry, display: &str, micro_usdc: u64) -> String {
    let identity = LocalIdentity::generate(display);
    let profile = CapabilityProfile {
        operator: identity.agent_id(),
        hardware: HardwareClass::CpuOnly,
        vram_gb: 0,
        models_served: vec!["any".into()],
        job_kinds: vec![JobKind::BatchJob],
        price: PriceAsk {
            unit: PriceUnit::PerJob,
            micro_usdc,
        },
        tee_capable: false,
        kind_prices: Vec::new(),
        kind_models: Vec::new(),
    };
    let req = RegisterRequest::sign(profile, payout_for(display), &identity).unwrap();
    registry.register(&req, 0, None, false).unwrap();
    identity.agent_id().pubkey_base58()
}

#[tokio::test]
async fn random_histories_gate_the_matcher_floor_on_derived_scores() {
    // Prices tie in pairs on purpose: ties fall through to reputation
    // descending, and only asymmetric histories make that comparison
    // load-bearing. Everything else the matcher gates on (liveness,
    // capability, bond) is held constant so this drives exactly the
    // floor × derived-score × price interplay.
    let prices = [100u64, 100, 250, 250];
    for seed in 0..12u64 {
        let mut rng = Rng::new(0xF100 ^ (seed << 8));
        let registry = OperatorRegistry::new();
        let bonds = OperatorBonds::new();
        let operators: Vec<String> = prices
            .iter()
            .enumerate()
            .map(|(i, price)| register(&registry, &format!("op-{i}@local"), *price))
            .collect();
        let audit = Arc::new(InMemoryAuditLog::new());
        let source = AuditReputationSource::new(audit.clone());
        let mut rows: Vec<(u64, Fact)> = Vec::new();

        for op in 0..140u64 {
            let now = op;
            let ctx = format!("seed {seed} op {op}");
            match rng.below(100) {
                0..=54 => {
                    let fact = random_fact(&mut rng, operators.len());
                    record(&audit, now, kind_for(&fact, &operators)).await;
                    rows.push((now, fact));
                }
                55..=64 => {
                    let cutoff = rng.below(now + 2);
                    audit.purge_older_than(cutoff).await.unwrap();
                    rows.retain(|(ts, _)| *ts >= cutoff);
                }
                _ => {
                    // Floors probe the boundary on purpose: an
                    // operator's own current score must ADMIT it, one
                    // more basis point must not.
                    let anchor = rng.below(operators.len() as u64) as usize;
                    let anchor_score = derive(&rows, anchor).score_bps;
                    let floor = match rng.below(4) {
                        0 => 0,
                        1 => 5_000,
                        2 => anchor_score,
                        _ => anchor_score + 1,
                    };
                    let offered = *rng.pick(&[99u64, 100, 250, 1_000]);
                    let exclude = if rng.below(4) == 0 {
                        Some(rng.pick(&operators).clone())
                    } else {
                        None
                    };
                    let got = select_operator(
                        &registry,
                        &source,
                        &bonds,
                        &requirement(),
                        offered,
                        0,
                        Duration::from_secs(45),
                        floor,
                        BondFloor::OPEN,
                        exclude.as_deref(),
                    )
                    .await;

                    let mut want: Vec<(u64, u32, &String)> = operators
                        .iter()
                        .enumerate()
                        .filter(|(_, key)| exclude.as_ref() != Some(*key))
                        .map(|(i, key)| (prices[i], derive(&rows, i).score_bps, key))
                        .filter(|(price, score, _)| *price <= offered && *score >= floor)
                        .collect();
                    want.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(a.2.cmp(b.2)));
                    assert_eq!(
                        got,
                        want.first().map(|(_, _, key)| (*key).clone()),
                        "{ctx}: winner diverges (floor {floor}, offered {offered}, excluded {exclude:?})"
                    );
                }
            }
        }
    }
}

/// The one arm no in-memory history can reach: `recent` failing. An
/// audit outage must read as NO history — the neutral prior, same as
/// an unknown operator — not a crash and not a stale remembered score.
struct UnreadableAudit;

#[async_trait]
impl AuditLog for UnreadableAudit {
    async fn record(&self, _event: AuditEvent) -> Result<(), AuditError> {
        Err(AuditError::Io(std::io::Error::other("audit store gone")))
    }

    async fn recent(&self, _limit: usize) -> Result<Vec<AuditEvent>, AuditError> {
        Err(AuditError::Io(std::io::Error::other("audit store gone")))
    }

    async fn purge_older_than(&self, _before_ms: u64) -> Result<u64, AuditError> {
        Err(AuditError::Io(std::io::Error::other("audit store gone")))
    }

    async fn verify_integrity(&self) -> Result<AuditIntegrityReport, AuditError> {
        Err(AuditError::Io(std::io::Error::other("audit store gone")))
    }
}

#[tokio::test]
async fn an_unreadable_audit_log_scores_everyone_at_the_neutral_prior() {
    let source = AuditReputationSource::new(Arc::new(UnreadableAudit));
    let stats = source.stats("op-alpha").await;
    assert_eq!(stats, derive(&[], usize::MAX));
    assert_eq!(stats.score_bps, 5_000);
    assert_eq!(source.score("op-alpha").await, 5_000);
}
