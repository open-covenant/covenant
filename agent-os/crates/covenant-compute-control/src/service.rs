use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use covenant_compute::{
    AppAvailability, AppCatalog, ComputeJob, ComputeOffer, LaunchPlan, TrustClass,
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
    pub recovered: usize,
    pub deferred: usize,
}

#[derive(Clone)]
pub struct ControlPlane {
    catalog: AppCatalog,
    store: SqliteStore,
    provider: Arc<dyn ProviderBackend>,
    launch_guards: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
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

    pub async fn offers(&self) -> Result<Vec<ComputeOffer>, ServiceError> {
        let offers = self.provider.offers().await?;
        validate_offer_set(&offers)?;
        Ok(offers)
    }

    pub async fn submit(
        &self,
        principal: &Principal,
        idempotency_key: &str,
        plan: LaunchPlan,
    ) -> Result<ComputeJob, ServiceError> {
        validate_idempotency_key(idempotency_key)?;
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

        if submitted.job.is_terminal() {
            return Ok(submitted.job.wire());
        }
        if submitted.job.is_cancel_requested() {
            return self.cancel_coordinated(submitted.job).await;
        }
        if submitted.job.is_prepared() {
            return self.launch_provider(submitted.job).await;
        }
        if submitted.disposition == SubmitDisposition::Replay {
            return self.refresh(submitted.job).await;
        }
        Ok(submitted.job.wire())
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
                Ok(_) => report.recovered += 1,
                Err(ServiceError::Provider(_)) => report.deferred += 1,
                Err(error) if first_fatal.is_none() => first_fatal = Some(error),
                Err(_) => {}
            }
        }
        first_fatal.map_or(Ok(report), Err)
    }

    async fn validate_plan(&self, plan: &LaunchPlan) -> Result<(), ServiceError> {
        let catalog_app = self
            .catalog
            .app(&plan.app.id)
            .map_err(|_| ServiceError::InvalidPlan)?;
        if catalog_app != &plan.app
            || plan.app.availability != AppAvailability::Available
            || plan.app.image.is_none()
            || plan.duration_secs == 0
            || plan.duration_secs > plan.app.max_duration_secs
            || !plan.offer.online
            || plan.offer.gpu.vram_mib < plan.app.min_vram_mib
            || plan.offer.trust_class < plan.app.min_trust.max(TrustClass::Open)
            || plan.maximum_usdc_micros
                != quote_maximum(plan.offer.rate_usdc_micros_per_hour, plan.duration_secs)?
        {
            return Err(ServiceError::InvalidPlan);
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
            let job = self
                .store
                .request_cancel(&job.owner, &job.id, requested_at_ms)
                .await?;
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
            started_at_ms: job.ready_at_ms.unwrap_or(requested_at_ms),
            requested_at_ms,
        };
        match self.provider.launch(request).await {
            Ok(provider_job) => {
                let provider_job_id = provider_job.id.clone();
                let provider_status = provider_job.status;
                let access_url = provider_job.access_url.clone();
                let recorded = self
                    .store
                    .record_provider(&job.id, provider_job, now_ms()?)
                    .await?;
                if recorded.is_terminal() {
                    if !provider_status.terminal() {
                        self.cleanup_late_launch(&recorded, provider_job_id).await?;
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
        if job.deadline_reached(now_ms()?) {
            let job = self
                .store
                .request_cancel(&job.owner, &job.id, now_ms()?)
                .await?;
            return self.cancel_coordinated(job).await;
        }
        if job.is_prepared() {
            return Ok(job.wire());
        }
        let Some(provider_job_id) = &job.provider_job_id else {
            return Err(ServiceError::InvalidDurableState);
        };
        let requested_at_ms = now_ms()?;
        match self
            .provider
            .job(ProviderPoll {
                job_id: job.id.clone(),
                provider_job_id: provider_job_id.clone(),
                plan: job.plan.clone(),
                started_at_ms: job.ready_at_ms.unwrap_or(requested_at_ms),
                requested_at_ms,
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

    async fn cancel_provider(&self, job: StoredJob) -> Result<ComputeJob, ServiceError> {
        let requested_at_ms = now_ms()?;
        match self
            .provider
            .cancel(ProviderCancel {
                job_id: job.id.clone(),
                provider_job_id: job.provider_job_id.clone(),
                plan: job.plan.clone(),
                started_at_ms: job.ready_at_ms.unwrap_or(requested_at_ms),
                requested_at_ms,
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

    async fn cancel_coordinated(&self, job: StoredJob) -> Result<ComputeJob, ServiceError> {
        let launch_guard = self.launch_guard(&job.id).await;
        let _guard = launch_guard.lock().await;
        let job = self.store.job(&job.owner, &job.id).await?;
        if job.is_terminal() {
            return Ok(job.wire());
        }
        self.cancel_provider(job).await
    }

    async fn cleanup_late_launch(
        &self,
        job: &StoredJob,
        provider_job_id: String,
    ) -> Result<(), ServiceError> {
        let cleanup = self
            .provider
            .cancel(ProviderCancel {
                job_id: job.id.clone(),
                provider_job_id: Some(provider_job_id),
                plan: job.plan.clone(),
                started_at_ms: job.ready_at_ms.unwrap_or(u64::MAX),
                requested_at_ms: now_ms()?,
            })
            .await?;
        if !cleanup.status.terminal() {
            return Err(ServiceError::Provider(ProviderError::InvalidState));
        }
        Ok(())
    }

    async fn launch_guard(&self, job_id: &str) -> Arc<Mutex<()>> {
        let mut guards = self.launch_guards.lock().await;
        Arc::clone(
            guards
                .entry(job_id.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }

    async fn reconcile_job(&self, job: StoredJob) -> Result<ComputeJob, ServiceError> {
        if job.deadline_reached(now_ms()?) && !job.is_cancel_requested() {
            let job = self
                .store
                .request_cancel(&job.owner, &job.id, now_ms()?)
                .await?;
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

fn validate_offer_set(offers: &[ComputeOffer]) -> Result<(), ServiceError> {
    let mut ids = std::collections::HashSet::new();
    for offer in offers {
        if offer.id.is_empty()
            || offer.id.len() > 200
            || offer.gpu.model.trim().is_empty()
            || offer.gpu.vram_mib == 0
            || offer.rate_usdc_micros_per_hour == 0
            || !ids.insert(&offer.id)
        {
            return Err(ServiceError::InvalidProviderOffers);
        }
    }
    Ok(())
}

fn quote_maximum(rate_per_hour: u64, duration_secs: u64) -> Result<u64, ServiceError> {
    rate_per_hour
        .checked_mul(duration_secs)
        .and_then(|value| value.checked_add(3_599))
        .and_then(|value| value.checked_div(3_600))
        .ok_or(ServiceError::InvalidPlan)
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

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("launch plan does not match the immutable catalog")]
    InvalidPlan,
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
