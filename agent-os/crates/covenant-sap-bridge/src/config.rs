//! Bridge configuration.
//!
//! Mirrors `resolveSynapseConfig` in `packages/config/networks.mjs`.
//! The program ID and RPC URL live in config only — never inlined in
//! consumer crates — so a SAP redeploy never requires rebuilding the
//! daemon.

use serde::{Deserialize, Serialize};

/// Solana cluster the bridge is pointed at. Mirrors the Covenant
/// network keys so the bridge config can be derived from the active
/// network without re-reading env.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cluster {
    Devnet,
    Localnet,
    Mainnet,
}

impl Cluster {
    pub fn as_str(self) -> &'static str {
        match self {
            Cluster::Devnet => "devnet",
            Cluster::Localnet => "localnet",
            Cluster::Mainnet => "mainnet",
        }
    }
}

/// Resolved bridge configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Master switch. When false the bridge surface is a no-op and
    /// every method returns [`crate::BridgeError::Disabled`].
    pub enabled: bool,
    pub cluster: Cluster,
    pub program_id: String,
    pub rpc_url: String,
    pub explorer_url: String,
}

impl Config {
    /// Build a disabled-default config for tests and offline daemons.
    pub fn disabled(cluster: Cluster) -> Self {
        Self {
            enabled: false,
            cluster,
            program_id: String::new(),
            rpc_url: String::new(),
            explorer_url: String::new(),
        }
    }
}
