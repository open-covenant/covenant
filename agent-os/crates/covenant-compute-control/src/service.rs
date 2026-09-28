//! The control plane's orchestration facade. The web layer speaks only to
//! this type: it turns an authenticated request into catalog reads and,
//! for a launch, into a signed lease against the coordinator's engine,
//! then projects the engine's answer back onto the customer job shape.
//! The money, the market and the meter live in the coordinator; this
//! layer holds the catalog and the per-owner bookkeeping the engine does
//! not.

use std::collections::HashSet;
use std::sync::Arc;

use thiserror::Error;
use uuid::Uuid;

use crate::auth::Principal;
use crate::catalog::AppCatalog;
use crate::plan::{PlanRejection, ResolveRejection};
use crate::provider::{ProviderBackend, ProviderError};
use crate::store::{JobStore, Reserved, StoreError};
use crate::wire::{ComputeApp, ComputeJob, ComputeOffer, LaunchPlan, LaunchRequest};

/// The live control plane, cloned into every request handler.
#[derive(Clone)]
pub struct ControlPlane {
    catalog: AppCatalog,
    provider: Arc<dyn ProviderBackend>,
    store: Arc<JobStore>,
}

impl ControlPlane {
    pub fn new(catalog: AppCatalog, provider: Arc<dyn ProviderBackend>) -> Self {
        Self {
            catalog,
            provider,
            store: Arc::new(JobStore::new()),
        }
    }

    /// The apps a caller may launch. Served as-is; a launch re-checks the
    /// plan against this same catalog before any money moves.
    pub fn apps(&self) -> &[ComputeApp] {
        self.catalog.apps()
    }

    /// The GPU supply on offer right now, screened so a client never sees
    /// an offer it cannot name in a launch.
    pub async fn offers(&self) -> Result<Vec<ComputeOffer>, ServiceError> {
        conforming_offers(self.provider.offers().await?)
    }

    /// Resolves a launch request against the live market into a committed
    /// plan, moving no money. The plan names the cheapest offer that clears
    /// the app's floors and the caller's budget and trust request, priced
    /// exactly as a launch will be — so the caller reviews the concrete
    /// offer and ceiling, then submits it. The market may only invalidate
    /// the plan by moving underneath it before the launch, which [`submit`]
    /// catches as a stale offer.
    ///
    /// [`submit`]: ControlPlane::submit
    pub async fn plan(&self, request: &LaunchRequest) -> Result<LaunchPlan, ServiceError> {
        let offers = self.offers().await?;
        Ok(crate::plan::resolve_plan(&self.catalog, &offers, request)?)
    }

    /// Launches a job for a beta owner. A repeated idempotency key returns
    /// the recorded job untouched; a fresh one is checked against the
    /// catalog and the live market, has its spend reserved against the
    /// owner's cap, and only then opens a lease. A launch the provider
    /// refuses becomes a visible failed job; a launch that never reaches
    /// the provider releases its reservation.
    pub async fn submit(
        &self,
        principal: &Principal,
        idempotency_key: &str,
        plan: LaunchPlan,
    ) -> Result<ComputeJob, ServiceError> {
        validate_idempotency_key(idempotency_key)?;
        if let Some(job) = self.store.replay(&principal.id, idempotency_key, &plan)? {
            return Ok(job);
        }
        self.check_plan(&plan).await?;
        let job_id = match self.store.reserve(
            &principal.id,
            principal.spend_cap_usdc_micros,
            idempotency_key,
            &plan,
        )? {
            Reserved::Fresh(job_id) => job_id,
            Reserved::Existing(job) => return Ok(*job),
        };
        match self.provider.launch(job_id, &plan).await {
            Ok(job) => Ok(self.store.record(job_id, job)?),
            Err(ProviderError::Rejected) => Ok(self.store.fail(job_id, "provider_rejected")?),
            Err(error) => {
                self.store.release(job_id);
                Err(ServiceError::Provider(error))
            }
        }
    }

    /// Every job the owner launched, newest state last recorded. A listing
    /// carries no access credential and is not refreshed against the
    /// coordinator; a client reads one job for its live state.
    pub fn jobs(&self, principal: &Principal) -> Vec<ComputeJob> {
        self.store.jobs(&principal.id)
    }

