//! Non-secret configuration for the BlockRun overlay. The x402 signing key is
//! never held here; it stays with the daemon's subprocess signer.

use serde::{Deserialize, Serialize};

pub const DEFAULT_BASE_URL: &str = "https://blockrun.ai/api";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BlockRunConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    /// Tool allowlist. `None` = all registered tools, `Some([])` = none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
}

fn default_base_url() -> String {
    DEFAULT_BASE_URL.to_string()
}

impl Default for BlockRunConfig {
    fn default() -> Self {
        BlockRunConfig {
            enabled: false,
            base_url: default_base_url(),
            allow: None,
        }
    }
}

impl BlockRunConfig {
    pub fn allows(&self, tool: &str) -> bool {
        match &self.allow {
            None => true,
            Some(list) => list.iter().any(|t| t == tool),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled_with_prod_url() {
        let c = BlockRunConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.base_url, DEFAULT_BASE_URL);
        assert!(c.allows("blockrun.call"));
    }

    #[test]
    fn empty_allowlist_denies_all() {
        let c = BlockRunConfig {
            allow: Some(vec![]),
            ..Default::default()
        };
        assert!(!c.allows("blockrun.call"));
    }

    #[test]
    fn allowlist_gates_by_name() {
        let c = BlockRunConfig {
            allow: Some(vec!["blockrun.verify".into()]),
            ..Default::default()
        };
        assert!(c.allows("blockrun.verify"));
        assert!(!c.allows("blockrun.call"));
    }
}
