//! Experimental event-classification heuristic with a legacy reputation name.
//!
//! The caller supplies an arbitrary event slice. This module classifies selected
//! event variants and returns a ratio; it does not establish event completeness,
//! runtime mediation, subject attribution, authorization, delivery, settlement,
//! or reputation. Some positive variants are operator actions, and
//! `IntentDispatched` is counted without inspecting its status. Consumers must
//! treat the output as a local implementation-defined metric only.

use crate::{AuditEvent, AuditKind};

/// Decimal places the scaled score is expressed in. `9500` at 4 decimals is
/// `0.95`. Matches the EAS reputation schema's `score_decimals`.
pub const SCORE_DECIMALS: u8 = 4;

/// How one audit event bears on the compliance score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signal {
    /// An event variant this heuristic counts in its numerator.
    Compliant,
    /// An event variant this heuristic counts in its denominator only.
    Violation,
    /// Administrative, operator, in-progress, or informational; no bearing.
    Neutral,
}

/// Legacy-named output of the experimental local event heuristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditReputation {
    /// Events classified into the numerator; not proof of compliant execution.
    pub compliant: u64,
    /// Events classified as negative by this implementation.
    pub violations: u64,
    /// `compliant / (compliant + violations)` scaled to [`SCORE_DECIMALS`],
    /// rounded half-up. `None` when the agent has no scored activity yet, so a
    /// blank history is never dressed up as a perfect score.
    pub score: Option<u32>,
    /// The scale `score` is expressed in.
    pub decimals: u8,
}

impl AuditReputation {
    /// Events this heuristic chose to score (`compliant + violations`).
    pub fn scored_actions(&self) -> u64 {
        self.compliant + self.violations
    }
}

/// Compute the legacy-named ratio over the caller-supplied event slice.
pub fn score<'a>(events: impl IntoIterator<Item = &'a AuditEvent>) -> AuditReputation {
    let mut compliant = 0u64;
    let mut violations = 0u64;
    for event in events {
        match classify(&event.kind) {
            Signal::Compliant => compliant += 1,
            Signal::Violation => violations += 1,
            Signal::Neutral => {}
        }
    }
    let total = compliant + violations;
    let score = if total == 0 {
        None
    } else {
        let scale = 10u128.pow(SCORE_DECIMALS as u32);
        let scaled = (u128::from(compliant) * scale + u128::from(total) / 2) / u128::from(total);
        Some(scaled as u32)
    };
    AuditReputation {
        compliant,
        violations,
        score,
        decimals: SCORE_DECIMALS,
    }
}

