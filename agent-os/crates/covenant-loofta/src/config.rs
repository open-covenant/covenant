//! Loofta connector configuration.
//!
//! One switch. `enabled` turns on the `loofta.payout_check` tool and lets the
//! gate run. `false` (the default) means the connector is inert: no recipient
//! read leaves the host and no tool is registered. The payout policy itself is
//! held by the daemon, not here, so a deployment tunes the gate without a config
//! migration.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct LooftaConfig {
    /// Off by default: the connector registers no tool and reads nothing about a
    /// recipient until a deployment sets this true.
    #[serde(default)]
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_off() {
        assert!(!LooftaConfig::default().enabled);
    }

    #[test]
    fn deserializes_empty_object_to_off() {
        let c: LooftaConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!c.enabled);
    }
}
