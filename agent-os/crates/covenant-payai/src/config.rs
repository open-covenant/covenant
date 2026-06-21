use crate::PAYAI_SOLANA_FEE_PAYER;

/// Daemon-side PayAI config. Off by default; built from env, advertises tools
/// only when `enabled`.
#[derive(Debug, Clone)]
pub struct PayAiConfig {
    pub enabled: bool,
    /// Solana RPC URL the indexer reads settlements from.
    pub rpc_url: String,
    /// Override the watched facilitator fee-payer (defaults to PayAI's).
    pub fee_payer: Option<String>,
    /// How many recent signatures to scan per reputation read.
    pub limit: usize,
}

impl Default for PayAiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            rpc_url: String::new(),
            fee_payer: None,
            limit: 50,
        }
    }
}

impl PayAiConfig {
    /// `COVENANT_PAYAI_ENABLED` gates it; RPC url falls back to the shared
    /// `COVENANT_SOLANA_RPC_URL` when no PayAI-specific one is set.
    pub fn from_env() -> Self {
        let enabled = std::env::var("COVENANT_PAYAI_ENABLED")
            .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);
        let rpc_url = std::env::var("COVENANT_PAYAI_RPC_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("COVENANT_SOLANA_RPC_URL").ok())
            .unwrap_or_default();
        let fee_payer = std::env::var("COVENANT_PAYAI_FEE_PAYER")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let limit = std::env::var("COVENANT_PAYAI_LIMIT")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(50);
        Self {
            enabled,
            rpc_url,
            fee_payer,
            limit,
        }
    }

    /// Fee-payer to watch: the override, or PayAI's default.
    pub fn fee_payer(&self) -> &str {
        self.fee_payer.as_deref().unwrap_or(PAYAI_SOLANA_FEE_PAYER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled_and_uses_payai_fee_payer() {
        let c = PayAiConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.fee_payer(), PAYAI_SOLANA_FEE_PAYER);
        assert_eq!(c.limit, 50);
    }

    #[test]
    fn fee_payer_override_wins() {
        let c = PayAiConfig {
            fee_payer: Some("OverrideWallet".into()),
            ..Default::default()
        };
        assert_eq!(c.fee_payer(), "OverrideWallet");
    }
}
