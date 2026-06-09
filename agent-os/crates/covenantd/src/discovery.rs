//! Daemon glue for x402 provider discovery.
//!
//! Wraps the existing [`covenant_x402::OrbitClient`] in a lazily-refreshed
//! cache. The first `Request::DiscoverProviders` after startup populates
//! the catalog (no fetch happens at boot, so a daemon with discovery
//! enabled still starts offline); subsequent calls reuse the cached
//! catalog until it ages past the refresh interval.
//!
//! Discovery is read-only: it never signs, pays, or touches the budget.
//! The handler in [`crate::Server`] gates it behind the `x402.discover`
//! capability and the optional operator allowlist held here.

use std::time::{Duration, Instant};

use covenant_ipc::DiscoveredProvider;
use covenant_x402::{Catalog, OrbitClient, RegistryEntry};
use tokio::sync::Mutex;
use tracing::{debug, warn};

/// Cached orbit-x402 catalog plus the policy for serving it.
///
/// Held on [`crate::Server`] behind `Option<Arc<DiscoveryState>>`. The
/// catalog starts empty and is filled on the first discovery call; a
/// refresh failure leaves the previous catalog in place so a transient
/// registry outage degrades to stale-but-usable rather than empty.
pub struct DiscoveryState {
    client: OrbitClient,
    refresh_interval: Duration,
    /// Optional operator pin: only providers whose `server_title` is in
    /// this set are ever returned. `None` means no pin (every provider
    /// the registry lists is eligible).
    allow: Option<Vec<String>>,
    cache: Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    catalog: Option<Catalog>,
    fetched_at: Option<Instant>,
}

impl DiscoveryState {
    pub fn new(
        client: OrbitClient,
        refresh_interval: Duration,
        allow: Option<Vec<String>>,
    ) -> Self {
        let allow = allow.filter(|a| !a.is_empty());
        Self {
            client,
            refresh_interval,
            allow,
            cache: Mutex::new(Cache::default()),
        }
    }

    /// The cache TTL: how long a fetched catalog is served before the
    /// next discovery call triggers a refresh.
    pub fn refresh_interval(&self) -> Duration {
        self.refresh_interval
    }

    /// The operator `server_title` allowlist, or `None` when no pin is
    /// configured (every provider eligible).
    pub fn allow(&self) -> Option<&[String]> {
        self.allow.as_deref()
    }

    /// Whether a `server_title` passes the operator allowlist. No pin
    /// configured means everything passes.
    fn allowed(&self, server_title: &str) -> bool {
        match &self.allow {
            Some(list) => list.iter().any(|t| t == server_title),
            None => true,
        }
    }

    /// Return the providers matching `filter` (an exact `server_title`),
    /// refreshing the cached catalog first when it is empty or stale.
    ///
    /// On a refresh fetch failure with a populated cache, the stale
    /// catalog is served and the error logged; with an empty cache the
    /// error propagates so the caller can surface it.
    pub async fn providers(
        &self,
        filter: Option<&str>,
    ) -> Result<Vec<DiscoveredProvider>, covenant_x402::X402Error> {
        let mut cache = self.cache.lock().await;
        let stale = match cache.fetched_at {
            Some(at) => at.elapsed() >= self.refresh_interval,
            None => true,
        };
        if stale {
            match self.client.fetch_all().await {
                Ok(entries) => {
                    debug!(count = entries.len(), "orbit-x402 catalog refreshed");
                    cache.catalog = Some(Catalog::new(entries));
                    cache.fetched_at = Some(Instant::now());
                }
                Err(e) if cache.catalog.is_some() => {
                    warn!(error = %e, "orbit-x402 refresh failed; serving stale catalog");
                }
                Err(e) => return Err(e),
            }
        }

        let catalog = cache
            .catalog
            .as_ref()
            .expect("catalog populated after a successful refresh or preserved stale entry");
        let providers = catalog
            .iter()
            .filter(|e| filter.is_none_or(|t| e.server_title == t))
            .filter(|e| self.allowed(&e.server_title))
            .map(project)
            .collect();
        Ok(providers)
    }
}

