//! Metaplex profile configuration.
//!
//! Everything operator-tunable about the Metaplex profile lives here: the
//! cluster and RPC the signer sidecar submits to, the DAS endpoint reads
//! go through, the sidecar binary that holds the minting key, the
//! collection writes are pinned to, and the per-action SOL ceiling.
//!
//! Defaults are read-only and devnet. A daemon that only flips
//! `enabled = true` and points `das_url` at a DAS provider gets the
//! read tools and nothing that can sign or spend — every write tool
//! stays dark until a signer binary and RPC are configured.

use serde::{Deserialize, Serialize};

/// MPL Core program (single-account NFT standard; AppData + Oracle
/// external plugins). Mainnet and devnet share this id.
pub const MPL_CORE_PROGRAM_ID: &str = "CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d";
/// MPL Agent Identity registry (binds a PDA to a Core asset).
pub const MPL_AGENT_IDENTITY_PROGRAM_ID: &str = "1DREGFgysWYxLnRnKQnwrxnJQeSMk2HmGaC6whw2B2p";
/// MPL Agent Validation registry (attestation surface). Upstream is
/// early/unfinalised — we align our schema with it but do not depend on
/// it; raw MPL Core AppData is the v1 write path.
pub const MPL_AGENT_VALIDATION_PROGRAM_ID: &str = "VALREGY66A9ieJfFUNs5GrxFTy498KUoSU7TbmSePQi";
/// MPL Agent Reputation registry (early/unfinalised upstream).
pub const MPL_AGENT_REPUTATION_PROGRAM_ID: &str = "REPREG5c1gPHuHukEyANpksLdHFaJCiTrm6zJgNhRZR";
/// Bubblegum (compressed NFTs). The v2 instruction set shares this id.
pub const MPL_BUBBLEGUM_PROGRAM_ID: &str = "BGUMAp9Gq7iTEuizy4pqaxsTyUCBK68MDfK752saRPUY";

/// Metaplex program ids the signer sidecar is permitted to target.
/// Pinned so a poisoned config cannot redirect a signed instruction at
/// an attacker-controlled program. The sidecar re-checks this list
/// before building any instruction.
pub const ALLOWED_PROGRAM_IDS: &[&str] = &[
    MPL_CORE_PROGRAM_ID,
    MPL_AGENT_IDENTITY_PROGRAM_ID,
    MPL_AGENT_VALIDATION_PROGRAM_ID,
    MPL_AGENT_REPUTATION_PROGRAM_ID,
    MPL_BUBBLEGUM_PROGRAM_ID,
];

/// Default cluster. Devnet on purpose — a misconfigured daemon must not
/// write to mainnet by accident.
pub const DEFAULT_CLUSTER: &str = "devnet";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetaplexConfig {
    /// Master switch. `false` means no Metaplex tool is registered and
    /// no Metaplex call ever leaves the host.
    #[serde(default)]
    pub enabled: bool,
    /// Settlement cluster (`devnet` | `mainnet-beta`).
    #[serde(default = "default_cluster")]
    pub cluster: String,
    /// Solana RPC the signer sidecar submits transactions to. Empty
    /// disables every write tool.
    #[serde(default)]
    pub rpc_url: String,
    /// DAS (Digital Asset Standard) JSON-RPC endpoint reads go through.
    /// Empty disables every read tool.
    #[serde(default)]
    pub das_url: String,
    /// Absolute path to the `covenant-metaplex-signer` binary. Empty
    /// disables every write tool; reads never need it.
    #[serde(default)]
    pub signer_binary: String,
    /// MPL Core collection that minted assets and attestations are
    /// grouped under. Empty means no collection pin.
    #[serde(default)]
    pub collection: String,
    /// Per-action ceiling in lamports. A signer request whose estimated
    /// cost (rent + protocol fee) exceeds this is refused before signing.
    /// `0` defers to the sidecar's built-in cap.
    #[serde(default)]
    pub per_action_cap_lamports: u64,
    /// Tool allowlist by short slug (`das.get_asset`, `attest.audit_root`).
    /// `None` allows every tool the surface enables; `Some([])` allows none.
    #[serde(default)]
    pub allow: Option<Vec<String>>,
}

