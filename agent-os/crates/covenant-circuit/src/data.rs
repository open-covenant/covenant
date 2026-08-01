use std::sync::Arc;

use serde_json::Value;

use crate::capability::{CircuitCapability, SpendLedger};
use crate::payer::CircPayer;
use crate::x402::{Paid, PaidRequest, X402};
use crate::{circ, CircuitError, Result};

/// The Circuit Data API: 40+ market and on-chain endpoints. Free endpoints (quote,
/// status, prices) answer 200; the rest answer 402 and settle in CIRC. [`get`](Self::get)
/// is the generic escape hatch that covers any endpoint; the named methods wrap the common
/// calls with their real paths.
pub struct DataClient {
    x402: X402,
    base: String,
}

impl DataClient {
    pub fn new(
        payer: Arc<dyn CircPayer>,
        cap: CircuitCapability,
        ledger: Arc<SpendLedger>,
    ) -> Self {
        Self::from_x402(X402::new(payer, cap, ledger))
    }

    pub fn with_client_builder(
        builder: reqwest::ClientBuilder,
        payer: Arc<dyn CircPayer>,
        cap: CircuitCapability,
        ledger: Arc<SpendLedger>,
    ) -> Result<Self> {
        Ok(Self::from_x402(X402::with_client_builder(
            builder, payer, cap, ledger,
        )?))
    }

    pub fn from_x402(x402: X402) -> Self {
        Self {
            x402,
            base: circ::DATA_BASE.to_string(),
        }
    }

    pub fn with_base_url(mut self, base: impl Into<String>) -> Self {
        self.base = base.into().trim_end_matches('/').to_string();
        self
    }

    /// Generic GET against any data path, with query params. Runs the x402 loop and
    /// returns the JSON body.
    pub async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        Ok(self.get_paid(path, query).await?.body)
    }

    /// Like [`get`](Self::get) but also surfaces the settling signature (if the endpoint
    /// charged) so a caller can record what a data pull cost.
    pub async fn get_paid(&self, path: &str, query: &[(&str, String)]) -> Result<Paid> {
        let url = reqwest::Url::parse_with_params(&format!("{}{}", self.base, path), query)
            .map_err(|e| CircuitError::BadUrl(format!("{}{}: {e}", self.base, path)))?;
        self.x402.send(PaidRequest::get(url.to_string())).await
    }

    // Free endpoints (200, no charge) — live pricing for every endpoint, and status.
    pub async fn quote(&self) -> Result<Value> {
        self.get("/api/quote", &[]).await
    }
    pub async fn status(&self) -> Result<Value> {
        self.get("/api/status", &[]).await
    }
    pub async fn prices(&self, mints: &[&str]) -> Result<Value> {
        self.get("/api/prices", &[("mints", mints.join(","))]).await
    }

    // Token endpoints.
    pub async fn token_price(&self, mint: &str) -> Result<Value> {
        self.get("/api/token-price", &[("mint", mint.into())]).await
    }
    pub async fn token_info(&self, mint: &str) -> Result<Value> {
        self.get("/api/token-info", &[("mint", mint.into())]).await
    }
    pub async fn token_holders(&self, mint: &str) -> Result<Value> {
        self.get("/api/token-holders", &[("mint", mint.into())])
            .await
    }
    pub async fn token_security(&self, mint: &str) -> Result<Value> {
        self.get("/api/token-security", &[("mint", mint.into())])
            .await
    }
    pub async fn token_top_traders(&self, mint: &str) -> Result<Value> {
        self.get("/api/token-top-traders", &[("mint", mint.into())])
            .await
    }
    pub async fn token_trending(&self) -> Result<Value> {
        self.get("/api/token-trending", &[]).await
    }
    pub async fn scan(&self, mint: &str) -> Result<Value> {
        self.get("/api/scan", &[("mint", mint.into())]).await
    }

    // Wallet endpoints.
    pub async fn wallet_analytics(&self, wallet: &str) -> Result<Value> {
        self.get("/api/wallet-analytics", &[("wallet", wallet.into())])
            .await
    }
    pub async fn wallet_pnl(&self, wallet: &str) -> Result<Value> {
        self.get("/api/wallet-pnl", &[("wallet", wallet.into())])
            .await
    }

    // Market / DeFi / network.
    pub async fn market_overview(&self) -> Result<Value> {
        self.get("/api/market-overview", &[]).await
    }
    pub async fn market_sentiment(&self) -> Result<Value> {
        self.get("/api/market-sentiment", &[]).await
    }
    pub async fn new_tokens(&self) -> Result<Value> {
        self.get("/api/new-tokens", &[]).await
    }
    pub async fn defi_overview(&self) -> Result<Value> {
        self.get("/api/defi-overview", &[]).await
    }
    pub async fn yields(&self) -> Result<Value> {
        self.get("/api/yields", &[]).await
    }
    pub async fn network_stats(&self) -> Result<Value> {
        self.get("/api/network-stats", &[]).await
    }
    pub async fn news(&self) -> Result<Value> {
        self.get("/api/news", &[]).await
    }
    pub async fn top_pools(&self) -> Result<Value> {
        self.get("/api/top-pools", &[]).await
    }

    // Real-time price feed (Geyser-backed, distinct from the aggregated token endpoints).
    pub async fn sol_price(&self) -> Result<Value> {
        self.get("/api/price-feed/sol-price", &[]).await
    }
    pub async fn live_price(&self, mint: &str) -> Result<Value> {
        self.get(&format!("/api/price-feed/price/{mint}"), &[])
            .await
    }
}
