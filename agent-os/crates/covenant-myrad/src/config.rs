//! Myrad connector configuration.
//!
//! `enabled` registers the verification tool; `false` (the default) leaves the
//! connector inert. `policy` is the bar the integrity checks run against, and it
//! is recorded in every receipt so two receipts are only comparable when they
//! were issued under the same thresholds.

use serde::{Deserialize, Serialize};

use crate::integrity::IntegrityPolicy;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MyradConfig {
    /// Off by default: no tool is registered until a deployment sets this true.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub policy: IntegrityPolicy,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_off_with_the_default_policy() {
        let c = MyradConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.policy.min_k, 5);
    }

    #[test]
    fn an_empty_object_deserializes_to_the_default() {
        let c: MyradConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(c, MyradConfig::default());
    }

    #[test]
    fn the_policy_can_be_tightened_from_config() {
        let c: MyradConfig = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "policy": { "min_k": 25, "require_reclaim_proof_ref": true }
        }))
        .unwrap();
        assert!(c.enabled);
        assert_eq!(c.policy.min_k, 25);
        assert!(c.policy.require_reclaim_proof_ref);
    }
}
