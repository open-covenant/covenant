use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

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

pub const DEFAULT_SAID_MAINNET_PROGRAM_ID: &str = "5dpw6KEQPn248pnkkaYyWfHwu2nfb3LUMbTucb6LaA8G";
pub const DEFAULT_SAID_DEVNET_PROGRAM_ID: &str = "ESPreFucjVwtDmZbhtL3JLJ9VxCethNEYtosMQhkcurv";
pub const DEFAULT_SAID_API_BASE_URL: &str = "https://api.saidprotocol.com";

pub const DEFAULT_WORKER_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_REST_TIMEOUT: Duration = Duration::from_secs(15);

fn parse_bool(value: Option<&str>) -> bool {
    match value.map(str::trim).map(str::to_ascii_lowercase) {
        Some(s) => matches!(s.as_str(), "1" | "true" | "yes"),
        None => false,
    }
}

fn default_program_id(cluster: Cluster) -> &'static str {
    match cluster {
        Cluster::Mainnet => DEFAULT_SAID_MAINNET_PROGRAM_ID,
        Cluster::Devnet | Cluster::Localnet => DEFAULT_SAID_DEVNET_PROGRAM_ID,
    }
}

fn default_rpc_url(cluster: Cluster) -> &'static str {
    match cluster {
        Cluster::Mainnet => "https://api.mainnet-beta.solana.com",
        Cluster::Devnet => "https://api.devnet.solana.com",
        Cluster::Localnet => "http://127.0.0.1:8899",
    }
}

fn default_worker_command() -> Vec<String> {
    vec!["covenant-said-worker".to_owned()]
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct PaidGates {
    pub register: bool,
    pub verify: bool,
    pub anchor: bool,
    pub validate_work: bool,
}

impl PaidGates {
    pub fn any(&self) -> bool {
        self.register || self.verify || self.anchor || self.validate_work
    }

    pub fn summary(&self) -> String {
        let mut on = Vec::new();
        if self.register { on.push("register"); }
        if self.verify { on.push("verify"); }
        if self.anchor { on.push("anchor"); }
        if self.validate_work { on.push("validate_work"); }
        if on.is_empty() { "none".into() } else { on.join(",") }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub enabled: bool,
    pub cluster: Cluster,
    pub program_id: String,
    pub rpc_url: String,
    pub api_base_url: String,
    pub paid: PaidGates,
    pub worker_command: Vec<String>,
    pub worker_timeout: Duration,
    pub rest_timeout: Duration,
}

impl Config {
    pub fn disabled(cluster: Cluster) -> Self {
        Self {
            enabled: false,
            cluster,
            program_id: String::new(),
            rpc_url: String::new(),
            api_base_url: String::new(),
            paid: PaidGates::default(),
            worker_command: default_worker_command(),
            worker_timeout: DEFAULT_WORKER_TIMEOUT,
            rest_timeout: DEFAULT_REST_TIMEOUT,
        }
    }

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
            get(&format!("COVENANT_SAID_{upper}_ENABLED")).or_else(|| get("COVENANT_SAID_ENABLED")),
        );

        let program_id = get(&format!("COVENANT_SAID_{upper}_PROGRAM_ID"))
            .or_else(|| get("COVENANT_SAID_PROGRAM_ID"))
            .map(str::to_owned)
            .unwrap_or_else(|| default_program_id(cluster).to_owned());

        let rpc_url = get(&format!("COVENANT_SAID_{upper}_RPC_URL"))
            .or_else(|| get("COVENANT_SAID_RPC_URL"))
            .map(str::to_owned)
            .unwrap_or_else(|| default_rpc_url(cluster).to_owned());

        let api_base_url = get("COVENANT_SAID_API_BASE_URL")
            .map(str::to_owned)
            .unwrap_or_else(|| DEFAULT_SAID_API_BASE_URL.to_owned());

        let paid = PaidGates {
            register: parse_bool(get("COVENANT_SAID_ALLOW_PAID_REGISTER")),
            verify: parse_bool(get("COVENANT_SAID_ALLOW_PAID_VERIFY")),
            anchor: parse_bool(get("COVENANT_SAID_ALLOW_PAID_ANCHOR")),
            validate_work: parse_bool(get("COVENANT_SAID_ALLOW_PAID_VALIDATE")),
        };

        let worker_command = get("COVENANT_SAID_WORKER_CMD")
            .map(|s| s.split_whitespace().map(str::to_owned).collect::<Vec<_>>())
            .filter(|parts| !parts.is_empty())
            .unwrap_or_else(default_worker_command);

        let worker_timeout = get("COVENANT_SAID_WORKER_TIMEOUT_SECS")
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|secs| *secs > 0)
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_WORKER_TIMEOUT);

        let rest_timeout = get("COVENANT_SAID_REST_TIMEOUT_SECS")
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|secs| *secs > 0)
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_REST_TIMEOUT);

        Self {
            enabled,
            cluster,
            program_id,
            rpc_url,
            api_base_url,
            paid,
            worker_command,
            worker_timeout,
            rest_timeout,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    #[test]
    fn disabled_by_default() {
        let cfg = Config::from_env(env(&[]));
        assert!(!cfg.enabled);
        assert!(!cfg.paid.any());
        assert_eq!(cfg.cluster, Cluster::Devnet);
        assert_eq!(cfg.program_id, DEFAULT_SAID_DEVNET_PROGRAM_ID);
    }

    #[test]
    fn mainnet_default_program_id() {
        let cfg = Config::from_env(env(&[("COVENANT_SOLANA_CLUSTER", "mainnet")]));
        assert_eq!(cfg.program_id, DEFAULT_SAID_MAINNET_PROGRAM_ID);
    }

    #[test]
    fn cluster_specific_overrides_global_program_id() {
        let cfg = Config::from_env(env(&[
            ("COVENANT_SOLANA_CLUSTER", "devnet"),
            ("COVENANT_SAID_ENABLED", "true"),
            ("COVENANT_SAID_PROGRAM_ID", "global-id"),
            ("COVENANT_SAID_DEVNET_PROGRAM_ID", "devnet-id"),
        ]));
        assert!(cfg.enabled);
        assert_eq!(cfg.program_id, "devnet-id");
    }

    #[test]
    fn paid_gates_individually() {
        let cfg = Config::from_env(env(&[
            ("COVENANT_SAID_ALLOW_PAID_ANCHOR", "1"),
            ("COVENANT_SAID_ALLOW_PAID_VERIFY", "true"),
        ]));
        assert!(cfg.paid.anchor);
        assert!(cfg.paid.verify);
        assert!(!cfg.paid.register);
        assert!(!cfg.paid.validate_work);
        assert_eq!(cfg.paid.summary(), "verify,anchor");
    }

    #[test]
    fn rejects_unknown_cluster() {
        let err = "mainnent".parse::<Cluster>().unwrap_err();
        assert!(err.contains("mainnent"));
    }

    #[test]
    fn api_base_overridable() {
        let cfg = Config::from_env(env(&[(
            "COVENANT_SAID_API_BASE_URL",
            "https://staging.saidprotocol.test",
        )]));
        assert_eq!(cfg.api_base_url, "https://staging.saidprotocol.test");
    }
}
