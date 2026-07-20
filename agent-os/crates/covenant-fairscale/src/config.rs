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
    /// Settle keyless reads per call over x402 (USDC on Solana mainnet)
    /// instead of failing on the 402. The host process must also wire a payer
    /// into the client; this flag alone moves nothing.
    #[serde(default)]
    pub x402_enabled: bool,
    /// Per-read cap, atomic USDC (6 decimals), for score and trust-gate reads.
    /// Live quote 2026-07-20: 5000 ($0.005) each.
    #[serde(default = "default_x402_read_cap_atomic")]
    pub x402_read_cap_atomic: u64,
    /// Per-read cap for the credit read. Live quote 2026-07-20: 500000 ($0.50).
    #[serde(default = "default_x402_credit_cap_atomic")]
    pub x402_credit_cap_atomic: u64,
}

fn default_x402_read_cap_atomic() -> u64 {
    10_000
}

fn default_x402_credit_cap_atomic() -> u64 {
    600_000
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
            x402_enabled: false,
            x402_read_cap_atomic: default_x402_read_cap_atomic(),
            x402_credit_cap_atomic: default_x402_credit_cap_atomic(),
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
        assert!(!c.x402_enabled, "pay-per-read must default off");
        assert_eq!(c.x402_read_cap_atomic, 10_000);
        assert_eq!(c.x402_credit_cap_atomic, 600_000);
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
