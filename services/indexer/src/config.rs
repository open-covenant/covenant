#[derive(Debug, Clone)]
pub struct AppConfig {
    pub bind_addr: String,
    pub solana_rpc_url: String,
    pub cluster: String,
    pub program_id: String,
    pub confirmations: u64,
}

impl AppConfig {
    pub fn from_env() -> Self {
        Self {
            bind_addr: std::env::var("INDEXER_BIND_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
            solana_rpc_url: std::env::var("COVENANT_SOLANA_RPC_URL")
                .unwrap_or_else(|_| "https://api.devnet.solana.com".to_string()),
            cluster: std::env::var("COVENANT_SOLANA_CLUSTER")
                .unwrap_or_else(|_| "devnet".to_string()),
            program_id: std::env::var("COVENANT_PROTOCOL_PROGRAM_ID")
                .unwrap_or_else(|_| "CovntSettLement1111111111111111111111111111".to_string()),
            confirmations: std::env::var("COVENANT_SOLANA_CONFIRMATIONS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(32),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AppConfig;

    // The five keys from_env reads. Read nowhere else in the crate's tests, so a
    // single sequential test that owns them is race-free under cargo's parallel runner.
    const KEYS: [&str; 5] = [
        "INDEXER_BIND_ADDR",
        "COVENANT_SOLANA_RPC_URL",
        "COVENANT_SOLANA_CLUSTER",
        "COVENANT_PROTOCOL_PROGRAM_ID",
        "COVENANT_SOLANA_CONFIRMATIONS",
    ];

    fn clear_env() {
        for key in KEYS {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn from_env_defaults_overrides_and_confirmations_parsing() {
        clear_env();

        let cfg = AppConfig::from_env();
        assert_eq!(cfg.bind_addr, "0.0.0.0:8080");
        assert_eq!(cfg.solana_rpc_url, "https://api.devnet.solana.com");
        assert_eq!(cfg.cluster, "devnet");
        assert_eq!(cfg.program_id, "CovntSettLement1111111111111111111111111111");
        assert_eq!(cfg.confirmations, 32);

        // Non-numeric confirmations falls back to the default via the parse chain.
        std::env::set_var("COVENANT_SOLANA_CONFIRMATIONS", "garbage");
        assert_eq!(AppConfig::from_env().confirmations, 32);

        // Numeric confirmations is parsed and used.
        std::env::set_var("COVENANT_SOLANA_CONFIRMATIONS", "64");
        assert_eq!(AppConfig::from_env().confirmations, 64);

        // Each remaining var honors an explicit override.
        std::env::remove_var("COVENANT_SOLANA_CONFIRMATIONS");
        std::env::set_var("INDEXER_BIND_ADDR", "127.0.0.1:9000");
        std::env::set_var("COVENANT_SOLANA_RPC_URL", "https://rpc.example");
        std::env::set_var("COVENANT_SOLANA_CLUSTER", "mainnet-beta");
        std::env::set_var("COVENANT_PROTOCOL_PROGRAM_ID", "ProgramOverride111111111111111111111111111");
        let cfg = AppConfig::from_env();
        assert_eq!(cfg.bind_addr, "127.0.0.1:9000");
        assert_eq!(cfg.solana_rpc_url, "https://rpc.example");
        assert_eq!(cfg.cluster, "mainnet-beta");
        assert_eq!(cfg.program_id, "ProgramOverride111111111111111111111111111");
        assert_eq!(cfg.confirmations, 32);

        clear_env();
    }
}
