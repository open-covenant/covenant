//! Myrad connector configuration.
//!
//! One switch. `enabled` registers the verification tool; `false` (the default)
//! leaves the connector inert. The integrity thresholds are not here: issuance
//! takes an [`crate::integrity::IntegrityPolicy`] directly, so the bar a receipt
//! was issued under travels in the receipt rather than in a config a reader of
//! the receipt cannot see.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MyradConfig {
    /// Off by default: no tool is registered until a deployment sets this true.
    #[serde(default)]
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_off() {
        assert!(!MyradConfig::default().enabled);
    }

    #[test]
    fn an_empty_object_deserializes_to_the_default() {
        let c: MyradConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(c, MyradConfig::default());
    }
}