    /// One of the owner's jobs, refreshed against the coordinator while it
    /// is still live. A terminal job is returned as recorded; if the
    /// coordinator cannot be reached, the last recorded state stands rather
    /// than the read failing.
    pub async fn job(&self, principal: &Principal, id: &str) -> Result<ComputeJob, ServiceError> {
        let job_id = validate_job_id(id)?;
        let (plan, cached) = self.store.get(&principal.id, job_id)?;
        if cached.status.terminal() {
            return Ok(cached);
        }
        match self.provider.poll(job_id, &plan).await {
            Ok(job) => Ok(self.store.record(job_id, job)?),
            Err(ProviderError::Unavailable) => Ok(cached),
            Err(error) => Err(ServiceError::Provider(error)),
        }
    }

    /// Ends one of the owner's live sessions and returns its settled
    /// account. A job already terminal is returned untouched; closing is
    /// safe to repeat.
    pub async fn cancel(
        &self,
        principal: &Principal,
        id: &str,
    ) -> Result<ComputeJob, ServiceError> {
        let job_id = validate_job_id(id)?;
        let (plan, cached) = self.store.get(&principal.id, job_id)?;
        if cached.status.terminal() {
            return Ok(cached);
        }
        match self.provider.cancel(job_id, &plan).await {
            Ok(job) => Ok(self.store.record(job_id, job)?),
            Err(ProviderError::Unavailable) => Ok(cached),
            Err(error) => Err(ServiceError::Provider(error)),
        }
    }

    /// Checks a plan against the catalog and the live market. The catalog
    /// checks are deterministic; the last one — that the offer is still on
    /// the market — needs the current offer set, so it lives here rather
    /// than in [`crate::plan`].
    async fn check_plan(&self, plan: &LaunchPlan) -> Result<(), ServiceError> {
        crate::plan::validate_plan(&self.catalog, plan)?;
        let offers = self.offers().await?;
        if !offers.iter().any(|offer| offer == &plan.offer) {
            return Err(ServiceError::StaleOffer);
        }
        Ok(())
    }
}

/// Screens the provider's offers: an id a launch can name, a described
/// GPU, a non-zero rate, and no duplicates. One malformed offer is dropped
/// and logged; losing every offer to malformation is a provider fault
/// worth surfacing rather than serving an empty market as if the supply
/// had simply gone quiet.
fn conforming_offers(offers: Vec<ComputeOffer>) -> Result<Vec<ComputeOffer>, ServiceError> {
    let mut ids = HashSet::new();
    let supplied = offers.len();
    let retained: Vec<ComputeOffer> = offers
        .into_iter()
        .filter(|offer| {
            let valid = !offer.id.is_empty()
                && offer.id.len() <= 200
                && !offer.gpu.model.trim().is_empty()
                && offer.gpu.vram_mib != 0
                && offer.rate_usdc_micros_per_hour != 0
                && ids.insert(offer.id.clone());
            if !valid {
                tracing::warn!(offer_id = %offer.id, "dropping malformed provider offer");
            }
            valid
        })
        .collect();
    if retained.is_empty() && supplied > 0 {
        return Err(ServiceError::InvalidProviderOffers);
    }
    Ok(retained)
}

fn validate_idempotency_key(value: &str) -> Result<(), ServiceError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(ServiceError::InvalidIdempotencyKey);
    }
    Ok(())
}

fn validate_job_id(value: &str) -> Result<Uuid, ServiceError> {
    Uuid::parse_str(value).map_err(|_| ServiceError::InvalidJobId)
}

