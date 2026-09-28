//! The matchmaker's reputation tie-break input, behind a trait.
//!
//! No `covenant-proof` crate exists anywhere in this workspace (checked
//! `agent-os/crates/*` and the `members` list in `agent-os/Cargo.toml`)
//! to depend on for this. Design-02-federation.md §5's own reused-as-
//! pattern table already points at the answer instead:
//! `covenantd/src/reputation.rs::compute_reputation` derives a
//! completion rate by scanning one daemon's own audit chain — "the
//! coordinator, sitting in every job's path, can run the same
//! computation over its own unified log." [`AuditReputationSource`] is
//! exactly that; [`NoReputation`] is the zero-input stub for tests and
//! for a coordinator run without an audit log.
//!
//! Scoring (C4) counts CONCLUDED, ATTRIBUTED outcomes only: a release
//! is a success, a refund attributed to the operator
//! (`execution_failed` / `operator_rejected` / `deadline_expired` with
//! an assignee) is a fault, and a job still in flight is neither — the
//! naive released/offered rate punished an operator for every job it
//! was still busy serving. The score is Laplace-smoothed,
//! `(released + 1) / (released + faults + 2)` in basis points, so an
//! unknown operator sits at a neutral 5_000 — above any operator with
//! a proven failure record, below anyone with a proven success record
//! — instead of tying with the worst.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use covenant_audit::{AuditKind, AuditLog};

/// One operator's concluded outcomes and the score they produce — the
/// same numbers `GET /federation/operators/:operator/reputation`
/// serves, so a match result is checkable against the audit chain with
/// zero reader homework.
///
/// Canary bookkeeping (C5): a failed probe is already inside `faults`
/// (it is a concluded, attributed outcome like any refund fault); the
/// two probe counters break the probe history out separately so a
/// reader can tell "never probed" from "probed and clean". A passed
/// probe adds nothing to `released` here — its release row already
/// did.
///
/// Disputes (C4) are inside `faults` too, and broken out the same way —
/// but with the opposite provenance caveat: a refund or canary fault is
/// the coordinator's own attributed judgment, while a dispute is the
/// buyer's attestation, which the coordinator records but cannot
/// verify. The separate counter is what lets a reader discount them
/// differently. Disputing costs the buyer the job price they already
/// paid, so it is not free griefing — but a buyer willing to pay to
/// tank a score can, until stake/slash (Phase 2) raises the stakes on
/// both sides.
///
/// Redundancy verdicts (C5) follow the canary's pattern: a
/// `ComputeRedundancyResult { agreed: Some(false) }` row — this
/// operator's receipt hash sat outside the strict majority of a
/// cross-operator sample — is a fault with its own counter, agreement
/// is the informational counterpart, and inconclusive samples touch
/// neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ReputationStats {
    pub released: u64,
    pub faults: u64,
    pub canary_passed: u64,
    pub canary_failed: u64,
    pub disputed: u64,
    pub redundancy_agreed: u64,
    pub redundancy_disagreed: u64,
    pub score_bps: u32,
}

pub fn smoothed_score_bps(released: u64, faults: u64) -> u32 {
    let numerator = (u128::from(released) + 1) * 10_000;
    let denominator = u128::from(released) + u128::from(faults) + 2;
    (numerator / denominator) as u32
}

