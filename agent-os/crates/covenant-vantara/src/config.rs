//! Vantara connector configuration.
//!
//! Non-secret tunables only. There is no API key: the explorer feeds are
//! public and read-only, so the connector holds no credential to protect.

use serde::{Deserialize, Serialize};

/// Production explorer host.
pub const BASE_URL: &str = "https://www.vantara.services";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VantaraConfig {
    /// Master switch. `false` registers no Vantara tools.
    #[serde(default)]
    pub enabled: bool,
    /// Explorer host. Feed URLs are built against this.
    #[serde(default = "default_base_url")]
    pub base_url: String,
    /// Tool allowlist by name (`vantara.jobs`). `None` registers every
    /// tool; `Some([])` registers none.
    #[serde(default)]
    pub allow: Option<Vec<String>>,
}

fn default_base_url() -> String {
    BASE_URL.to_string()
}

impl Default for VantaraConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_base_url(),
            allow: None,
        }
    }
}

impl VantaraConfig {
    /// Read config from `COVENANT_VANTARA_*` — `ENABLED` (1/true), `BASE_URL`,
    /// and `ALLOW` (comma-separated tool names).
    pub fn from_env() -> Self {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let mut cfg = Self::default();
        if let Some(v) = env("COVENANT_VANTARA_ENABLED") {
            cfg.enabled = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Some(v) = env("COVENANT_VANTARA_BASE_URL") {
            cfg.base_url = v;
        }
        cfg.allow = env("COVENANT_VANTARA_ALLOW").map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        });
        cfg
    }

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
    fn default_is_disabled_with_prod_host() {
        let c = VantaraConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.base_url, BASE_URL);
        assert_eq!(c.allow, None);
    }

    #[test]
    fn allowlist_none_allows_all_some_is_exact() {
        let mut c = VantaraConfig::default();
        assert!(c.allows("vantara.jobs"));
        c.allow = Some(vec!["vantara.jobs".into()]);
        assert!(c.allows("vantara.jobs"));
        assert!(!c.allows("vantara.attest"));
        c.allow = Some(vec![]);
        assert!(!c.allows("vantara.jobs"));
    }

    #[test]
    fn config_round_trips_with_defaults() {
        let c: VantaraConfig =
            serde_json::from_value(serde_json::json!({ "enabled": true })).unwrap();
        assert!(c.enabled);
        assert_eq!(c.base_url, BASE_URL);
        assert_eq!(c.allow, None);
    }
}
