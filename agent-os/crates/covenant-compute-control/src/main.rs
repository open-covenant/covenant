use std::sync::Arc;

use covenant_compute_control::{serve, ServerConfig, StartupError, VastBackend};

#[tokio::main]
async fn main() -> Result<(), StartupError> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "covenant_compute_control=info".into()),
        )
        .init();

    let config = ServerConfig::from_environment()?;
    if config.provider != "vast" {
        return Err(StartupError::ProviderNotLinked(config.provider));
    }
    let provider =
        VastBackend::from_environment().map_err(|_| StartupError::ProviderConfiguration)?;
    serve(config, Arc::new(provider)).await
}
