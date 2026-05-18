use serde::{Deserialize, Serialize};
use url::Url;

/// Local-side configuration for the gitlawb bridge.
///
/// `enabled` defaults to `false`. With the bridge disabled every network entry
/// point returns [`crate::GitlawbError::Disabled`] without touching the
/// transport, the daemon must stay fully usable offline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// When `false`, the bridge refuses to make any network call.
    pub enabled: bool,

    /// Base URL of the gitlawb node (e.g. `http://localhost:7545`).
    pub node_url: Url,

    /// Optional explicit clock skew tolerance for signature validation, in
    /// seconds. `None` falls back to the protocol default (300s).
    pub clock_skew_secs: Option<i64>,
}

impl Config {
    /// Disabled config pinned to localhost, used as a safe default and as the
    /// canonical "off" state in tests.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            node_url: Url::parse("http://localhost:7545").expect("static url parses"),
            clock_skew_secs: None,
        }
    }

    pub fn enabled(node_url: Url) -> Self {
        Self {
            enabled: true,
            node_url,
            clock_skew_secs: None,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::disabled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled_localhost() {
        let c = Config::default();
        assert!(!c.enabled);
        assert_eq!(c.node_url.as_str(), "http://localhost:7545/");
    }

    #[test]
    fn enabled_constructor_flips_flag() {
        let u = Url::parse("https://node.example.com").unwrap();
        let c = Config::enabled(u);
        assert!(c.enabled);
    }
}
