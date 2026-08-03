//! Reputation derived from the audit chain — not self-reported.
//!
//! A worker's standing is computed entirely from the escrow rows the daemon
//! already records: how many completion proofs it issued for that worker, how
//! many validated, and how many of those proofs an escrow actually released
//! against. The score is pinned to the audit chain root it was computed over,
//! so anyone holding the same chain recomputes the same numbers — neither the
//! worker nor the operator can inflate it. Covenant supplies this half;
//! earnings (the escrow's ledger) are the other half a marketplace combines
//! with it.
//!
//! Read-only: this never writes. Disputes are not yet a primitive (no dispute
//! row exists to count), so they are out of this first cut.
//!
//! This escrow standing is one of three distinct reputation surfaces and is
//! deliberately named apart from the other two: `covenant-audit`'s
//! `AuditReputation` (the 4-decimal compliance score, the only one projected
//! to EVM) and the SAP-native `reputationScore` the sap-bridge proxies
//! (upstream-defined scale). The three never mix; see
//! `docs/multichain-value-capture.md` for the reconciliation table.

use std::collections::HashSet;

use covenant_audit::{AuditError, AuditKind, AuditLog};
use serde::{Deserialize, Serialize};

/// A worker's escrow-derived standing at a point in the chain: completion
/// proofs, validation outcomes, and releases, with the rate in basis points.
/// Not the audit-chain compliance score (`covenant_audit::reputation`) and
/// not the SAP peer score — this one is Solana/IPC-local and is never
/// projected to another chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscrowReputation {
    pub worker_pubkey: String,
    /// `escrow_completion_proven` rows naming this worker.
    pub proofs_total: u64,
    pub validations_passed: u64,
    pub validations_failed: u64,
    /// `escrow_released` rows whose proof was one of this worker's.
    pub releases: u64,
    /// `validations_passed / proofs_total`, in basis points (0..=10000).
    pub completion_rate_bps: u32,
    /// Audit chain root the score was computed against. Recompute over the
    /// same chain to reproduce these numbers exactly.
    pub computed_audit_root_hex: String,
}

