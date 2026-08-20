//! What an outsider can see an operator do.
//!
//! Every field here is observable without access to the thing being settled.
//! An obligation carries an opaque handle, the moment the operator took it on,
//! and how it ended. No amount, no payer, no recipient, nothing that describes
//! the payload. That is not squeamishness: it is the constraint that makes this
//! usable on a private rail at all. An SLA that needed to see payments would be
//! an SLA nobody on such a rail could adopt.
//!
//! The entries are hash-chained with the same construction as
//! [`covenant-audit`], so the log a breach is computed from cannot be edited
//! after the fact without changing its root. That matters because of where the
//! root ends up: the deployed settlement program reads a slash reason from the
//! provenance root rather than from whatever a caller passes in, so the reason
//! for a slash is only as trustworthy as the record it derives from.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Chain seed, matching `covenant-audit`.
const ZERO_CHAIN_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// How an obligation ended.
///
/// There is deliberately no `Wrong` variant. Whether settlement went to the
/// right party for the right amount is not observable from outside a private
/// rail, and a variant for it would invite an operator, or us, to assert it
/// anyway. See the crate docs for what closing that gap actually requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Outcome {
    /// Completed, at this time.
    Settled { at_ms: u64 },
    /// Accepted and never completed inside the SLA's window.
    TimedOut,
    /// Accepted and then abandoned: the operator signalled it would not
    /// complete. Distinct from a timeout because the operator said so, which
    /// is a different failure to diagnose even though it slashes the same.
    Dropped,
}

impl Outcome {
    /// Whether this outcome counts against the operator.
    pub fn is_miss(&self, accepted_at_ms: u64, settle_within_ms: u64) -> bool {
        match self {
            Self::Settled { at_ms } => at_ms.saturating_sub(accepted_at_ms) > settle_within_ms,
            Self::TimedOut | Self::Dropped => true,
        }
    }
}

/// One thing the operator took on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Obligation {
    /// Opaque handle. The operator's own reference is fine; it identifies the
    /// obligation without describing it.
    pub id: String,
    pub accepted_at_ms: u64,
    pub outcome: Outcome,
}

impl Obligation {
    pub fn new(id: impl Into<String>, accepted_at_ms: u64, outcome: Outcome) -> Result<Self> {
        let id = id.into();
        if id.is_empty() || id.len() > 128 {
            return Err(Error::Observation(format!(
                "obligation id must be 1 to 128 bytes, got {}",
                id.len()
            )));
        }
        if id.chars().any(|c| c.is_control()) {
            return Err(Error::Observation(
                "obligation id must not contain control characters".into(),
            ));
        }
        if let Outcome::Settled { at_ms } = outcome {
            if at_ms < accepted_at_ms {
                return Err(Error::Observation(format!(
                    "settled at {at_ms} before it was accepted at {accepted_at_ms}"
                )));
            }
        }
        Ok(Self {
            id,
            accepted_at_ms,
            outcome,
        })
    }

    fn line_hash(&self) -> Result<String> {
        let canon = serde_json::to_string(self).map_err(|e| Error::Decode(e.to_string()))?;
        Ok(sha256_hex(canon.as_bytes()))
    }
}

/// The operator's observed history, hash-chained.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationLog {
    entries: Vec<Obligation>,
}

impl ObservationLog {
    pub fn new(entries: Vec<Obligation>) -> Self {
        Self { entries }
    }

    pub fn append(&mut self, obligation: Obligation) {
        self.entries.push(obligation);
    }

    pub fn entries(&self) -> &[Obligation] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Chain root over the log. An empty log is the genesis value.
    ///
    /// Same construction as `covenant-audit`: `sha256(prev + "\n" + entry)`,
    /// so a reordered or edited log produces a different root and any slash
    /// computed from the old root stops verifying.
    pub fn root(&self) -> Result<String> {
        let mut previous = ZERO_CHAIN_HASH.to_string();
        for entry in &self.entries {
            let material = format!("{previous}\n{}", entry.line_hash()?);
            previous = sha256_hex(material.as_bytes());
        }
        Ok(previous)
    }

