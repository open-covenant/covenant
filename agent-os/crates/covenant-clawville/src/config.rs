//! ClawVille profile configuration.
//!
//! The bounty-verification tools are pure compute — no keys, no network, no
//! spend — so the only switches are a master enable and a per-tool
//! allowlist. Mirrors the gating shape of the Metaplex/Hyre profiles so the
//! daemon wires it the same way.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ClawvilleConfig {
    /// Master switch. `false` means no `clawville.*` tool is registered.
    #[serde(default)]
    pub enabled: bool,
    /// Tool allowlist by short slug (`bounty.verify`, …). `None` allows
    /// every tool; `Some([])` allows none.
    #[serde(default)]
    pub allow: Option<Vec<String>>,
}

fn truthy(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

impl ClawvilleConfig {
    /// Build from `COVENANT_CLAWVILLE_*` environment variables.
    pub fn from_env() -> Self {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            enabled: env("COVENANT_CLAWVILLE_ENABLED")
                .map(|v| truthy(&v))
                .unwrap_or(false),
            allow: env("COVENANT_CLAWVILLE_ALLOW").map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            }),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
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
    fn default_is_disabled() {
        let c = ClawvilleConfig::default();
        assert!(!c.enabled());
    }

    #[test]
    fn allowlist_none_allows_all_some_is_exact() {
        let mut c = ClawvilleConfig::default();
        assert!(c.allows("bounty.verify"));
        c.allow = Some(vec!["bounty.verify".into()]);
        assert!(c.allows("bounty.verify"));
        assert!(!c.allows("bounty.open"));
        c.allow = Some(vec![]);
        assert!(!c.allows("bounty.verify"));
    }

    #[test]
    fn round_trips_through_serde_with_defaults() {
        let c: ClawvilleConfig =
            serde_json::from_value(serde_json::json!({ "enabled": true })).unwrap();
        assert!(c.enabled);
        assert!(c.allow.is_none());
    }
}
