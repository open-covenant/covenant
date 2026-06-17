//! The worker agent's action-log evidence: an append-only list of the
//! actions it took doing the bounty, plus the hash chain over them.
//!
//! This is the "action logging" half of the double verification. The chain
//! is computed identically to `covenant-audit` (sha256 over
//! `previous_hash + "\n" + entry_hash`, genesis = 64 zeros), so a trail's
//! recomputed root is exactly the value the daemon anchors on-chain. A
//! verifier recomputes [`AuditTrail::root`] over the supplied entries and
//! compares it to the anchored root: if they differ, the evidence was
//! edited after the fact and the bounty fails on integrity alone.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::validate;

const ZERO_CHAIN_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut out, "{byte:02x}").expect("write to string");
    }
    out
}

/// One recorded action the worker took. `detail_hash` is the worker's own
/// sha256 over the action's payload — we never carry payloads here, only
/// their hash, mirroring the attestation rule of "identifiers and roots,
/// never log contents".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ActionEntry {
    pub seq: u64,
    pub action: String,
    pub detail_hash: String,
}

impl ActionEntry {
    pub fn validate(&self) -> Result<(), String> {
        validate::field("action", &self.action)?;
        validate::hash_hex("detail_hash", &self.detail_hash)?;
        Ok(())
    }

    /// Canonical line hashed into the chain. Deterministic via JCS.
    fn line_hash(&self) -> String {
        let canonical = serde_jcs::to_vec(self).expect("entry serialise");
        sha256_hex(&canonical)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AuditTrail {
    pub entries: Vec<ActionEntry>,
}

impl AuditTrail {
    pub fn new(entries: Vec<ActionEntry>) -> Self {
        Self { entries }
    }

    /// Recompute the chain root over the entries. Empty trail → genesis.
    pub fn root(&self) -> String {
        let mut previous = ZERO_CHAIN_HASH.to_string();
        for entry in &self.entries {
            let material = format!("{previous}\n{}", entry.line_hash());
            previous = sha256_hex(material.as_bytes());
        }
        previous
    }

    /// Validate every entry and that `seq` is a 0-based contiguous run, so a
    /// trail can't silently omit or reorder actions.
    pub fn validate(&self) -> Result<(), String> {
        for (i, e) in self.entries.iter().enumerate() {
            e.validate()?;
            if e.seq != i as u64 {
                return Err(format!("entry {i} has seq {}, expected {i}", e.seq));
            }
        }
        Ok(())
    }

    /// Actions whose label is not permitted by `allowed` (prefix match).
    /// An action is in scope if it equals or is a `.`-child of a grant.
    pub fn out_of_scope<'a>(&'a self, allowed: &[String]) -> Vec<&'a str> {
        self.entries
            .iter()
            .map(|e| e.action.as_str())
            .filter(|a| !allowed.iter().any(|g| action_in_grant(a, g)))
            .collect()
    }
}

/// `tool.call.fs.read` is covered by grant `tool.call.fs.read` and by
/// `tool.call.fs` (namespace) but not by `tool.call.fsx`.
pub fn action_in_grant(action: &str, grant: &str) -> bool {
    action == grant
        || action
            .strip_prefix(grant)
            .is_some_and(|rest| rest.starts_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(seq: u64, action: &str) -> ActionEntry {
        ActionEntry {
            seq,
            action: action.into(),
            detail_hash: "a".repeat(64),
        }
    }

    #[test]
    fn empty_trail_is_genesis() {
        assert_eq!(AuditTrail::default().root(), ZERO_CHAIN_HASH);
    }

    #[test]
    fn root_is_deterministic_and_order_sensitive() {
        let a = AuditTrail::new(vec![
            entry(0, "tool.call.fs.read"),
            entry(1, "intent.dispatch"),
        ]);
        let b = AuditTrail::new(vec![
            entry(0, "tool.call.fs.read"),
            entry(1, "intent.dispatch"),
        ]);
        assert_eq!(a.root(), b.root());
        // Reordering changes the root: tamper-evidence.
        let c = AuditTrail::new(vec![
            entry(0, "intent.dispatch"),
            entry(1, "tool.call.fs.read"),
        ]);
        assert_ne!(a.root(), c.root());
        assert_ne!(a.root(), ZERO_CHAIN_HASH);
    }

    #[test]
    fn editing_an_entry_changes_the_root() {
        let clean = AuditTrail::new(vec![entry(0, "settlement.pay")]);
        let tampered = AuditTrail::new(vec![ActionEntry {
            seq: 0,
            action: "settlement.pay".into(),
            detail_hash: "b".repeat(64),
        }]);
        assert_ne!(clean.root(), tampered.root());
    }

    #[test]
    fn validate_requires_contiguous_seq() {
        AuditTrail::new(vec![entry(0, "a"), entry(1, "b")])
            .validate()
            .unwrap();
        assert!(AuditTrail::new(vec![entry(0, "a"), entry(2, "b")])
            .validate()
            .is_err());
    }

    #[test]
    fn scope_is_exact_or_namespace_child() {
        assert!(action_in_grant("tool.call.fs.read", "tool.call.fs.read"));
        assert!(action_in_grant("tool.call.fs.read", "tool.call.fs"));
        assert!(!action_in_grant("tool.call.fsx", "tool.call.fs"));
        assert!(!action_in_grant("settlement.pay", "tool.call.fs"));
        let trail = AuditTrail::new(vec![
            entry(0, "tool.call.fs.read"),
            entry(1, "settlement.pay"),
        ]);
        assert_eq!(
            trail.out_of_scope(&["tool.call.fs".into()]),
            vec!["settlement.pay"]
        );
    }
}