/// Why the control plane could not answer a request. Each arm carries the
/// customer consequence; [`crate::web`] maps it to a status and a stable
/// code.
#[derive(Debug, Error)]
pub enum ServiceError {
    #[error(transparent)]
    InvalidPlan(#[from] PlanRejection),
    #[error(transparent)]
    Unresolvable(#[from] ResolveRejection),
    #[error("the selected offer is no longer on the market")]
    StaleOffer,
    #[error("the idempotency key is invalid")]
    InvalidIdempotencyKey,
    #[error("the job id is invalid")]
    InvalidJobId,
    #[error("the compute provider is unavailable")]
    Provider(#[from] ProviderError),
    #[error("the compute provider returned no usable offers")]
    InvalidProviderOffers,
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{GpuSpec, TrustClass};

    fn offer(id: &str, model: &str, vram_mib: u64, rate_usdc_micros_per_hour: u64) -> ComputeOffer {
        ComputeOffer {
            id: id.into(),
            gpu: GpuSpec {
                model: model.into(),
                vram_mib,
                cuda_major: 12,
            },
            rate_usdc_micros_per_hour,
            trust_class: TrustClass::Open,
            online: true,
        }
    }

    fn ids(offers: &[ComputeOffer]) -> Vec<&str> {
        offers.iter().map(|o| o.id.as_str()).collect()
    }

    #[test]
    fn a_mixed_market_keeps_only_the_offers_a_launch_can_name() {
        // The production case: real supply arrives interleaved with every
        // malformed shape the screen exists to drop. The good offers survive
        // in market order, and nothing a launch cannot name or price leaks
        // through to the buyer.
        let retained = conforming_offers(vec![
            offer("rtx-4090", "rtx-4090", 24_576, 1_000_000),
            offer("", "a100", 81_920, 3_000_000), // no id to name
            offer("blank-gpu", "   ", 24_576, 1_000_000), // no described gpu
            offer("no-vram", "h100", 0, 2_000_000), // vram unstated
            offer("free-h100", "h100", 81_920, 0), // a $0 rate
            offer(&"x".repeat(201), "l40s", 49_152, 1_500_000), // id past the cap
            offer("a100-80g", "a100-80g", 81_920, 3_000_000),
        ])
        .expect("a market with conforming supply is served");
        assert_eq!(ids(&retained), ["rtx-4090", "a100-80g"]);
    }

    #[test]
    fn a_zero_rate_offer_never_reaches_a_buyer() {
        // Money is real buyer revenue: a $0/hr offer, priced and committed,
        // rents a machine for nothing. The rate screen is the only thing
        // between a mispriced provider row and a launch that quotes it.
        let err = conforming_offers(vec![offer("free-h100", "h100", 81_920, 0)])
            .expect_err("an all-$0 market carries no usable supply");
        assert!(matches!(err, ServiceError::InvalidProviderOffers));
    }

    #[test]
    fn duplicate_ids_collapse_to_the_first_seen() {
        // A launch names its offer by id, so the set a buyer sees must be
        // unique. Two rows sharing an id keep the first — a defined,
        // order-stable choice, not whichever the provider happened to send
        // second.
        let retained = conforming_offers(vec![
            offer("a100-80g", "a100-80g", 81_920, 3_000_000),
            offer("a100-80g", "a100-80g", 81_920, 2_000_000),
        ])
        .expect("one offer survives the duplicate");
        assert_eq!(ids(&retained), ["a100-80g"]);
        assert_eq!(
            retained[0].rate_usdc_micros_per_hour, 3_000_000,
            "the first-seen row is the one retained"
        );
    }

    #[test]
    fn a_dropped_malformed_offer_does_not_reserve_its_id_for_a_valid_twin() {
        // The dedupe check runs last and short-circuits, so only an offer
        // that clears every other screen records its id. A malformed row can
        // share an id with a later good one without stealing it; reorder the
        // screen so the id is claimed first and this real offer would vanish.
        let retained = conforming_offers(vec![
            offer("h100", "h100", 0, 1_000_000), // same id, no vram → dropped
            offer("h100", "h100", 81_920, 1_000_000),
        ])
        .expect("the valid twin survives");
        assert_eq!(ids(&retained), ["h100"]);
        assert_eq!(retained[0].gpu.vram_mib, 81_920);
    }

    #[test]
    fn an_all_malformed_market_is_a_provider_fault_but_a_quiet_one_is_not() {
        // Losing every offer to malformation is the provider misbehaving and
        // is surfaced as such; a market that supplied nothing is simply
        // quiet and returns an empty list rather than an error.
        let all_bad = conforming_offers(vec![offer("", "h100", 81_920, 1_000_000)]);
        assert!(matches!(all_bad, Err(ServiceError::InvalidProviderOffers)));

        let quiet = conforming_offers(vec![]).expect("an empty market is not a fault");
        assert!(quiet.is_empty());
    }
}