/// Project a registry entry to the IPC wire struct, lifting the first
/// pricing option (the registry lists one per `(network, asset)` rail;
/// the first is the canonical quote for display). A free endpoint (no
/// pricing) leaves the price/network/asset fields absent.
fn project(entry: &RegistryEntry) -> DiscoveredProvider {
    let pricing = entry.pricing.first();
    DiscoveredProvider {
        server_title: entry.server_title.clone(),
        endpoint: entry.endpoint.clone(),
        slug: entry.slug.clone(),
        method: entry.method.clone(),
        price_atomic_usdc: pricing.map(|p| p.amount.clone()),
        network: pricing.map(|p| p.network.clone()),
        asset: pricing.map(|p| p.asset.clone()),
    }
}

/// Test seam: build a `DiscoveryState` whose cache is pre-populated so
/// `providers` serves it without a live fetch. The `fetched_at` is set
/// to now so the interval gate treats it as fresh.
#[cfg(test)]
pub fn seeded(
    entries: Vec<RegistryEntry>,
    allow: Option<Vec<String>>,
) -> std::sync::Arc<DiscoveryState> {
    let state = DiscoveryState::new(OrbitClient::new(), Duration::from_secs(3600), allow);
    {
        let mut cache = state
            .cache
            .try_lock()
            .expect("fresh state has no contended lock");
        cache.catalog = Some(Catalog::new(entries));
        cache.fetched_at = Some(Instant::now());
    }
    std::sync::Arc::new(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_x402::{PaymentRequirements, RegistryEntry};

    fn entry(server: &str, slug: &str, priced: bool) -> RegistryEntry {
        RegistryEntry {
            server_url: format!("https://api.{}.test", server.to_lowercase()),
            server_title: server.into(),
            endpoint: format!("https://api.{}.test/{slug}", server.to_lowercase()),
            slug: slug.into(),
            method: "POST".into(),
            description: "test endpoint".into(),
            pricing: if priced {
                vec![PaymentRequirements {
                    network: "solana:mainnet".into(),
                    asset: "usdc-sol".into(),
                    amount: "80000".into(),
                    amount_usdc: 0.08,
                    pay_to: "pay-to-addr".into(),
                    scheme: "exact".into(),
                    extra: None,
                }]
            } else {
                vec![]
            },
        }
    }

    #[tokio::test]
    async fn seeded_catalog_projects_priced_and_free_entries() {
        let state = seeded(
            vec![
                entry("Xona", "image/creative-director", true),
                entry("Xona", "free/ping", false),
            ],
            None,
        );
        let providers = state.providers(None).await.expect("seeded serve");
        assert_eq!(providers.len(), 2);
        let priced = &providers[0];
        assert_eq!(priced.price_atomic_usdc.as_deref(), Some("80000"));
        assert_eq!(priced.network.as_deref(), Some("solana:mainnet"));
        let free = &providers[1];
        assert!(free.price_atomic_usdc.is_none());
        assert!(free.network.is_none());
        assert!(free.asset.is_none());
    }

    #[tokio::test]
    async fn server_title_filter_narrows_to_one_provider() {
        let state = seeded(
            vec![
                entry("Xona", "a", true),
                entry("Hyre", "b", true),
                entry("Xona", "c", true),
            ],
            None,
        );
        let xona = state.providers(Some("Xona")).await.unwrap();
        assert_eq!(xona.len(), 2);
        assert!(xona.iter().all(|p| p.server_title == "Xona"));
        let none = state.providers(Some("Nonexistent")).await.unwrap();
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn operator_allowlist_drops_unlisted_servers() {
        let state = seeded(
            vec![entry("Xona", "a", true), entry("Hyre", "b", true)],
            Some(vec!["Xona".into()]),
        );
        // No filter, but the allowlist pins to Xona only.
        let all = state.providers(None).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].server_title, "Xona");
        // A filter for an off-allowlist server yields nothing even though
        // the registry carries it.
        let hyre = state.providers(Some("Hyre")).await.unwrap();
        assert!(hyre.is_empty());
    }
}
