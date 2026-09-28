use std::error::Error;
use std::process::ExitCode;
use std::sync::Arc;

use covenant_compute_control::{
    engine_provider_from_environment, serve, ServerConfig, StartupError,
};

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
            // Returning Err from main prints the Debug form, which hides the
            // message each error carries; walk the chain by hand instead.
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
    let provider = engine_provider_from_environment()?;
    serve(config, Arc::new(provider)).await
}