/// Which reputation counter a single audit row moves, for the operator
/// it names. `None` is no signal — an open offer, a buyer-side refund
/// with no assignee, an inconclusive redundancy sample: rows that belong
/// to no operator's record. Both [`AuditReputationSource::stats`] (one
/// operator, every counter) and [`AuditReputationSource::scores`] (many
/// operators, released and faults only) read the log through this one
/// classifier, so the batched score can never drift from the
/// per-operator one.
fn classify(kind: &AuditKind) -> Option<(&str, Signal)> {
    match kind {
        AuditKind::ComputeJobReleased {
            operator_pubkey_b58,
            ..
        } => Some((operator_pubkey_b58, Signal::Released)),
        AuditKind::ComputeJobRefunded {
            operator_pubkey_b58: Some(op),
            ..
        } => Some((op, Signal::RefundFault)),
        AuditKind::ComputeCanaryResult {
            operator_pubkey_b58,
            passed,
            ..
        } => Some((
            operator_pubkey_b58,
            if *passed {
                Signal::CanaryPassed
            } else {
                Signal::CanaryFailed
            },
        )),
        AuditKind::ComputeJobDisputed {
            operator_pubkey_b58,
            ..
        } => Some((operator_pubkey_b58, Signal::Disputed)),
        AuditKind::ComputeRedundancyResult {
            operator_pubkey_b58,
            agreed: Some(agreed),
            ..
        } => Some((
            operator_pubkey_b58,
            if *agreed {
                Signal::RedundancyAgreed
            } else {
                Signal::RedundancyDisagreed
            },
        )),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum Signal {
    Released,
    RefundFault,
    CanaryPassed,
    CanaryFailed,
    Disputed,
    RedundancyAgreed,
    RedundancyDisagreed,
}

impl Signal {
    /// A concluded success — the only signal that raises a score.
    fn is_released(self) -> bool {
        matches!(self, Signal::Released)
    }

    /// An attributed fault — the only signal that lowers a score. The
    /// informational signals (a passed canary, an agreed sample) never
    /// move it: the release row they accompany already did.
    fn is_fault(self) -> bool {
        matches!(
            self,
            Signal::RefundFault
                | Signal::CanaryFailed
                | Signal::Disputed
                | Signal::RedundancyDisagreed
        )
    }
}

impl ReputationStats {
    const fn empty() -> Self {
        ReputationStats {
            released: 0,
            faults: 0,
            canary_passed: 0,
            canary_failed: 0,
            disputed: 0,
            redundancy_agreed: 0,
            redundancy_disagreed: 0,
            score_bps: 0,
        }
    }

    fn apply(&mut self, signal: Signal) {
        match signal {
            Signal::Released => self.released += 1,
            Signal::RefundFault => self.faults += 1,
            Signal::CanaryPassed => self.canary_passed += 1,
            Signal::CanaryFailed => {
                self.canary_failed += 1;
                self.faults += 1;
            }
            Signal::Disputed => {
                self.disputed += 1;
                self.faults += 1;
            }
            Signal::RedundancyAgreed => self.redundancy_agreed += 1,
            Signal::RedundancyDisagreed => {
                self.redundancy_disagreed += 1;
                self.faults += 1;
            }
        }
    }
}

#[async_trait]
pub trait ReputationSource: Send + Sync {
    /// Higher is better. No fixed scale is contractual — the matcher
    /// only ever compares two operators' scores against each other.
    async fn score(&self, operator_pubkey_b58: &str) -> u32;

    /// Scores several operators in one call. The default loops
    /// [`ReputationSource::score`] — right for a stateless source (a
    /// fixed table, a flat zero) that answers each key in O(1). A source
    /// that derives scores by scanning a history
    /// ([`AuditReputationSource`]) overrides this to read the history
    /// once for the whole set instead of once per operator, turning a
    /// match's O(candidates × history) into O(history). The returned map
    /// carries an entry for every requested key.
    async fn scores(&self, operator_pubkeys: &[&str]) -> HashMap<String, u32> {
        let mut out = HashMap::with_capacity(operator_pubkeys.len());
        for key in operator_pubkeys {
            out.insert((*key).to_string(), self.score(key).await);
        }
        out
    }

    /// The counts behind the score, for sources that have them. The
    /// default reports no history and echoes `score`.
    async fn stats(&self, operator_pubkey_b58: &str) -> ReputationStats {
        ReputationStats {
            released: 0,
            faults: 0,
            canary_passed: 0,
            canary_failed: 0,
            disputed: 0,
            redundancy_agreed: 0,
            redundancy_disagreed: 0,
            score_bps: self.score(operator_pubkey_b58).await,
        }
    }
}

/// Every operator ties at zero, so matching leans entirely on the
/// matcher's terminal tie-break — a deterministic order by operator
/// pubkey, so the same pool picks the same winner every time rather
/// than whatever the registry's HashMap iteration happened to yield.
/// (A load-spreading or seniority policy is a separate economic
/// decision; determinism is the invariant here.)
pub struct NoReputation;

#[async_trait]
impl ReputationSource for NoReputation {
    async fn score(&self, _operator_pubkey_b58: &str) -> u32 {
        0
    }
}

/// Fault-attributed reputation over this coordinator's own
/// hash-chained log: `ComputeJobReleased` rows are successes,
/// `ComputeJobRefunded` rows attributed to the operator are faults —
/// and so are failed `ComputeCanaryResult` rows, the fault class the
/// refund machinery cannot see (a hash-valid receipt over garbage
/// output, judged by the canary prober). Everything else (in-flight
/// offers, buyer-side refunds with no assignee, passed canaries whose
/// release row already counted) is no signal at all.
pub struct AuditReputationSource {
    audit: Arc<dyn AuditLog>,
}

impl AuditReputationSource {
    pub fn new(audit: Arc<dyn AuditLog>) -> Self {
        Self { audit }
    }
}

#[async_trait]
impl ReputationSource for AuditReputationSource {
    async fn score(&self, operator_pubkey_b58: &str) -> u32 {
        self.stats(operator_pubkey_b58).await.score_bps
    }

    async fn scores(&self, operator_pubkeys: &[&str]) -> HashMap<String, u32> {
        let Ok(events) = self.audit.recent(usize::MAX).await else {
            let neutral = smoothed_score_bps(0, 0);
            return operator_pubkeys
                .iter()
                .map(|k| ((*k).to_string(), neutral))
                .collect();
        };
        let wanted: HashSet<&str> = operator_pubkeys.iter().copied().collect();
        let mut tally: HashMap<&str, (u64, u64)> = HashMap::new();
        for event in &events {
            if let Some((op, signal)) = classify(&event.kind) {
                if wanted.contains(op) {
                    let (released, faults) = tally.entry(op).or_insert((0, 0));
                    if signal.is_released() {
                        *released += 1;
                    } else if signal.is_fault() {
                        *faults += 1;
                    }
                }
            }
        }
        operator_pubkeys
            .iter()
            .map(|key| {
                let (released, faults) = tally.get(key).copied().unwrap_or((0, 0));
                ((*key).to_string(), smoothed_score_bps(released, faults))
            })
            .collect()
    }

    async fn stats(&self, operator_pubkey_b58: &str) -> ReputationStats {
        let Ok(events) = self.audit.recent(usize::MAX).await else {
            let mut empty = ReputationStats::empty();
            empty.score_bps = smoothed_score_bps(0, 0);
            return empty;
        };
        let mut stats = ReputationStats::empty();
        for event in &events {
            if let Some((op, signal)) = classify(&event.kind) {
                if op == operator_pubkey_b58 {
                    stats.apply(signal);
                }
            }
        }
        stats.score_bps = smoothed_score_bps(stats.released, stats.faults);
        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_audit::{AuditEvent, InMemoryAuditLog};
    use covenant_types::AgentId;
    use uuid::Uuid;

    fn issuer() -> AgentId {
        AgentId::new("coordinator@local", [7u8; 32])
    }

    async fn record(audit: &InMemoryAuditLog, kind: AuditKind) {
        audit
            .record(AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: 1,
                issuer: issuer(),
                kind,
            })
            .await
            .unwrap();
    }

    async fn offered(audit: &InMemoryAuditLog, operator: &str) {
        record(
            audit,
            AuditKind::ComputeJobOffered {
                job_id: Uuid::new_v4(),
                operator_pubkey_b58: operator.to_string(),
                price_micro_usdc: 100,
                funding_source: "organic".into(),
            },
        )
        .await;
    }

    async fn released(audit: &InMemoryAuditLog, operator: &str) {
        record(
            audit,
            AuditKind::ComputeJobReleased {
                job_id: Uuid::new_v4(),
                operator_pubkey_b58: operator.to_string(),
                amount_micro_usdc: 100,
                funding_source: "organic".into(),
            },
        )
        .await;
    }

    async fn faulted(audit: &InMemoryAuditLog, operator: Option<&str>, reason: &str) {
        record(
            audit,
            AuditKind::ComputeJobRefunded {
                job_id: Uuid::new_v4(),
                reason: reason.into(),
                operator_pubkey_b58: operator.map(str::to_string),
            },
        )
        .await;
    }

    #[tokio::test]
    async fn no_reputation_always_zero() {
        assert_eq!(NoReputation.score("anyone").await, 0);
        let stats = NoReputation.stats("anyone").await;
        assert_eq!(stats.released, 0);
        assert_eq!(stats.score_bps, 0);
    }

    #[test]
    fn the_smoothed_score_orders_unknown_between_proven_good_and_proven_bad() {
        let unknown = smoothed_score_bps(0, 0);
        assert_eq!(unknown, 5_000, "no history is a neutral prior");
        assert!(smoothed_score_bps(1, 0) > unknown);
        assert!(smoothed_score_bps(0, 1) < unknown);
        // Asymptotes, never quite certain.
        assert_eq!(smoothed_score_bps(9_998, 0), 9_999);
        assert_eq!(smoothed_score_bps(0, 9_998), 1);
    }

    #[tokio::test]
    async fn successes_and_attributed_faults_move_the_score_but_in_flight_offers_do_not() {
        let audit = InMemoryAuditLog::new();
        let alice = "alice-pubkey";

        // Five open offers must not read as failure.
        for _ in 0..5 {
            offered(&audit, alice).await;
        }
        released(&audit, alice).await;
        let source = AuditReputationSource::new(Arc::new(audit));
        let stats = source.stats(alice).await;
        assert_eq!((stats.released, stats.faults), (1, 0));
        assert_eq!(
            stats.score_bps, 6_666,
            "(1+1)/(1+0+2): one success, nothing in flight counted"
        );
    }

    #[tokio::test]
    async fn a_failed_canary_is_a_fault_and_a_passed_one_never_double_counts() {
        let audit = InMemoryAuditLog::new();
        let alice = "alice-pubkey";

        // A passed canary's money already produced a release row; the
        // canary row itself must add nothing to the score inputs.
        released(&audit, alice).await;
        record(
            &audit,
            AuditKind::ComputeCanaryResult {
                job_id: Uuid::new_v4(),
                operator_pubkey_b58: alice.into(),
                passed: true,
                detail: "ok".into(),
            },
        )
        .await;
        // A failed canary ALSO released (the receipt verified — the
        // fraud is in the content), so its release row stands and the
        // canary row lands the fault.
        released(&audit, alice).await;
        record(
            &audit,
            AuditKind::ComputeCanaryResult {
                job_id: Uuid::new_v4(),
                operator_pubkey_b58: alice.into(),
                passed: false,
                detail: "output did not contain the expected token".into(),
            },
        )
        .await;

        let source = AuditReputationSource::new(Arc::new(audit));
        let stats = source.stats(alice).await;
        assert_eq!((stats.released, stats.faults), (2, 1));
        assert_eq!((stats.canary_passed, stats.canary_failed), (1, 1));
        assert_eq!(stats.score_bps, 6_000, "(2+1)/(2+1+2)");
    }

    #[tokio::test]
    async fn attributed_refunds_are_faults_and_unattributed_ones_are_nobodys() {
        let audit = InMemoryAuditLog::new();
        let alice = "alice-pubkey";

        released(&audit, alice).await;
        released(&audit, alice).await;
        faulted(&audit, Some(alice), "execution_failed").await;
        // Admission never assigned an operator: no one's fault.
        faulted(&audit, None, "admission_failed").await;
        // Another operator's fault is not alice's.
        faulted(&audit, Some("bob-pubkey"), "deadline_expired").await;

        let source = AuditReputationSource::new(Arc::new(audit));
        let alice_stats = source.stats(alice).await;
        assert_eq!((alice_stats.released, alice_stats.faults), (2, 1));
        assert_eq!(alice_stats.score_bps, 6_000, "(2+1)/(2+1+2)");

        let bob_stats = source.stats("bob-pubkey").await;
        assert_eq!((bob_stats.released, bob_stats.faults), (0, 1));
        assert!(
            bob_stats.score_bps < 5_000,
            "a proven failure record scores below an unknown operator"
        );
        assert_eq!(source.stats("never-seen").await.score_bps, 5_000);
    }

    #[tokio::test]
    async fn a_dispute_is_a_fault_with_its_own_counter() {
        let audit = InMemoryAuditLog::new();
        let alice = "alice-pubkey";

        // The disputed job released (the receipt verified, the buyer
        // paid); the dispute lands next to it, like a failed canary.
        released(&audit, alice).await;
        record(
            &audit,
            AuditKind::ComputeJobDisputed {
                job_id: Uuid::new_v4(),
                operator_pubkey_b58: alice.into(),
                buyer_pubkey_b58: "buyer-pubkey".into(),
                reason: "output was unrelated to the prompt".into(),
            },
        )
        .await;

        let source = AuditReputationSource::new(Arc::new(audit));
        let stats = source.stats(alice).await;
        assert_eq!((stats.released, stats.faults, stats.disputed), (1, 1, 1));
        assert_eq!(stats.score_bps, 5_000, "(1+1)/(1+1+2): nets to neutral");

        // Someone else's dispute is not alice's.
        assert_eq!(source.stats("buyer-pubkey").await.disputed, 0);
    }

    #[tokio::test]
    async fn a_redundancy_disagreement_is_a_fault_and_inconclusive_samples_are_nothing() {
        let audit = InMemoryAuditLog::new();
        let alice = "alice-pubkey";
        let source_job_id = Uuid::new_v4();

        released(&audit, alice).await;
        record(
            &audit,
            AuditKind::ComputeRedundancyResult {
                source_job_id,
                operator_pubkey_b58: alice.into(),
                agreed: Some(false),
                detail: "hash disagreed with the majority (2/3 on aa)".into(),
            },
        )
        .await;
        record(
            &audit,
            AuditKind::ComputeRedundancyResult {
                source_job_id: Uuid::new_v4(),
                operator_pubkey_b58: alice.into(),
                agreed: Some(true),
                detail: "hash agreed with 3/3 receipts".into(),
            },
        )
        .await;
        record(
            &audit,
            AuditKind::ComputeRedundancyResult {
                source_job_id: Uuid::new_v4(),
                operator_pubkey_b58: alice.into(),
                agreed: None,
                detail: "inconclusive: only 1 participating receipt(s)".into(),
            },
        )
        .await;

        let source = AuditReputationSource::new(Arc::new(audit));
        let stats = source.stats(alice).await;
        assert_eq!(
            (stats.redundancy_agreed, stats.redundancy_disagreed),
            (1, 1),
            "inconclusive rows count in neither column"
        );
        assert_eq!((stats.released, stats.faults), (1, 1));
        assert_eq!(stats.score_bps, 5_000, "(1+1)/(1+1+2): nets to neutral");
    }

    #[tokio::test]
    async fn batched_scores_equal_per_operator_scores_over_a_varied_history() {
        // The matcher reads a pool's scores in one batch; the reputation
        // endpoint serves them one at a time. They must never disagree —
        // a drift would let a batched match rank an operator differently
        // from the record its own reputation view shows. Drive a varied
        // history through every audit kind `classify` reads and assert
        // the two paths agree for every operator, including one the log
        // never names.
        let audit = InMemoryAuditLog::new();
        let pool = ["alice", "bob", "carol"];

        // A tiny deterministic PRNG: the "varied" history is the same
        // every run, since a flaky reputation test is worse than none.
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u32
        };

        for _ in 0..400 {
            let op = pool[next() as usize % pool.len()];
            match next() % 8 {
                0 => released(&audit, op).await,
                1 => faulted(&audit, Some(op), "execution_failed").await,
                // An unattributed and an off-pool fault must touch no one
                // in the pool.
                2 => faulted(&audit, None, "admission_failed").await,
                3 => faulted(&audit, Some("dave"), "deadline_expired").await,
                4 => offered(&audit, op).await,
                5 => {
                    record(
                        &audit,
                        AuditKind::ComputeCanaryResult {
                            job_id: Uuid::new_v4(),
                            operator_pubkey_b58: op.into(),
                            passed: next() % 2 == 0,
                            detail: "probe".into(),
                        },
                    )
                    .await
                }
                6 => {
                    record(
                        &audit,
                        AuditKind::ComputeJobDisputed {
                            job_id: Uuid::new_v4(),
                            operator_pubkey_b58: op.into(),
                            buyer_pubkey_b58: "buyer".into(),
                            reason: "unrelated".into(),
                        },
                    )
                    .await
                }
                _ => {
                    let agreed = match next() % 3 {
                        0 => Some(true),
                        1 => Some(false),
                        _ => None,
                    };
                    record(
                        &audit,
                        AuditKind::ComputeRedundancyResult {
                            source_job_id: Uuid::new_v4(),
                            operator_pubkey_b58: op.into(),
                            agreed,
                            detail: "sample".into(),
                        },
                    )
                    .await
                }
            }
        }

        let source = AuditReputationSource::new(Arc::new(audit));
        let keys: Vec<&str> = pool
            .iter()
            .copied()
            .chain(std::iter::once("never-seen"))
            .collect();
        let batched = source.scores(&keys).await;

        assert_eq!(
            batched.len(),
            keys.len(),
            "the batch returns an entry for every requested key"
        );
        for &key in &keys {
            assert_eq!(
                batched.get(key).copied(),
                Some(source.score(key).await),
                "batched score for {key} must equal its per-operator score"
            );
        }
        assert_eq!(
            batched.get("never-seen").copied(),
            Some(5_000),
            "a key the log never names scores at the neutral prior"
        );
    }
}
