use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use covenant_indexer::api::{router, AppState};
use covenant_indexer::config::AppConfig;
use covenant_indexer::model::seed_events;
use covenant_indexer::verified::VerifiedState;
use covenant_indexer::x402_gate::{X402Config, X402Gate};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = AppConfig::from_env();
    let events = seed_events(&config.cluster, &config.program_id);

    let x402 = X402Config::from_env().map(X402Gate::new);
    let x402_enabled = x402.is_some();

    let verified = VerifiedState::from_env();
    if let Some(v) = &verified {
        info!(issuer = %v.issuer(), "covenant-verified enabled; attesting zauth directory");
        v.spawn_refresher();
    }
    let verified_enabled = verified.is_some();

    let state = AppState {
        cluster: config.cluster.clone(),
        rpc_url: config.solana_rpc_url.clone(),
        confirmations: config.confirmations,
        events: Arc::new(events),
        x402,
        verified,
    };

    let app = router(state);
    let address: SocketAddr = config
        .bind_addr
        .parse()
        .with_context(|| format!("invalid bind addr: {}", config.bind_addr))?;
    let listener = TcpListener::bind(address).await?;
    info!(
        address = %config.bind_addr,
        cluster = %config.cluster,
        mode = "fixture",
        x402 = x402_enabled,
        verified = verified_enabled,
        "covenant indexer up; serving seeded events (no Solana RPC subscriber wired yet)"
    );
    axum::serve(listener, app).await?;
    Ok(())
}
