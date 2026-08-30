use std::error::Error;
use std::process::ExitCode;
use std::sync::Arc;

use covenant_compute_control::{serve, ServerConfig, StartupError, VastBackend};

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "covenant_compute_control=info".into()),
        )
        .init();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Returning Err from main would print the Debug form, which hides
            // every message these errors carry.
            eprintln!("covenant-compute-control: {error}");
            let mut cause = error.source();
            while let Some(error) = cause {
                eprintln!("  caused by: {error}");
                cause = error.source();
            }
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), StartupError> {
    let config = ServerConfig::from_environment()?;
    if config.provider != "vast" {
        return Err(StartupError::ProviderNotLinked(config.provider));
    }
    serve(config, Arc::new(VastBackend::from_environment()?)).await
}
