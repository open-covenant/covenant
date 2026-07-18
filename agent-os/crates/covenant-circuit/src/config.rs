use serde::Deserialize;

use crate::CircuitCapability;

/// Daemon-facing Circuit config. Deserialized from the daemon's provider config; disabled
/// by default, so Circuit tools register only when explicitly turned on. The capability
/// fields become the [`CircuitCapability`] every tool call is bound by.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CircuitConfig {
    pub enabled: bool,
    /// Tool-name allowlist. `None` = all Circuit tools; `Some([..])` = only those named.
    pub allow: Option<Vec<String>>,

    pub inference_base: Option<String>,
    pub data_base: Option<String>,
    pub model: Option<String>,
    /// Trusted co-located bypass key (skips payment on the inference gateway).
    pub internal_key: Option<String>,

    /// Per-call CIRC ceiling, raw base units.
    pub per_call_cap_raw: Option<u64>,
    /// Cumulative CIRC budget across all calls, raw base units.
    pub budget_raw: Option<u64>,
    /// Allowed 402 recipients (treasury pubkeys).
    pub allowed_recipients: Vec<String>,
    /// Allowed endpoint hosts.
    pub allowed_hosts: Vec<String>,

    /// Funding keypair path for the on-chain CIRC payer (used by the `solana` payer).
    pub keypair_path: Option<String>,
    /// Solana RPC the payer submits to.
    pub rpc_url: Option<String>,
}

impl CircuitConfig {
    /// Whether a given tool name is permitted by the allowlist.
    pub fn allows(&self, tool: &str) -> bool {
        match &self.allow {
            None => true,
            Some(list) => list.iter().any(|t| t == tool),
        }
    }

    /// The capability every Circuit call runs under, built from the config's spend fields.
    pub fn capability(&self) -> CircuitCapability {
        CircuitCapability {
            max_spend_raw: self.per_call_cap_raw,
            max_total_spend_raw: self.budget_raw,
            allowed_recipients: self.allowed_recipients.iter().cloned().collect(),
            allowed_hosts: self.allowed_hosts.iter().cloned().collect(),
        }
    }
}
