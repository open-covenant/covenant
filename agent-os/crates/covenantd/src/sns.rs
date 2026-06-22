//! Daemon glue for the SNS profile.
//!
//! `covenant-sns` owns `.sol` name resolution over the hosted SNS resolver.
//! Read-only: no key, no signer, no `solana-sdk`. The daemon holds only the
//! config and a shared resolver client, built once at startup.

use std::sync::Arc;

use covenant_sns::{HttpSnsResolver, SnsConfig, SnsResolver};

/// Materialised SNS profile: config plus a resolver client, shared behind an
/// `Arc`. An empty resolver URL yields a client that refuses every call, so
/// the daemon can build this unconditionally and let config gate the tools.
pub struct SnsState {
    pub config: SnsConfig,
    pub resolver: Arc<dyn SnsResolver>,
}

impl SnsState {
    pub fn new(config: SnsConfig) -> Self {
        let resolver: Arc<dyn SnsResolver> =
            Arc::new(HttpSnsResolver::new(config.resolver_url.clone()));
        Self { config, resolver }
    }
}