    /// Obligations accepted inside `[start_ms, end_ms)`.
    pub fn in_window(&self, start_ms: u64, end_ms: u64) -> impl Iterator<Item = &Obligation> {
        self.entries
            .iter()
            .filter(move |o| o.accepted_at_ms >= start_ms && o.accepted_at_ms < end_ms)
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settled(id: &str, at: u64, took: u64) -> Obligation {
        Obligation::new(id, at, Outcome::Settled { at_ms: at + took }).unwrap()
    }

    #[test]
    fn a_settlement_inside_the_promise_is_not_a_miss() {
        let o = settled("a", 1_000, 500);
        assert!(!o.outcome.is_miss(o.accepted_at_ms, 1_000));
    }

    #[test]
    fn a_late_settlement_is_a_miss_at_the_boundary_and_beyond() {
        let exactly_on_time = settled("a", 1_000, 1_000);
        assert!(!exactly_on_time.outcome.is_miss(1_000, 1_000));

        let one_late = settled("a", 1_000, 1_001);
        assert!(one_late.outcome.is_miss(1_000, 1_000));
    }

    #[test]
    fn a_timeout_or_a_drop_is_always_a_miss() {
        assert!(Outcome::TimedOut.is_miss(1_000, u64::MAX));
        assert!(Outcome::Dropped.is_miss(1_000, u64::MAX));
    }

    #[test]
    fn an_empty_log_is_the_genesis_root() {
        assert_eq!(ObservationLog::default().root().unwrap(), ZERO_CHAIN_HASH);
    }

    #[test]
    fn editing_the_log_changes_the_root() {
        let base = ObservationLog::new(vec![settled("a", 1_000, 10), settled("b", 2_000, 10)]);
        let root = base.root().unwrap();

        // A later outcome rewritten to look on time.
        let edited = ObservationLog::new(vec![
            settled("a", 1_000, 10),
            Obligation::new("b", 2_000, Outcome::TimedOut).unwrap(),
        ]);
        assert_ne!(root, edited.root().unwrap());

        // Reordering, with the same entries.
        let reordered = ObservationLog::new(vec![settled("b", 2_000, 10), settled("a", 1_000, 10)]);
        assert_ne!(root, reordered.root().unwrap());

        // Dropping an inconvenient entry.
        let truncated = ObservationLog::new(vec![settled("a", 1_000, 10)]);
        assert_ne!(root, truncated.root().unwrap());
    }

    #[test]
    fn the_root_is_stable_for_the_same_log() {
        let log = ObservationLog::new(vec![settled("a", 1, 1), settled("b", 2, 1)]);
        assert_eq!(log.root().unwrap(), log.root().unwrap());
        assert_eq!(log.root().unwrap().len(), 64);
    }

    #[test]
    fn the_window_is_half_open() {
        let log = ObservationLog::new(vec![
            settled("before", 999, 1),
            settled("start", 1_000, 1),
            settled("inside", 1_500, 1),
            settled("end", 2_000, 1),
        ]);
        let ids: Vec<&str> = log.in_window(1_000, 2_000).map(|o| o.id.as_str()).collect();
        assert_eq!(ids, vec!["start", "inside"]);
    }

    #[test]
    fn an_obligation_carries_nothing_about_the_payload() {
        let o = settled("payment-1", 1_000, 10);
        let wire = serde_json::to_string(&o).unwrap();
        for leaked in ["amount", "recipient", "payer", "sender", "usd"] {
            assert!(!wire.contains(leaked), "obligation leaked {leaked}");
        }
    }

    #[test]
    fn malformed_obligations_are_refused() {
        assert!(Obligation::new("", 0, Outcome::TimedOut).is_err());
        assert!(Obligation::new("a".repeat(129), 0, Outcome::TimedOut).is_err());
        assert!(Obligation::new("a\nb", 0, Outcome::TimedOut).is_err());
        // Settled before accepted: a clock or ingestion fault, not an SLA pass.
        assert!(Obligation::new("a", 1_000, Outcome::Settled { at_ms: 999 }).is_err());
    }
}
