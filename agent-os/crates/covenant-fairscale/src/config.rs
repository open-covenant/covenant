//! FairScale provider configuration.
//!
//! `enabled` turns on the read-only oracle and the `fairscale.score` tool.
//! `credit_enabled` gates the credit read separately: it is a heavier,
//! pay-per-call underwriting read ($0.50 over x402 vs $0.005 for reputation),
//! so it stays off by default even when the oracle is on.

use serde::{Deserialize, Serialize};

/// FairScale reputation host: `GET /score`, `/fairScore`, `/walletScore`.
pub const REPUTATION_BASE_URL: &str = "https://api.fairscale.xyz";

/// FairScale agent + credit host: `GET /v1/score`, `/v1/trust-gate`, `/v1/credit`.
pub const AGENT_BASE_URL: &str = "https://agent-api.fairscale.xyz";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FairScaleConfig {
    /// Master switch for the read-only oracle + the `fairscale.score` tool.
    /// `false` means no FairScale call ever leaves the host.
    #[serde(default)]
    pub enabled: bool,
    /// Reputation host. Endpoint paths are built against this.
    #[serde(default = "default_reputation_base_url")]
    pub reputation_base_url: String,
    /// Agent + credit host.
    #[serde(default = "default_agent_base_url")]
    pub agent_base_url: String,
    /// Include the FairScale trust-gate decision (agent host) in the result.
    #[serde(default)]
    pub include_trust_gate: bool,
    /// Enable the credit read. Off by default: it is a separate, more expensive
    /// underwriting read and is advisory only (no funds move).
    #[serde(default)]
    pub credit_enabled: bool,
}

fn default_reputation_base_url() -> String {
    REPUTATION_BASE_URL.to_string()
}

fn default_agent_base_url() -> String {
    AGENT_BASE_URL.to_string()
}

impl Default for FairScaleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            reputation_base_url: default_reputation_base_url(),
            agent_base_url: default_agent_base_url(),
            include_trust_gate: false,
            credit_enabled: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_off_with_prod_hosts() {
        let c = FairScaleConfig::default();
        assert!(!c.enabled);
        assert!(!c.credit_enabled, "credit read must default off");
        assert!(!c.include_trust_gate);
        assert_eq!(c.reputation_base_url, REPUTATION_BASE_URL);
        assert_eq!(c.agent_base_url, AGENT_BASE_URL);
    }

    #[test]
    fn enabling_reads_does_not_enable_credit() {
        let c: FairScaleConfig =
            serde_json::from_value(serde_json::json!({ "enabled": true })).unwrap();
        assert!(c.enabled);
        assert!(!c.credit_enabled, "reads on must not turn credit on");
    }
}
