//! Invoica provider configuration.
//!
//! Carries only non-secret tunables. The API key is read from env and handed
//! to the [`crate::InvoicaClient`] separately, so it never lands on a
//! serializable struct that could be logged or written to disk.

use serde::{Deserialize, Serialize};

/// Invoica's REST base, serving the Bearer-key invoice API.
pub const DEFAULT_BASE_URL: &str = "https://api.invoica.ai";

/// Settlement chain stamped on new invoices. Invoica's published SDK `Chain`
/// type is EVM-only, but its backend `openapi.json` whitelists
/// `base|polygon|arbitrum|skale|solana`, and Covenant's x402 rail is Solana, so
/// the connector defaults there.
pub const DEFAULT_CHAIN: &str = "solana";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvoicaConfig {
    /// Master switch. `false` registers no tools and makes no calls.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    /// Chain stamped on new invoices when the caller does not name one.
    #[serde(default = "default_chain")]
    pub chain: String,
}

fn default_base_url() -> String {
    DEFAULT_BASE_URL.to_string()
}

fn default_chain() -> String {
    DEFAULT_CHAIN.to_string()
}

impl Default for InvoicaConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_base_url(),
            chain: default_chain(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_off_solana_prod_host() {
        let c = InvoicaConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.base_url, DEFAULT_BASE_URL);
        assert_eq!(c.chain, "solana");
    }

    #[test]
    fn config_carries_no_secret() {
        let json = serde_json::to_string(&InvoicaConfig::default()).unwrap();
        assert!(
            !json.to_lowercase().contains("key"),
            "config leaks a key field"
        );
    }
}