/// Compute a worker's reputation by scanning the audit log. One pass tallies
/// the worker's proofs and validation outcomes and collects its proof ids; the
/// release count joins `escrow_released` rows back to those ids. Pinned to the
/// chain root at read time.
pub async fn compute_reputation(
    audit: &dyn AuditLog,
    worker_pubkey: &str,
) -> Result<EscrowReputation, AuditError> {
    let events = audit.recent(usize::MAX).await?;

    let mut proof_ids: HashSet<uuid::Uuid> = HashSet::new();
    let mut proofs_total = 0u64;
    let mut validations_passed = 0u64;
    let mut validations_failed = 0u64;

    for e in &events {
        if let AuditKind::EscrowCompletionProven {
            worker_address: w,
            validation_passed,
            proof_id,
            ..
        } = &e.kind
        {
            if w == worker_pubkey {
                proofs_total += 1;
                proof_ids.insert(*proof_id);
                if *validation_passed {
                    validations_passed += 1;
                } else {
                    validations_failed += 1;
                }
            }
        }
    }

    let releases = events
        .iter()
        .filter(|e| {
            matches!(
                &e.kind,
                AuditKind::EscrowReleased { decision_id, .. } if proof_ids.contains(decision_id)
            )
        })
        .count() as u64;

    let completion_rate_bps = (validations_passed * 10_000)
        .checked_div(proofs_total)
        .unwrap_or(0) as u32;

    let report = audit.verify_integrity().await?;

    Ok(EscrowReputation {
        worker_pubkey: worker_pubkey.to_string(),
        proofs_total,
        validations_passed,
        validations_failed,
        releases,
        completion_rate_bps,
        computed_audit_root_hex: report.root_hash_hex,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_audit::{AuditEvent, InMemoryAuditLog};
    use covenant_types::AgentId;
    use uuid::Uuid;

    fn issuer() -> AgentId {
        AgentId::new("daemon@local", [9u8; 32])
    }

    async fn proof(audit: &InMemoryAuditLog, worker: &str, proof_id: Uuid, passed: bool) {
        audit
            .record(AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: 1,
                issuer: issuer(),
                kind: AuditKind::EscrowCompletionProven {
                    proof_id,
                    escrow_id: "escrow_x".into(),
                    job_id: Uuid::new_v4(),
                    hirer_address: "0xHirer".into(),
                    worker_address: worker.into(),
                    amount: "80000".into(),
                    asset: "0xUSDC".into(),
                    network: "eip155:84532".into(),
                    provider: "orbserv".into(),
                    result_hash_hex: "abc".into(),
                    validation_passed: passed,
                    audit_root_hex: "root".into(),
                    signature_b58: "sig".into(),
                },
            })
            .await
            .unwrap();
    }

    async fn release(audit: &InMemoryAuditLog, proof_id: Uuid) {
        audit
            .record(AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: 2,
                issuer: issuer(),
                kind: AuditKind::EscrowReleased {
                    decision_id: proof_id,
                    receipt_id: Uuid::new_v4(),
                    escrow_id: "escrow_x".into(),
                    hirer_address: "0xHirer".into(),
                    worker_address: "0xWorker".into(),
                    amount: "80000".into(),
                    asset: "0xUSDC".into(),
                    network: "eip155:84532".into(),
                    provider: "orbserv".into(),
                    tx_sig: None,
                },
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn derives_counts_rate_and_releases_for_the_named_worker() {
        let audit = InMemoryAuditLog::new();
        let alice = bs58::encode([1u8; 32]).into_string();
        let bob = bs58::encode([2u8; 32]).into_string();

        let (p1, p2, p3) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        proof(&audit, &alice, p1, true).await;
        proof(&audit, &alice, p2, true).await;
        proof(&audit, &alice, p3, false).await; // one failed validation
        proof(&audit, &bob, Uuid::from_u128(9), true).await; // other worker, ignored
        release(&audit, p1).await; // alice's, counts
        release(&audit, p2).await; // alice's, counts
        release(&audit, Uuid::from_u128(9)).await; // bob's proof, not alice's

        let rep = compute_reputation(&audit, &alice).await.unwrap();
        assert_eq!(rep.worker_pubkey, alice);
        assert_eq!(rep.proofs_total, 3);
        assert_eq!(rep.validations_passed, 2);
        assert_eq!(rep.validations_failed, 1);
        assert_eq!(rep.releases, 2, "only releases against alice's proofs");
        assert_eq!(rep.completion_rate_bps, 6666, "2/3 in bps");
        assert!(!rep.computed_audit_root_hex.is_empty());
    }

    #[test]
    fn escrow_reputation_wire_shape_is_frozen() {
        // The Rust type was renamed to disambiguate it from the other
        // reputation surfaces; the serialized shape predates the rename
        // and must not move with it.
        let rep = EscrowReputation {
            worker_pubkey: "w".into(),
            proofs_total: 3,
            validations_passed: 2,
            validations_failed: 1,
            releases: 2,
            completion_rate_bps: 6666,
            computed_audit_root_hex: "root".into(),
        };
        let value = serde_json::to_value(&rep).unwrap();
        let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "completion_rate_bps",
                "computed_audit_root_hex",
                "proofs_total",
                "releases",
                "validations_failed",
                "validations_passed",
                "worker_pubkey",
            ]
        );
    }

    #[tokio::test]
    async fn unknown_worker_is_all_zeros() {
        let audit = InMemoryAuditLog::new();
        proof(
            &audit,
            &bs58::encode([1u8; 32]).into_string(),
            Uuid::from_u128(1),
            true,
        )
        .await;

        let rep = compute_reputation(&audit, "neverseen").await.unwrap();
        assert_eq!(rep.proofs_total, 0);
        assert_eq!(rep.validations_passed, 0);
        assert_eq!(rep.releases, 0);
        assert_eq!(rep.completion_rate_bps, 0);
    }
}
