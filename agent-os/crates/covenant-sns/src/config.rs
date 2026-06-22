//! SNS profile configuration.
//!
//! Read-only and keyless by default. Flip `enabled = true` and the resolve
//! tools come online against the hosted SNS resolver. No signer, no RPC, no
//! spend in Phase 1 — the most a misconfigured daemon can do is read public
//! name records.

use serde::{Deserialize, Serialize};

use crate::DEFAULT_RESOLVER_URL;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnsConfig {
    /// Master switch. `false` means no `sns.*` tool is registered.
    #[serde(default)]
    pub enabled: bool,
    /// Hosted SNS REST resolver base URL. Defaults to the public proxy.
    #[serde(default = "default_resolver")]
    pub resolver_url: String,
    /// Tool allowlist by short slug (`resolve`, `reverse`, `record`). `None`
    /// allows every enabled tool; `Some([])` allows none.
    #[serde(default)]
    pub allow: Option<Vec<String>>,
}

fn default_resolver() -> String {
    DEFAULT_RESOLVER_URL.to_string()
}

impl Default for SnsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            resolver_url: default_resolver(),
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

impl SnsConfig {
    /// Build from `COVENANT_SNS_*` environment variables.
    pub fn from_env() -> Self {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            enabled: env("COVENANT_SNS_ENABLED")
                .map(|v| truthy(&v))
                .unwrap_or(false),
            resolver_url: env("COVENANT_SNS_RESOLVER_URL").unwrap_or_else(default_resolver),
            allow: env("COVENANT_SNS_ALLOW").map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            }),
        }
    }

    /// Read tools are advertised when the profile is on and a resolver is set.
    pub fn reads_enabled(&self) -> bool {
        self.enabled && !self.resolver_url.is_empty()
    }

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
    fn default_is_disabled_with_resolver_set() {
        let c = SnsConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.resolver_url, DEFAULT_RESOLVER_URL);
        assert!(!c.reads_enabled());
    }

    #[test]
    fn reads_enabled_once_on() {
        let c = SnsConfig {
            enabled: true,
            ..Default::default()
        };
        assert!(c.reads_enabled());
    }

    #[test]
    fn allowlist_none_allows_all_some_is_exact() {
        let mut c = SnsConfig::default();
        assert!(c.allows("resolve"));
        c.allow = Some(vec!["resolve".into()]);
        assert!(c.allows("resolve"));
        assert!(!c.allows("reverse"));
        c.allow = Some(vec![]);
        assert!(!c.allows("resolve"));
    }

    #[test]
    fn config_round_trips_through_serde_with_defaults() {
        let c: SnsConfig = serde_json::from_value(serde_json::json!({ "enabled": true })).unwrap();
        assert!(c.enabled);
        assert_eq!(c.resolver_url, DEFAULT_RESOLVER_URL);
        assert!(c.allow.is_none());
    }
}
