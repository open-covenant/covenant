//! AceData provider configuration.
//!
//! Non-secret tunables only: the master switch, the API host, the
//! default models, and the tool allowlist. The API key is a billing
//! credential and is handed to [`crate::AceDataClient`] separately, so
//! it never lands in a serializable struct that could be logged or
//! written to disk.

use serde::{Deserialize, Serialize};

/// Production API host.
pub const BASE_URL: &str = "https://api.acedata.cloud";
/// Default Flux image model.
pub const DEFAULT_IMAGE_MODEL: &str = "flux-pro";
/// Default Suno music model.
pub const DEFAULT_MUSIC_MODEL: &str = "chirp-v4";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AceDataConfig {
    /// Master switch. `false` means no AceData tools are registered and
    /// no AceData call ever leaves the host.
    #[serde(default)]
    pub enabled: bool,
    /// API host. Endpoint URLs are built against this.
    #[serde(default = "default_base_url")]
    pub base_url: String,
    /// Default model for `acedata.image.generate`.
    #[serde(default = "default_image_model")]
    pub image_model: String,
    /// Default model for `acedata.music.generate`.
    #[serde(default = "default_music_model")]
    pub music_model: String,
    /// Tool allowlist by name (`acedata.search`). `None` registers every
    /// tool; `Some([])` registers none.
    #[serde(default)]
    pub allow: Option<Vec<String>>,
}

fn default_base_url() -> String {
    BASE_URL.to_string()
}
fn default_image_model() -> String {
    DEFAULT_IMAGE_MODEL.to_string()
}
fn default_music_model() -> String {
    DEFAULT_MUSIC_MODEL.to_string()
}

impl Default for AceDataConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_base_url(),
            image_model: default_image_model(),
            music_model: default_music_model(),
            allow: None,
        }
    }
}

impl AceDataConfig {
    /// Whether `tool` is permitted by the allowlist.
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
        let c = AceDataConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.base_url, BASE_URL);
        assert_eq!(c.image_model, DEFAULT_IMAGE_MODEL);
        assert_eq!(c.music_model, DEFAULT_MUSIC_MODEL);
    }

    #[test]
    fn allowlist_none_allows_all_some_is_exact() {
        let mut c = AceDataConfig::default();
        assert!(c.allows("acedata.search"));
        c.allow = Some(vec!["acedata.search".into()]);
        assert!(c.allows("acedata.search"));
        assert!(!c.allows("acedata.image.generate"));
        c.allow = Some(vec![]);
        assert!(!c.allows("acedata.search"));
    }

    #[test]
    fn config_round_trips_through_serde_with_defaults() {
        let json = serde_json::json!({ "enabled": true });
        let c: AceDataConfig = serde_json::from_value(json).unwrap();
        assert!(c.enabled);
        assert_eq!(c.base_url, BASE_URL);
        assert_eq!(c.music_model, DEFAULT_MUSIC_MODEL);
        assert_eq!(c.allow, None);
    }
}
