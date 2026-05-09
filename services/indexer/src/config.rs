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
