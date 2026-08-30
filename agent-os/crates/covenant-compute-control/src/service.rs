use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use covenant_compute::{
    AppAvailability, AppCatalog, ComputeApp, ComputeJob, ComputeOffer, LaunchPlan,
    MIN_DURATION_SECS,
};
use futures::{stream, StreamExt};
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::auth::Principal;
use crate::provider::{
    ProviderBackend, ProviderCancel, ProviderError, ProviderLaunch, ProviderPoll,
};
use crate::store::{SqliteStore, StoreError, StoredJob, SubmitDisposition};

const RECONCILE_CONCURRENCY: usize = 8;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// In-flight jobs polled successfully, not a failure count.
    pub reconciled: usize,
    pub deferred: usize,
    pub released: usize,
}

#[derive(Clone)]
pub struct ControlPlane {
    catalog: AppCatalog,
    store: SqliteStore,
    provider: Arc<dyn ProviderBackend>,
    pub(crate) launch_guards: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl ControlPlane {
    pub fn new(
        catalog: AppCatalog,
        store: SqliteStore,
        provider: Arc<dyn ProviderBackend>,
    ) -> Self {
        Self {
            catalog,
            store,
            provider,
            launch_guards: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn apps(&self) -> &[ComputeApp] {
        self.catalog.apps()
    }

    pub async fn offers(&self) -> Result<Vec<ComputeOffer>, ServiceError> {
        conforming_offers(self.provider.offers().await?)
    }

    pub async fn submit(
        &self,
        principal: &Principal,
        idempotency_key: &str,
        plan: LaunchPlan,
    ) -> Result<ComputeJob, ServiceError> {
        validate_idempotency_key(idempotency_key)?;
        // A returning caller is resolved against the durable record first.
        // Validating the plan against live offers would answer a retry, or a
        // reused key, with a stale-offer error once the market rotated.
        if let Some(job) = self
            .store
            .replay(&principal.id, idempotency_key, &plan)
            .await?
        {
            return self.dispatch(job, SubmitDisposition::Replay).await;
        }
        self.validate_plan(&plan).await?;
        let submitted = self
            .store
            .submit(
                &principal.id,
                principal.spend_cap_usdc_micros,
                idempotency_key,
                &plan,
                now_ms()?,
            )
            .await?;
        self.dispatch(submitted.job, submitted.disposition).await
    }

    async fn dispatch(
        &self,
        job: StoredJob,
        disposition: SubmitDisposition,
    ) -> Result<ComputeJob, ServiceError> {
        if job.is_terminal() {
            return Ok(job.wire());
        }
        if job.is_cancel_requested() {
            return self.cancel_coordinated(job).await;
        }
        if job.is_prepared() {
            return self.launch_provider(job).await;
        }
        if disposition == SubmitDisposition::Replay {
            return self.refresh(job).await;
        }
        Ok(job.wire())
    }

    pub async fn jobs(&self, principal: &Principal) -> Result<Vec<ComputeJob>, ServiceError> {
        Ok(self
            .store
            .jobs(&principal.id)
            .await?
            .into_iter()
            .map(|job| job.wire())
            .collect())
    }

    pub async fn job(&self, principal: &Principal, id: &str) -> Result<ComputeJob, ServiceError> {
        validate_job_id(id)?;
        let job = self.store.job(&principal.id, id).await?;
        self.refresh(job).await
    }

    pub async fn cancel(
        &self,
        principal: &Principal,
        id: &str,
    ) -> Result<ComputeJob, ServiceError> {
        validate_job_id(id)?;
        let job = self
            .store
            .request_cancel(&principal.id, id, now_ms()?)
            .await?;
        if job.is_terminal() {
            return Ok(job.wire());
        }
        tracing::info!(job_id = %job.id, "compute job cancellation requested");
        self.cancel_coordinated(job).await
    }

    pub async fn recover(&self) -> Result<RecoveryReport, ServiceError> {
        let jobs = self.store.recoverable_jobs().await?;
        let mut report = RecoveryReport::default();
        let mut first_fatal = None;
        let mut results = stream::iter(jobs.into_iter().map(|job| self.reconcile_job(job)))
            .buffer_unordered(RECONCILE_CONCURRENCY);
        while let Some(result) = results.next().await {
            match result {
                Ok(_) => report.reconciled += 1,
                Err(error @ ServiceError::Provider(ProviderError::Configuration)) => {
                    tracing::error!(%error, "compute provider credentials are rejected");
                    first_fatal.get_or_insert(error);
                }
                Err(ServiceError::Provider(_)) => report.deferred += 1,
                Err(error) => {
                    tracing::error!(%error, "compute job reconciliation failed");
                    first_fatal.get_or_insert(error);
                }
            }
        }
        report.released = self.release_orphans().await?;
        first_fatal.map_or(Ok(report), Err)
    }

    /// Tears down allocations left behind by a settled job. Without this the
    /// only teardown attempt is the one made at settlement time.
    async fn release_orphans(&self) -> Result<usize, ServiceError> {
        let mut released = 0;
        for job in self.store.unreleased_jobs().await? {
            match self.release_provider(&job).await {
                Ok(()) => released += 1,
                Err(error) => tracing::error!(
                    job_id = %job.id,
                    %error,
                    "settled job still holds a provider allocation"
                ),
            }
        }
        Ok(released)
    }

    async fn validate_plan(&self, plan: &LaunchPlan) -> Result<(), ServiceError> {
        let catalog_app = self
            .catalog
            .app(&plan.app.id)
            .map_err(|_| PlanRejection::UnknownApp)?;
        if catalog_app != &plan.app {
            return Err(PlanRejection::CatalogMismatch.into());
        }
        if plan.app.availability != AppAvailability::Available || plan.app.image.is_none() {
            return Err(PlanRejection::AppUnavailable.into());
        }
        if plan.duration_secs < MIN_DURATION_SECS || plan.duration_secs > plan.app.max_duration_secs
        {
            return Err(PlanRejection::Duration {
                minimum_secs: MIN_DURATION_SECS,
                maximum_secs: plan.app.max_duration_secs,
            }
            .into());
        }
        if !plan.offer.online {
            return Err(PlanRejection::OfferOffline.into());
        }
        if plan.offer.gpu.vram_mib < plan.app.min_vram_mib {
            return Err(PlanRejection::GpuMemory.into());
        }
        if plan.offer.trust_class < plan.app.min_trust {
            return Err(PlanRejection::TrustClass.into());
        }
        let expected = quote_maximum(plan.offer.rate_usdc_micros_per_hour, plan.duration_secs)
            .ok_or(PlanRejection::OfferRate)?;
        if plan.maximum_usdc_micros != expected {
            return Err(PlanRejection::Maximum {
                expected_usdc_micros: expected,
            }
            .into());
        }

        let offers = self.offers().await?;
        if !offers.iter().any(|offer| offer == &plan.offer) {
            return Err(ServiceError::StaleOffer);
        }
        Ok(())
    }

    async fn launch_provider(&self, job: StoredJob) -> Result<ComputeJob, ServiceError> {
        let launch_guard = self.launch_guard(&job.id).await;
        let guard = launch_guard.lock().await;
        let job = self.store.job(&job.owner, &job.id).await?;
        if job.is_cancel_requested() {
            return self.cancel_provider(job).await;
        }
        let requested_at_ms = now_ms()?;
        if job.deadline_reached(requested_at_ms) {
            let job = self.mark_overdue(job, requested_at_ms).await?;
            return self.cancel_provider(job).await;
        }
        if !job.is_prepared() {
            drop(guard);
            return self.refresh(job).await;
        }
        let request = ProviderLaunch {
            job_id: job.id.clone(),
            idempotency_key: job.idempotency_key.clone(),
            plan: job.plan.clone(),
            clock: job.clock(requested_at_ms),
        };
        match self.provider.launch(request).await {
            Ok(provider_job) => {
                let provider_status = provider_job.status;
                let access_url = provider_job.access_url.clone();
                let recorded = self
                    .store
                    .record_provider(&job.id, provider_job, now_ms()?)
                    .await?;
                if recorded.is_terminal() {
                    if !provider_status.terminal() {
                        if let Err(error) = self.release_provider(&recorded).await {
                            tracing::error!(
                                job_id = %recorded.id,
                                %error,
                                "late launch allocation could not be torn down"
                            );
                        }
                    }
                    return Ok(recorded.wire());
                }
                if recorded.is_cancel_requested() || recorded.deadline_reached(now_ms()?) {
                    let recorded = if recorded.is_cancel_requested() {
                        recorded
                    } else {
                        self.store
                            .request_cancel(&recorded.owner, &recorded.id, now_ms()?)
                            .await?
                    };
                    self.cancel_provider(recorded).await
                } else {
                    Ok(with_access(recorded.wire(), access_url))
                }
            }
            Err(ProviderError::Rejected) => Ok(self
                .store
                .fail_launch(&job.id, "provider_rejected", now_ms()?)
                .await?
                .wire()),
            Err(error) => Err(ServiceError::Provider(error)),
        }
    }

    async fn refresh(&self, job: StoredJob) -> Result<ComputeJob, ServiceError> {
        if job.is_terminal() {
            return Ok(job.wire());
        }
        let requested_at_ms = now_ms()?;
        if job.deadline_reached(requested_at_ms) {
            let job = self.mark_overdue(job, requested_at_ms).await?;
            return self.cancel_coordinated(job).await;
        }
        if job.is_prepared() {
            return Ok(job.wire());
        }
        let Some(provider_job_id) = &job.provider_job_id else {
            return Err(ServiceError::InvalidDurableState);
        };
        match self
            .provider
            .job(ProviderPoll {
                job_id: job.id.clone(),
                provider_job_id: provider_job_id.clone(),
                plan: job.plan.clone(),
                clock: job.clock(requested_at_ms),
            })
            .await
        {
            Ok(provider_job) => {
                let access_url = provider_job.access_url.clone();
                Ok(with_access(
                    self.store
                        .record_provider(&job.id, provider_job, requested_at_ms)
                        .await?
                        .wire(),
                    access_url,
                ))
            }
            Err(ProviderError::Unavailable) => Ok(job.wire()),
            Err(error) => Err(ServiceError::Provider(error)),
        }
    }

    /// Records the deadline before the cancellation it triggers, so a disputed
    /// charge can be read back from the log in the order it happened.
    async fn mark_overdue(
        &self,
        job: StoredJob,
        requested_at_ms: u64,
    ) -> Result<StoredJob, ServiceError> {
        tracing::info!(
            job_id = %job.id,
            duration_secs = job.plan.duration_secs,
            "compute job reached its deadline"
        );
        Ok(self
            .store
            .request_cancel(&job.owner, &job.id, requested_at_ms)
            .await?)
    }

    async fn cancel_provider(&self, job: StoredJob) -> Result<ComputeJob, ServiceError> {
        let requested_at_ms = now_ms()?;
        match self
            .provider
            .cancel(ProviderCancel {
                job_id: job.id.clone(),
                provider_job_id: job.provider_job_id.clone(),
                plan: job.plan.clone(),
                clock: job.clock(requested_at_ms),
            })
            .await
        {
            Ok(provider_job) => {
                if provider_job.status.terminal() {
                    tracing::info!(
                        job_id = %job.id,
                        provider_job_id = %provider_job.id,
                        "compute provider teardown confirmed"
                    );
                }
                let access_url = provider_job.access_url.clone();
                Ok(with_access(
                    self.store
                        .record_provider(&job.id, provider_job, requested_at_ms)
                        .await?
                        .wire(),
                    access_url,
                ))
            }
            Err(ProviderError::Unavailable) => Ok(job.wire()),
            Err(error) => Err(ServiceError::Provider(error)),
        }
    }

    async fn cancel_coordinated(&self, job: StoredJob) -> Result<ComputeJob, ServiceError> {
        let launch_guard = self.launch_guard(&job.id).await;
        let _guard = launch_guard.lock().await;
        let job = self.store.job(&job.owner, &job.id).await?;
        if job.is_terminal() {
            return Ok(job.wire());
        }
        self.cancel_provider(job).await
    }

    async fn release_provider(&self, job: &StoredJob) -> Result<(), ServiceError> {
        let requested_at_ms = now_ms()?;
        let cleanup = self
            .provider
            .cancel(ProviderCancel {
                job_id: job.id.clone(),
                provider_job_id: job.provider_job_id.clone(),
                plan: job.plan.clone(),
                clock: job.clock(requested_at_ms),
            })
            .await?;
        if !cleanup.status.terminal() {
            return Err(ServiceError::Provider(ProviderError::InvalidState));
        }
        self.store.mark_released(&job.id, now_ms()?).await?;
        tracing::info!(
            job_id = %job.id,
            provider_job_id = %cleanup.id,
            "compute provider teardown confirmed after settlement"
        );
        Ok(())
    }

    async fn launch_guard(&self, job_id: &str) -> Arc<Mutex<()>> {
        let mut guards = self.launch_guards.lock().await;
        guards.retain(|_, guard| Arc::strong_count(guard) > 1);
        Arc::clone(
            guards
                .entry(job_id.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }

    async fn reconcile_job(&self, job: StoredJob) -> Result<ComputeJob, ServiceError> {
        let requested_at_ms = now_ms()?;
        if job.deadline_reached(requested_at_ms) && !job.is_cancel_requested() {
            let job = self.mark_overdue(job, requested_at_ms).await?;
            return self.cancel_coordinated(job).await;
        }
        if job.is_prepared() {
            self.launch_provider(job).await
        } else if job.is_cancel_requested() {
            self.cancel_coordinated(job).await
        } else {
            self.refresh(job).await
        }
    }
}

fn with_access(mut job: ComputeJob, access_url: Option<String>) -> ComputeJob {
    if !job.status.terminal() {
        job.access_url = access_url;
    }
    job
}

/// One malformed offer must not take the catalog down; losing every offer to
/// malformation is a provider fault worth reporting.
fn conforming_offers(offers: Vec<ComputeOffer>) -> Result<Vec<ComputeOffer>, ServiceError> {
    let mut ids = std::collections::HashSet::new();
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

fn quote_maximum(rate_per_hour: u64, duration_secs: u64) -> Option<u64> {
    rate_per_hour
        .checked_mul(duration_secs)
        .and_then(|value| value.checked_add(3_599))
        .and_then(|value| value.checked_div(3_600))
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

fn validate_job_id(value: &str) -> Result<(), ServiceError> {
    if Uuid::parse_str(value).is_err() {
        return Err(ServiceError::InvalidJobId);
    }
    Ok(())
}

fn now_ms() -> Result<u64, ServiceError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ServiceError::Clock)?
        .as_millis();
    u64::try_from(millis).map_err(|_| ServiceError::Clock)
}

/// Why a launch plan was refused. Every arm names the field the caller has to
/// change; collapsing them loses the only clue a first-time caller gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PlanRejection {
    #[error("launch plan names an app that is not in the catalog")]
    UnknownApp,
    #[error("launch plan does not match the released catalog")]
    CatalogMismatch,
    #[error("app is not released for launch")]
    AppUnavailable,
    #[error("duration_secs must be between {minimum_secs} and {maximum_secs}")]
    Duration {
        minimum_secs: u64,
        maximum_secs: u64,
    },
    #[error("the selected offer is offline")]
    OfferOffline,
    #[error("the selected offer has less GPU memory than the app requires")]
    GpuMemory,
    #[error("the selected offer is below the app's minimum trust class")]
    TrustClass,
    #[error("the offer rate cannot be priced for this duration")]
    OfferRate,
    #[error("maximum_usdc_micros must be {expected_usdc_micros} for this offer and duration")]
    Maximum { expected_usdc_micros: u64 },
}

impl PlanRejection {
    pub fn code(self) -> &'static str {
        match self {
            Self::UnknownApp => "unknown_app",
            Self::CatalogMismatch => "invalid_launch_plan",
            Self::AppUnavailable => "app_unavailable",
            Self::Duration { .. } => "invalid_duration",
            Self::OfferOffline => "offer_offline",
            Self::GpuMemory => "insufficient_gpu_memory",
            Self::TrustClass => "insufficient_trust",
            Self::OfferRate => "invalid_offer_rate",
            Self::Maximum { .. } => "invalid_maximum_usdc_micros",
        }
    }
}

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error(transparent)]
    InvalidPlan(#[from] PlanRejection),
    #[error("launch offer is no longer available")]
    StaleOffer,
    #[error("provider returned invalid offers")]
    InvalidProviderOffers,
    #[error("idempotency key is invalid")]
    InvalidIdempotencyKey,
    #[error("job id is invalid")]
    InvalidJobId,
    #[error("durable job state is invalid")]
    InvalidDurableState,
    #[error("system clock is invalid")]
    Clock,
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Store(#[from] StoreError),
}
