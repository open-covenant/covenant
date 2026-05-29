//! Bridge configuration.
//!
//! Mirrors `resolveSynapseConfig` in `packages/config/networks.mjs`.
//! The program ID and RPC URL live in config only — never inlined in
//! consumer crates — so a SAP redeploy never requires rebuilding the
//! daemon.

use std::collections::HashMap;
use std::time::Duration;

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

    fn upper(self) -> &'static str {
        match self {
            Cluster::Devnet => "DEVNET",
            Cluster::Localnet => "LOCALNET",
            Cluster::Mainnet => "MAINNET",
        }
    }
}

impl std::str::FromStr for Cluster {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "devnet" => Ok(Cluster::Devnet),
            "localnet" => Ok(Cluster::Localnet),
            "mainnet" | "mainnet-beta" => Ok(Cluster::Mainnet),
            other => Err(format!("unknown cluster: {other}")),
        }
    }
}

/// Mainnet+devnet SAP deployment as of 2026-05. Override via env when
/// the program redeploys — never edit consumer crates to chase a new
/// address.
pub const DEFAULT_SYNAPSE_PROGRAM_ID: &str = "SAPpUhsWLJG1FfkGRcXagEDMrMsWGjbky7AyhGpFETZ";

const DEFAULT_EXPLORER_URL: &str = "https://explorer.oobeprotocol.ai";

fn default_rpc_url(cluster: Cluster) -> &'static str {
    match cluster {
        Cluster::Mainnet => "https://api.mainnet-beta.solana.com",
        Cluster::Devnet => "https://api.devnet.solana.com",
        Cluster::Localnet => "http://127.0.0.1:8899",
    }
}

fn parse_bool(value: Option<&str>) -> bool {
    match value.map(str::trim).map(str::to_ascii_lowercase) {
        Some(s) => matches!(s.as_str(), "1" | "true" | "yes"),
        None => false,
    }
}

/// Maximum wall-clock budget for one worker subprocess invocation. The
/// previous code awaited `wait_with_output` with no timeout, so a hung
/// SAP worker (network stalls, RPC saturation, a deadlocked SDK call)
/// blocked the daemon reconciliation/attestation task indefinitely —
/// violating the contract that the SAP bridge must never block the
/// offline daemon. 30s is comfortably above a healthy round-trip and
/// well below any supervisor-level deadline.
pub const DEFAULT_WORKER_TIMEOUT: Duration = Duration::from_secs(30);

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
    /// Command (program followed by args) used to spawn the TypeScript
    /// bridge worker. Defaults to the installed `covenant-sap-worker`
    /// bin; override via `COVENANT_SAP_WORKER_CMD`.
    pub worker_command: Vec<String>,
    /// Per-call wall-clock budget for `worker::invoke`. Override via
    /// `COVENANT_SAP_WORKER_TIMEOUT_SECS`. Set high enough to cover a
    /// healthy `attest-root` round-trip (~5s on devnet) but low enough
    /// that a stalled subprocess can't pin the daemon's reconciliation
    /// task across a maintenance window.
    pub worker_timeout: Duration,
}

