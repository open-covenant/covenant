//! Krexa provider configuration.
//!
//! Two independent switches. `enabled` turns on the read-only oracle and
//! the `krexa.score` tool. `credit_enabled` gates credit-backed x402 draws
//! and stays off until Krexa ships an audit, real vault TVL, and a custody
//! answer (own signer vs Krexa PDA). The credit code is built and tested
//! behind this flag, never live. See the crate doc for the risk split.

use serde::{Deserialize, Serialize};

/// Krexa backend (the credit/score REST API). Same host serves the
/// score, eligibility, credit-line, and oracle endpoints.
pub const BASE_URL: &str = "https://tcredit-backend.onrender.com/api/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KrexaConfig {
    /// Master switch for the read-only oracle + the `krexa.score` tool.
    /// `false` means no Krexa call ever leaves the host.
    #[serde(default)]
    pub enabled: bool,
    /// REST base URL. Endpoint paths are built against this.
    #[serde(default = "default_base_url")]
    pub base_url: String,
    /// Phase 1 gate. When `false` (the default), credit-backed draws are
    /// refused even if the code path is reached. Never set this true until
    /// the custody + audit + TVL questions are resolved.
    #[serde(default)]
    pub credit_enabled: bool,
}

fn default_base_url() -> String {
    BASE_URL.to_string()
}

impl Default for KrexaConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_base_url(),
            credit_enabled: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_fully_off_with_prod_host() {
        let c = KrexaConfig::default();
        assert!(!c.enabled);
        assert!(!c.credit_enabled, "credit must default OFF");
        assert_eq!(c.base_url, BASE_URL);
    }

    #[test]
    fn enabling_reads_does_not_enable_credit() {
        let c: KrexaConfig =
            serde_json::from_value(serde_json::json!({ "enabled": true })).unwrap();
        assert!(c.enabled);
        assert!(!c.credit_enabled, "reads on must not turn credit on");
    }
}
