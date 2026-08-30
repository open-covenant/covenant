use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use covenant_compute::AppCatalog;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::signal;
use tokio::time::{self, Duration};

use crate::{
    router, AuthConfigError, AuthRegistry, ControlPlane, ProviderBackend, RecoveryReport,
    SqliteStore,
};

pub struct ServerConfig {
    pub bind: SocketAddr,
    pub database_path: PathBuf,
    pub provider: String,
    pub auth: Arc<AuthRegistry>,
}

impl ServerConfig {
    pub fn from_environment() -> Result<Self, StartupError> {
        let database_path = required("COVENANT_COMPUTE_DATABASE_PATH")?.into();
        let provider = required("COVENANT_COMPUTE_PROVIDER")?;
        let credential_json = required("COVENANT_COMPUTE_BETA_TOKENS_JSON")?;
        let auth = Arc::new(AuthRegistry::from_json(&credential_json)?);
        let bind = env::var("COVENANT_COMPUTE_BIND")
            .unwrap_or_else(|_| "127.0.0.1:8787".into())
            .parse()
            .map_err(|_| StartupError::InvalidBind)?;
        Ok(Self {
            bind,
            database_path,
            provider,
            auth,
        })
    }
}

pub async fn serve(
    config: ServerConfig,
    provider: Arc<dyn ProviderBackend>,
) -> Result<(), StartupError> {
    if config.provider.trim().is_empty() {
        return Err(StartupError::MissingConfiguration(
            "COVENANT_COMPUTE_PROVIDER",
        ));
    }
    let store = SqliteStore::open(&config.database_path).await?;
    let control = ControlPlane::new(AppCatalog::builtin(), store, provider);
    // One unrecoverable job must not keep the service from starting; the
    // reconciler retries every pass.
    report_recovery(
        "compute startup reconciliation completed",
        control.recover().await,
    );
    // A first launch fails silently when no offer clears the configured
    // ceilings, so probe once at startup rather than at the first request.
    match control.offers().await {
        Ok(offers) => tracing::info!(offers = offers.len(), "compute offer probe completed"),
        Err(error) => tracing::warn!(%error, "compute offer probe failed"),
    }
    let reconciler = control.clone();
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(15));
        interval.tick().await;
        loop {
            interval.tick().await;
            report_recovery(
                "compute reconciliation pass completed",
                reconciler.recover().await,
            );
        }
    });
    let listener = TcpListener::bind(config.bind)
        .await
        .map_err(|_| StartupError::Bind)?;
    tracing::info!(address = %config.bind, provider = %config.provider, "compute control plane listening");
    axum::serve(listener, router(config.auth, control))
        .with_graceful_shutdown(shutdown())
        .await
        .map_err(|_| StartupError::Serve)
}

fn report_recovery(message: &'static str, result: Result<RecoveryReport, crate::ServiceError>) {
    match result {
        Ok(report) if report.reconciled > 0 || report.deferred > 0 || report.released > 0 => {
            tracing::info!(
                jobs_reconciled = report.reconciled,
                jobs_deferred = report.deferred,
                allocations_released = report.released,
                "{message}"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::error!(%error, "compute reconciliation pass failed"),
    }
}

/// Deploys send SIGTERM; draining in flight requests keeps a launch from being
/// killed between allocating a machine and recording it.
async fn shutdown() {
    let interrupt = async {
        let _ = signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => tracing::error!(%error, "SIGTERM handler could not be installed"),
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
    tracing::info!("compute control plane draining in-flight requests");
}

fn required(name: &'static str) -> Result<String, StartupError> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or(StartupError::MissingConfiguration(name))
}

#[derive(Debug, Error)]
pub enum StartupError {
    #[error("required configuration {0} is missing")]
    MissingConfiguration(&'static str),
    #[error("compute bind address is invalid")]
    InvalidBind,
    #[error("beta credential configuration is invalid")]
    Auth(#[from] AuthConfigError),
    #[error("durable compute database could not be opened")]
    Store(#[from] crate::StoreError),
    #[error("compute recovery failed")]
    Recovery(#[from] crate::ServiceError),
    #[error("compute listener could not bind")]
    Bind,
    #[error("compute server failed")]
    Serve,
    #[error("configured production provider is not linked: {0}")]
    ProviderNotLinked(String),
    #[error(transparent)]
    ProviderConfiguration(#[from] crate::VastBackendConfigError),
}