fn default_cluster() -> String {
    DEFAULT_CLUSTER.to_string()
}

impl Default for MetaplexConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cluster: default_cluster(),
            rpc_url: String::new(),
            das_url: String::new(),
            signer_binary: String::new(),
            collection: String::new(),
            per_action_cap_lamports: 0,
            allow: None,
        }
    }
}

fn truthy(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

impl MetaplexConfig {
    /// Build a config from `COVENANT_METAPLEX_*` environment variables.
    /// Unset variables fall back to the read-only devnet defaults.
    pub fn from_env() -> Self {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            enabled: env("COVENANT_METAPLEX_ENABLED")
                .map(|v| truthy(&v))
                .unwrap_or(false),
            cluster: env("COVENANT_METAPLEX_CLUSTER").unwrap_or_else(default_cluster),
            rpc_url: env("COVENANT_METAPLEX_RPC_URL").unwrap_or_default(),
            das_url: env("COVENANT_METAPLEX_DAS_URL").unwrap_or_default(),
            signer_binary: env("COVENANT_METAPLEX_SIGNER_BIN").unwrap_or_default(),
            collection: env("COVENANT_METAPLEX_COLLECTION").unwrap_or_default(),
            per_action_cap_lamports: env("COVENANT_METAPLEX_PER_ACTION_CAP_LAMPORTS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            allow: env("COVENANT_METAPLEX_ALLOW").map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            }),
        }
    }

    /// Whether the read tools (DAS queries) should be advertised: the
    /// profile is on and a DAS endpoint is configured.
    pub fn reads_enabled(&self) -> bool {
        self.enabled && !self.das_url.is_empty()
    }

    /// Whether the write tools (mint/attest) should be advertised: the
    /// profile is on and both the signer sidecar and an RPC are set.
    pub fn writes_enabled(&self) -> bool {
        self.enabled && !self.signer_binary.is_empty() && !self.rpc_url.is_empty()
    }

    /// Whether `slug` is permitted by the allowlist.
    pub fn allows(&self, slug: &str) -> bool {
        match &self.allow {
            None => true,
            Some(list) => list.iter().any(|s| s == slug),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled_read_only_devnet() {
        let c = MetaplexConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.cluster, "devnet");
        assert!(!c.reads_enabled());
        assert!(!c.writes_enabled());
    }

    #[test]
    fn reads_need_endpoint_writes_need_signer_and_rpc() {
        let mut c = MetaplexConfig {
            enabled: true,
            ..Default::default()
        };
        assert!(!c.reads_enabled(), "no das_url yet");
        c.das_url = "https://das.example".into();
        assert!(c.reads_enabled());
        assert!(!c.writes_enabled(), "no signer/rpc yet");
        c.signer_binary = "/bin/covenant-metaplex-signer".into();
        assert!(!c.writes_enabled(), "still no rpc");
        c.rpc_url = "https://api.devnet.solana.com".into();
        assert!(c.writes_enabled());
    }

    #[test]
    fn allowlist_none_allows_all_some_is_exact() {
        let mut c = MetaplexConfig::default();
        assert!(c.allows("das.get_asset"));
        c.allow = Some(vec!["das.get_asset".into()]);
        assert!(c.allows("das.get_asset"));
        assert!(!c.allows("attest.audit_root"));
        c.allow = Some(vec![]);
        assert!(!c.allows("das.get_asset"));
    }

    #[test]
    fn program_id_pin_covers_the_five_surfaces() {
        assert_eq!(ALLOWED_PROGRAM_IDS.len(), 5);
        assert!(ALLOWED_PROGRAM_IDS.contains(&MPL_CORE_PROGRAM_ID));
        assert!(ALLOWED_PROGRAM_IDS.contains(&MPL_BUBBLEGUM_PROGRAM_ID));
    }

    #[test]
    fn config_round_trips_through_serde_with_defaults() {
        let c: MetaplexConfig = serde_json::from_value(serde_json::json!({ "enabled": true })).unwrap();
        assert!(c.enabled);
        assert_eq!(c.cluster, "devnet");
        assert_eq!(c.per_action_cap_lamports, 0);
        assert!(c.allow.is_none());
    }
}
