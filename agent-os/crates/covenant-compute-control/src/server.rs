//! Booting the control plane: reading its configuration from the
//! environment, standing the HTTP surface up over the engine-backed
//! provider, and draining in-flight requests on shutdown so a launch is
//! never killed between opening a lease and recording it.

use std::env;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use covenant_compute_buyer::BuyerConfig;
use covenant_identity::LocalIdentity;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::signal;

use crate::provider::ProviderBackend;
use crate::{
    router, AppAvailability, AppCatalog, AuthConfigError, AuthRegistry, CatalogConfigError,
    ControlPlane, EngineProvider,
};

/// The default per-lease poll cadence a buyer client reads the coordinator
/// with. The control plane polls on demand, so this only bounds the
/// buyer library's own waits.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

pub struct ServerConfig {
    pub bind: SocketAddr,
    pub auth: Arc<AuthRegistry>,
    pub catalog: AppCatalog,
}

impl ServerConfig {
    pub fn from_environment() -> Result<Self, StartupError> {
        let credential_json = required("COVENANT_COMPUTE_BETA_TOKENS_JSON")?;
        let auth = Arc::new(AuthRegistry::from_json(&credential_json)?);
        let bind = env::var("COVENANT_COMPUTE_BIND")
            .unwrap_or_else(|_| "127.0.0.1:8787".into())
            .parse()
            .map_err(|_| StartupError::InvalidBind)?;
        let catalog = match env::var("COVENANT_COMPUTE_CATALOG_JSON") {
            Ok(json) if !json.trim().is_empty() => AppCatalog::from_json(&json)?,
            _ => AppCatalog::builtin(),
        };
        Ok(Self {
            bind,
            auth,
            catalog,
        })
    }
}

/// Builds the engine-backed provider from the environment: the coordinator
/// it buys from, the buyer identity that funds every lease, and the
/// per-lease price ceiling that identity will not exceed.
pub fn engine_provider_from_environment() -> Result<EngineProvider, StartupError> {
    let coordinator_url = required("COVENANT_COMPUTE_COORDINATOR_URL")?;
    let identity_path = required("COVENANT_COMPUTE_BUYER_IDENTITY")?;
    let identity =
        LocalIdentity::load_or_create(Path::new(&identity_path), "covenant-compute-control")
            .map_err(|_| StartupError::Identity)?;
    let max_price_micro_usdc = required("COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC")?
        .parse()
        .map_err(|_| StartupError::InvalidPrice)?;
    let min_reputation_bps = match env::var("COVENANT_COMPUTE_MIN_REPUTATION_BPS") {
        Ok(value) if !value.trim().is_empty() => {
            Some(value.parse().map_err(|_| StartupError::InvalidReputation)?)
        }
        _ => None,
    };
    let config = BuyerConfig {
        coordinator_url,
        poll_interval: POLL_INTERVAL,
        referral_code: None,
        rpc_url: None,
    };
    Ok(EngineProvider::new(
        config,
        identity,
        max_price_micro_usdc,
        min_reputation_bps,
    ))
}

pub async fn serve(
    config: ServerConfig,
    provider: Arc<dyn ProviderBackend>,
) -> Result<(), StartupError> {
    let control = ControlPlane::new(config.catalog, provider);
    // Confirm the catalog a deployment configured at startup, and warn when
    // it holds nothing a customer can launch — every entry previewed — so a
    // misconfigured catalog surfaces at boot rather than as an empty market.
    let apps = control.apps();
    let available = apps
        .iter()
        .filter(|app| app.availability == AppAvailability::Available)
        .count();
    if available == 0 {
        tracing::warn!(
            apps = apps.len(),
            "compute catalog has no launchable apps; every entry is preview-only"
        );
    } else {
        tracing::info!(apps = apps.len(), available, "compute catalog loaded");
    }
    // A first launch fails quietly when no offer clears the ceilings, so
    // probe the market once at startup rather than at the first request.
    match control.offers().await {
        Ok(offers) => tracing::info!(offers = offers.len(), "compute offer probe completed"),
        Err(error) => tracing::warn!(%error, "compute offer probe failed"),
    }
    let listener = TcpListener::bind(config.bind)
        .await
        .map_err(|_| StartupError::Bind)?;
    tracing::info!(address = %config.bind, "compute control plane listening");
    axum::serve(listener, router(config.auth, control))
        .with_graceful_shutdown(shutdown())
        .await
        .map_err(|_| StartupError::Serve)
}

/// Deploys send SIGTERM; draining in-flight requests keeps a launch from
/// being killed between opening a lease and recording it.
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
    #[error("the compute catalog configuration is invalid")]
    Catalog(#[from] CatalogConfigError),
    #[error("the buyer identity could not be loaded")]
    Identity,
    #[error("the per-lease price ceiling is invalid")]
    InvalidPrice,
    #[error("the reputation floor is invalid")]
    InvalidReputation,
    #[error("compute listener could not bind")]
    Bind,
    #[error("compute server failed")]
    Serve,
}