/// Classify a kind. Exhaustive by design: a new `AuditKind` variant fails to
/// compile here until someone decides whether it bears on reputation.
fn classify(kind: &AuditKind) -> Signal {
    use AuditKind::*;
    match kind {
        // Numerator variants. This is event-type classification, not a proof
        // that the action was authorized, successful, or attributable.
        IntentDispatched { .. }
        | HermesToolCompleted { .. }
        | HermesFileWritten { .. }
        | CapabilityGranted { .. }
        | SecretAccessGranted { .. }
        | CrossHostA2AAdmission { .. }
        | CrossHostA2ADelivery { .. }
        | ExternalPaymentSettled { .. }
        | ToolCallCompleted { .. } => Signal::Compliant,

        // Denominator-only variants selected by this local heuristic.
        CapabilityGrantRejected { .. }
        | CapabilityScopeRejected { .. }
        | SecretAccessDenied { .. }
        | A2AResultRejected { .. }
        | AuthenticationFailed { .. }
        | A2ASenderMismatch { .. }
        | A2ARecipientRejected { .. }
        | ToolCallSchemaRejected { .. }
        | CapabilityBudgetExhausted { .. }
        | BudgetExhausted { .. } => Signal::Violation,

        // In-progress, informational, maintenance, or operator/peer plumbing:
        // not a signal of the agent's own compliance.
        HermesToolInvoked { .. }
        | HermesApprovalRequested { .. }
        | HermesApprovalResolved { .. }
        | CapabilityCheck { .. }
        | IntentIgnored { .. }
        | A2ARepairApplied { .. }
        | A2AAutoRetrySchedulerScan { .. }
        | MemoryRepairApplied { .. }
        | MemoryCompactionApplied { .. }
        | CapabilityRevoked { .. }
        | CapabilityRevokeRejected { .. }
        | BudgetPreempted { .. }
        | BudgetPreemptFailed { .. }
        | BudgetUnseeded { .. }
        | OperatorTokenRotated { .. }
        | OperatorTokenRotationRejected { .. }
        | OperatorPeersListRejected { .. }
        | PeerRevoked { .. }
        | OperatorPeerRevokeRejected { .. }
        | PeerSelfRevokeBlocked { .. }
        | SettlementReceiptBackfillApplied { .. }
        | MemoryRecordBackfillApplied { .. }
        | AceDataGeneration { .. }
        | PeerEnrolled { .. }
        | PeerEnrollmentRejected { .. }
        | SpendAuthorizationDecided { .. }
        | SpendSettled { .. }
        | EscrowCompletionProven { .. }
        | EscrowReleased { .. } => Signal::Neutral,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentId;

    fn ev(kind: AuditKind) -> AuditEvent {
        AuditEvent {
            id: uuid::Uuid::nil(),
            timestamp_ms: 0,
            issuer: AgentId::new("agent", [0u8; 32]),
            kind,
        }
    }

    fn granted() -> AuditKind {
        AuditKind::CapabilityGranted {
            subject_display: "s".into(),
            action: "a".into(),
            granted_by_display: "op".into(),
            signature_b58: "sig".into(),
        }
    }

    fn scope_rejected() -> AuditKind {
        AuditKind::CapabilityScopeRejected {
            agent_id: "a".into(),
            action: "x".into(),
            reason: "out of scope".into(),
        }
    }

    #[test]
    fn empty_history_has_no_score() {
        let r = score(&[]);
        assert_eq!(r.score, None);
        assert_eq!(r.scored_actions(), 0);
    }

    #[test]
    fn all_compliant_is_one_point_zero() {
        let events: Vec<_> = (0..4).map(|_| ev(granted())).collect();
        let r = score(&events);
        assert_eq!(r.compliant, 4);
        assert_eq!(r.violations, 0);
        assert_eq!(r.score, Some(10_000)); // 1.0000
    }

    #[test]
    fn mixed_history_is_the_compliant_fraction() {
        // 3 compliant, 1 violation -> 0.75.
        let mut events: Vec<_> = (0..3).map(|_| ev(granted())).collect();
        events.push(ev(scope_rejected()));
        let r = score(&events);
        assert_eq!((r.compliant, r.violations), (3, 1));
        assert_eq!(r.score, Some(7_500));
    }

    #[test]
    fn rounds_half_up() {
        // 2 of 3 -> 0.6667 (6666.66..).
        let mut events: Vec<_> = (0..2).map(|_| ev(granted())).collect();
        events.push(ev(scope_rejected()));
        assert_eq!(score(&events).score, Some(6_667));
    }

    #[test]
    fn neutral_events_do_not_move_the_score() {
        let events = vec![
            ev(granted()),
            ev(AuditKind::OperatorTokenRotated {
                peer_display: "peer".into(),
                old_token_prefix: "old".into(),
                new_token_prefix: "new".into(),
            }),
            ev(AuditKind::MemoryCompactionApplied {
                mode: "auto".into(),
                changed: true,
                reason: "scheduled".into(),
                deleted: vec![],
                stale_marked: vec![],
                parents_detached: vec![],
            }),
        ];
        let r = score(&events);
        assert_eq!(r.scored_actions(), 1);
        assert_eq!(r.score, Some(10_000));
    }
}