/// Default command used to invoke the TypeScript bridge worker. The
/// `@covenant/sap-bridge` package installs it as `covenant-sap-worker`;
/// operators with a non-standard layout override it via
/// `COVENANT_SAP_WORKER_CMD` (whitespace-separated, e.g.
/// `node /opt/covenant/sap-worker.js`).
fn default_worker_command() -> Vec<String> {
    vec!["covenant-sap-worker".to_owned()]
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
            worker_command: default_worker_command(),
            worker_timeout: DEFAULT_WORKER_TIMEOUT,
        }
    }

    /// Resolve from a map of env-style values. Mirrors the layering
    /// in `resolveSynapseConfig` on the TS side: cluster-specific env
    /// override beats the global one, and the bridge is disabled by
    /// default so the daemon stays offline when no operator has
    /// opted in.
    pub fn from_env<I, K, V>(env: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let map: HashMap<String, String> = env
            .into_iter()
            .map(|(k, v)| (k.as_ref().to_string(), v.as_ref().to_string()))
            .collect();
        let get = |key: &str| map.get(key).map(String::as_str);

        let cluster = get("COVENANT_SOLANA_CLUSTER")
            .and_then(|s| s.parse::<Cluster>().ok())
            .unwrap_or(Cluster::Devnet);
        let upper = cluster.upper();

        let enabled = parse_bool(
            get(&format!("COVENANT_SAP_{upper}_ENABLED")).or_else(|| get("COVENANT_SAP_ENABLED")),
        );

        let program_id = get(&format!("COVENANT_SAP_{upper}_PROGRAM_ID"))
            .or_else(|| get("COVENANT_SAP_PROGRAM_ID"))
            .map(str::to_owned)
            .unwrap_or_else(|| DEFAULT_SYNAPSE_PROGRAM_ID.to_owned());

        let rpc_url = get(&format!("COVENANT_SAP_{upper}_RPC_URL"))
            .or_else(|| get("COVENANT_SAP_RPC_URL"))
            .map(str::to_owned)
            .unwrap_or_else(|| default_rpc_url(cluster).to_owned());

        let explorer_url = get("COVENANT_SAP_EXPLORER_URL")
            .map(str::to_owned)
            .unwrap_or_else(|| DEFAULT_EXPLORER_URL.to_owned());

        let worker_command = get("COVENANT_SAP_WORKER_CMD")
            .map(|s| s.split_whitespace().map(str::to_owned).collect::<Vec<_>>())
            .filter(|parts| !parts.is_empty())
            .unwrap_or_else(default_worker_command);

        let worker_timeout = get("COVENANT_SAP_WORKER_TIMEOUT_SECS")
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|secs| *secs > 0)
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_WORKER_TIMEOUT);

        Self {
            enabled,
            cluster,
            program_id,
            rpc_url,
            explorer_url,
            worker_command,
            worker_timeout,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn disabled_by_default_with_no_env() {
        let cfg = Config::from_env(env(&[]));
        assert!(!cfg.enabled);
        assert_eq!(cfg.cluster, Cluster::Devnet);
        assert_eq!(cfg.program_id, DEFAULT_SYNAPSE_PROGRAM_ID);
    }

    #[test]
    fn parses_mainnet_beta_alias() {
        let cfg = Config::from_env(env(&[("COVENANT_SOLANA_CLUSTER", "mainnet-beta")]));
        assert_eq!(cfg.cluster, Cluster::Mainnet);
    }

    #[test]
    fn cluster_specific_program_id_beats_global() {
        let cfg = Config::from_env(env(&[
            ("COVENANT_SOLANA_CLUSTER", "devnet"),
            ("COVENANT_SAP_ENABLED", "true"),
            ("COVENANT_SAP_PROGRAM_ID", "global-program"),
            ("COVENANT_SAP_DEVNET_PROGRAM_ID", "devnet-program"),
        ]));
        assert!(cfg.enabled);
        assert_eq!(cfg.program_id, "devnet-program");
    }

    #[test]
    fn falls_back_to_cluster_default_rpc() {
        let cfg = Config::from_env(env(&[
            ("COVENANT_SOLANA_CLUSTER", "mainnet"),
            ("COVENANT_SAP_ENABLED", "1"),
        ]));
        assert_eq!(cfg.rpc_url, "https://api.mainnet-beta.solana.com");
    }

    #[test]
    fn explorer_url_overridable() {
        let cfg = Config::from_env(env(&[(
            "COVENANT_SAP_EXPLORER_URL",
            "https://staging.synapse.test",
        )]));
        assert_eq!(cfg.explorer_url, "https://staging.synapse.test");
    }

    #[test]
    fn parses_truthy_enabled() {
        for value in ["1", "true", "TRUE", "yes"] {
            let cfg = Config::from_env(env(&[("COVENANT_SAP_ENABLED", value)]));
            assert!(cfg.enabled, "value={value}");
        }
    }

    #[test]
    fn parses_falsy_enabled() {
        for value in ["0", "false", "no", ""] {
            let cfg = Config::from_env(env(&[("COVENANT_SAP_ENABLED", value)]));
            assert!(!cfg.enabled, "value={value}");
        }
    }
}
