use async_trait::async_trait;
use covenant_compute::{ComputeOffer, ComputeReceipt, GpuSpec, JobStatus, LaunchPlan, TrustClass};
use covenant_compute_vast::{
    workspace_label, Offer, OfferQuote, OfferSurvey, VastClient, VastConfig, VastError,
    WorkspaceFacts, WorkspaceLaunch, WorkspaceLaunchRequest,
};
use thiserror::Error;

use crate::{
    JobClock, ProviderBackend, ProviderCancel, ProviderError, ProviderJob, ProviderLaunch,
    ProviderPoll,
};

#[derive(Clone)]
pub struct VastBackend {
    client: VastClient,
}

impl VastBackend {
    pub fn new(client: VastClient) -> Self {
        Self { client }
    }

    pub fn from_environment() -> Result<Self, VastBackendConfigError> {
        let client = VastClient::from_environment()?.ok_or(VastBackendConfigError::Missing)?;
        let config = client.config();
        tracing::info!(
            api_url = %config.api_url,
            gpu_models = %config.gpu_models.join(","),
            min_gpu_memory_mib = config.min_gpu_memory_mib,
            max_hourly_micros = config.max_hourly_micros,
            max_inet_cost_micros = config.max_inet_cost_micros,
            disk_gb = config.disk_gb,
            "Vast offer search constraints"
        );
        Ok(Self::new(client))
    }

    async fn workspace_facts(
        &self,
        launch: &WorkspaceLaunch,
    ) -> Result<WorkspaceFacts, ProviderError> {
        match self.client.workspace(launch).await {
            Ok(facts) => Ok(facts),
            Err(error) if invalid_workspace_state(&error) => {
                // Report why the workspace was rejected, not why the cleanup
                // that followed it went wrong.
                if let Err(cleanup) = self.client.destroy(launch.instance_id).await {
                    tracing::error!(
                        instance_id = launch.instance_id,
                        %cleanup,
                        "rejected workspace could not be destroyed"
                    );
                }
                Err(map_vast_error(error))
            }
            Err(error) => Err(map_vast_error(error)),
        }
    }

    async fn workspace(
        &self,
        job_id: &str,
        provider_job_id: u64,
        plan: &LaunchPlan,
        clock: JobClock,
    ) -> Result<ProviderJob, ProviderError> {
        let launch = workspace_launch(job_id, provider_job_id, plan)?;
        let facts = self.workspace_facts(&launch).await?;
        if provider_failed(&facts) {
            self.client
                .destroy(facts.instance_id)
                .await
                .map_err(map_vast_error)?;
        }
        provider_job(facts, job_id, plan, clock)
    }

    async fn recover_one(
        &self,
        job_id: &str,
        plan: &LaunchPlan,
        clock: JobClock,
    ) -> Result<Option<ProviderJob>, ProviderError> {
        let mut instances = self.client.recover(job_id).await.map_err(map_vast_error)?;
        let Some(primary) = instances.first().copied() else {
            return Ok(None);
        };
        for duplicate in instances.drain(1..) {
            self.client
                .destroy(duplicate)
                .await
                .map_err(map_vast_error)?;
        }
        self.workspace(job_id, primary, plan, clock).await.map(Some)
    }
}

#[async_trait]
impl ProviderBackend for VastBackend {
    async fn offers(&self) -> Result<Vec<ComputeOffer>, ProviderError> {
        let config = self.client.config();
        let ranked = self
            .client
            .ranked_offers(64, &[], config.max_hourly_micros)
            .await
            .map_err(map_vast_error)?;
        report_survey(&ranked.survey, config);
        ranked.offers.into_iter().map(compute_offer).collect()
    }

    async fn launch(&self, request: ProviderLaunch) -> Result<ProviderJob, ProviderError> {
        if let Some(job) = self
            .recover_one(&request.job_id, &request.plan, request.clock)
            .await?
        {
            return Ok(job);
        }
        let image = request
            .plan
            .app
            .image
            .clone()
            .ok_or(ProviderError::Rejected)?;
        let required_offer = vast_offer(&request.plan.offer)?;
        let launch = self
            .client
            .launch_workspace(WorkspaceLaunchRequest {
                workload_id: request.job_id.clone(),
                image,
                max_hourly_micros: request.plan.offer.rate_usdc_micros_per_hour,
                rejected_machine_ids: Vec::new(),
                required_offer,
            })
            .await
            .map_err(map_vast_error)?;
        let facts = self.workspace_facts(&launch).await?;
        provider_job(facts, &request.job_id, &request.plan, request.clock)
    }

    async fn job(&self, request: ProviderPoll) -> Result<ProviderJob, ProviderError> {
        let instance_id = provider_instance_id(&request.provider_job_id)?;
        self.workspace(&request.job_id, instance_id, &request.plan, request.clock)
            .await
    }

    async fn cancel(&self, request: ProviderCancel) -> Result<ProviderJob, ProviderError> {
        let known_instance = request
            .provider_job_id
            .as_deref()
            .map(provider_instance_id)
            .transpose()?;
        let mut instances = Vec::new();
        if let Some(instance) = known_instance {
            self.client
                .destroy(instance)
                .await
                .map_err(map_vast_error)?;
            instances.push(instance);
        }
        for instance in self
            .client
            .recover(&request.job_id)
            .await
            .map_err(map_vast_error)?
        {
            if Some(instance) != known_instance {
                self.client
                    .destroy(instance)
                    .await
                    .map_err(map_vast_error)?;
                instances.push(instance);
            }
        }
        instances.sort_unstable();

        // Nothing was ever allocated, so there is nothing to bill.
        let runtime_secs = if instances.is_empty() {
            0
        } else {
            runtime_secs(&request.plan, request.clock)
        };
        let provider_id = instances
            .first()
            .map(u64::to_string)
            .unwrap_or_else(|| format!("absent-{}", request.job_id));
        let commitment = instances.first().map_or_else(
            || format!("vast:workload:{}:absent", request.job_id),
            |instance| format!("vast:instance:{instance}:destroyed"),
        );
        Ok(ProviderJob {
            id: provider_id,
            status: JobStatus::Cancelled,
            access_url: None,
            error: None,
            receipt: Some(usage_receipt(
                &request.job_id,
                &request.plan,
                request.clock,
                runtime_secs,
                commitment,
            )?),
        })
    }
}

fn compute_offer(offer: Offer) -> Result<ComputeOffer, ProviderError> {
    Ok(ComputeOffer {
        id: format!("vast:{}:{}", offer.id, offer.machine_id),
        gpu: GpuSpec {
            model: offer.gpu_model,
            vram_mib: offer.gpu_memory_mib,
            cuda_major: offer.cuda_max_good.major,
        },
        rate_usdc_micros_per_hour: offer.hourly_micros,
        trust_class: TrustClass::Open,
        online: true,
    })
}

fn vast_offer(offer: &ComputeOffer) -> Result<OfferQuote, ProviderError> {
    let (id, machine_id) = parse_offer_id(&offer.id)?;
    Ok(OfferQuote {
        id,
        machine_id,
        gpu_model: offer.gpu.model.clone(),
        gpu_memory_mib: offer.gpu.vram_mib,
        hourly_micros: offer.rate_usdc_micros_per_hour,
    })
}

fn workspace_launch(
    job_id: &str,
    instance_id: u64,
    plan: &LaunchPlan,
) -> Result<WorkspaceLaunch, ProviderError> {
    Ok(WorkspaceLaunch {
        instance_id,
        label: workspace_label(job_id).map_err(map_vast_error)?,
        offer: vast_offer(&plan.offer)?,
        image: plan.app.image.clone().ok_or(ProviderError::InvalidState)?,
    })
}

fn provider_job(
    facts: WorkspaceFacts,
    job_id: &str,
    plan: &LaunchPlan,
    clock: JobClock,
) -> Result<ProviderJob, ProviderError> {
    let terminal_failure = provider_failed(&facts);
    let receipt = terminal_failure
        .then(|| {
            usage_receipt(
                job_id,
                plan,
                clock,
                runtime_secs(plan, clock),
                format!("vast:instance:{}:provider_stopped", facts.instance_id),
            )
        })
        .transpose()?;
    let (status, error) = if terminal_failure {
        (JobStatus::Failed, Some("provider_stopped".into()))
    } else if facts.ready {
        (JobStatus::Running, None)
    } else {
        (JobStatus::Provisioning, None)
    };
    let access_url = facts
        .ready
        .then_some(facts.access)
        .flatten()
        .map(|access| access.expose_secret().as_str().to_owned());
    Ok(ProviderJob {
        id: facts.instance_id.to_string(),
        status,
        access_url,
        error,
        receipt,
    })
}

fn provider_failed(facts: &WorkspaceFacts) -> bool {
    matches!(
        facts.status.as_str(),
        "exited" | "stopped" | "deleted" | "offline"
    )
}

fn usage_receipt(
    job_id: &str,
    plan: &LaunchPlan,
    clock: JobClock,
    runtime_secs: u64,
    commitment: String,
) -> Result<ComputeReceipt, ProviderError> {
    let rate = plan.offer.rate_usdc_micros_per_hour;
    let charge = quote_maximum(rate, runtime_secs)?.min(plan.maximum_usdc_micros);
    let provisioning_secs = clock.provisioning_secs();
    Ok(ComputeReceipt {
        id: format!("vast-{job_id}"),
        job_id: job_id.to_owned(),
        app_id: plan.app.id.clone(),
        provider: "vast".into(),
        runtime_secs,
        provisioning_secs,
        provisioning_usdc_micros: quote_maximum(rate, provisioning_secs)?,
        charged_usdc_micros: charge,
        refunded_usdc_micros: plan.maximum_usdc_micros - charge,
        commitment,
        transaction: None,
    })
}

/// Billed time runs from the first running observation and never past the
/// selected duration.
fn runtime_secs(plan: &LaunchPlan, clock: JobClock) -> u64 {
    clock
        .requested_at_ms
        .saturating_sub(clock.billed_from_ms())
        .div_ceil(1_000)
        .min(plan.duration_secs)
}

/// An empty offer list is the first thing a new operator hits. Naming the
/// constraint that emptied it is the difference between a one-line env change
/// and reading the adapter source.
fn report_survey(survey: &OfferSurvey, config: &VastConfig) {
    if survey.admitted == 0 {
        tracing::warn!(
            provider_offers = survey.returned,
            dropped_bandwidth_cost = survey.bandwidth_cost,
            dropped_host_evidence = survey.host_evidence,
            dropped_gpu_class = survey.gpu_class,
            dropped_price_ceiling = survey.price_ceiling,
            gpu_models = %config.gpu_models.join(","),
            min_gpu_memory_mib = config.min_gpu_memory_mib,
            max_hourly_micros = config.max_hourly_micros,
            max_inet_cost_micros = config.max_inet_cost_micros,
            "no compute offer met the configured constraints"
        );
        return;
    }
    tracing::info!(
        provider_offers = survey.returned,
        dropped_bandwidth_cost = survey.bandwidth_cost,
        dropped_host_evidence = survey.host_evidence,
        dropped_gpu_class = survey.gpu_class,
        dropped_price_ceiling = survey.price_ceiling,
        admitted = survey.admitted,
        "compute offer search completed"
    );
}

fn parse_offer_id(value: &str) -> Result<(u64, u64), ProviderError> {
    let mut fields = value.split(':');
    let valid_prefix = fields.next() == Some("vast");
    let offer_id = fields.next().and_then(|value| value.parse().ok());
    let machine_id = fields.next().and_then(|value| value.parse().ok());
    if !valid_prefix || fields.next().is_some() {
        return Err(ProviderError::Rejected);
    }
    match (offer_id, machine_id) {
        (Some(offer_id), Some(machine_id)) if offer_id != 0 && machine_id != 0 => {
            Ok((offer_id, machine_id))
        }
        _ => Err(ProviderError::Rejected),
    }
}

fn provider_instance_id(value: &str) -> Result<u64, ProviderError> {
    value
        .parse()
        .ok()
        .filter(|value| *value != 0)
        .ok_or(ProviderError::InvalidState)
}

fn quote_maximum(rate_per_hour: u64, duration_secs: u64) -> Result<u64, ProviderError> {
    rate_per_hour
        .checked_mul(duration_secs)
        .and_then(|value| value.checked_add(3_599))
        .and_then(|value| value.checked_div(3_600))
        .ok_or(ProviderError::InvalidState)
}

fn map_vast_error(error: VastError) -> ProviderError {
    match error {
        VastError::NoCapacity
        | VastError::OfferChanged
        | VastError::InvalidRequest(_)
        | VastError::Refused { .. } => ProviderError::Rejected,
        VastError::Transport { .. } => ProviderError::Unavailable,
        VastError::UnexpectedStatus { status, .. }
            if status.is_server_error() || status.as_u16() == 429 =>
        {
            ProviderError::Unavailable
        }
        // Credential and configuration faults never clear on their own.
        VastError::UnexpectedStatus { status, .. } if matches!(status.as_u16(), 401 | 403) => {
            tracing::error!(status = status.as_u16(), "Vast rejected the API credential");
            ProviderError::Configuration
        }
        VastError::Configuration(_)
        | VastError::CredentialsMissing
        | VastError::CredentialRead { .. }
        | VastError::InvalidCredential
        | VastError::ClientBuild(_) => {
            tracing::error!(%error, "Vast provider configuration is unusable");
            ProviderError::Configuration
        }
        VastError::Decode { .. }
        | VastError::InvalidResponse { .. }
        | VastError::ResponseTooLarge { .. } => ProviderError::InvalidState,
        _ => ProviderError::Operation,
    }
}

fn invalid_workspace_state(error: &VastError) -> bool {
    matches!(
        error,
        VastError::Decode { .. }
            | VastError::InvalidResponse { .. }
            | VastError::ResponseTooLarge { .. }
    )
}

#[derive(Debug, Error)]
pub enum VastBackendConfigError {
    #[error(
        "Vast credentials are not configured: set COVENANT_VAST_API_KEY or \
         COVENANT_VAST_API_KEY_FILE"
    )]
    Missing,
    #[error(transparent)]
    Invalid(#[from] VastError),
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use covenant_compute::AppCatalog;
    use covenant_compute_vast::CudaVersion;

    use super::*;

    fn plan() -> LaunchPlan {
        LaunchPlan {
            app: AppCatalog::builtin().app("gpu-workspace").unwrap().clone(),
            offer: ComputeOffer {
                id: "vast:7:70".into(),
                gpu: GpuSpec {
                    model: "L40S".into(),
                    vram_mib: 46_068,
                    cuda_major: 12,
                },
                rate_usdc_micros_per_hour: 720_000,
                trust_class: TrustClass::Open,
                online: true,
            },
            duration_secs: 1_800,
            maximum_usdc_micros: 360_000,
        }
    }

    #[test]
    fn a_vanished_instance_settles_with_a_metered_receipt() {
        let plan = plan();
        let facts = WorkspaceFacts {
            instance_id: 99,
            status: "deleted".into(),
            ready: false,
            gpu_model: String::new(),
            gpu_memory_mib: 0,
            verification: String::new(),
            hourly_micros: 0,
            machine_id: 0,
            image: String::new(),
            runtime: String::new(),
            access: None,
        };

        let clock = JobClock {
            created_at_ms: 0,
            ready_at_ms: Some(30_000),
            requested_at_ms: 90_000,
        };
        let job = provider_job(facts, "job-1", &plan, clock).unwrap();
        assert_eq!(job.status, JobStatus::Failed);
        let receipt = job.receipt.unwrap();
        assert_eq!(receipt.runtime_secs, 60);
        assert_eq!(receipt.charged_usdc_micros, 12_000);
        assert_eq!(receipt.refunded_usdc_micros, 348_000);
        // The 30 s the provider billed before the workspace answered is
        // reported and priced at the same rate, not charged.
        assert_eq!(receipt.provisioning_secs, 30);
        assert_eq!(receipt.provisioning_usdc_micros, 6_000);
    }

    #[test]
    fn a_job_that_never_became_ready_reports_no_provisioning_window() {
        let clock = JobClock {
            created_at_ms: 1_000,
            ready_at_ms: None,
            requested_at_ms: 145_000,
        };
        let receipt =
            usage_receipt("job-2", &plan(), clock, 0, "vast:instance:1:test".into()).unwrap();
        assert_eq!(receipt.runtime_secs, 0);
        assert_eq!(receipt.provisioning_secs, 0);
        assert_eq!(receipt.provisioning_usdc_micros, 0);
        assert_eq!(receipt.charged_usdc_micros, 0);
    }

    #[test]
    fn a_provisioning_window_longer_than_the_session_is_priced_in_full() {
        let plan = plan();
        // The window a live L40S took to answer, against the shortest session
        // the control plane accepts.
        let clock = JobClock {
            created_at_ms: 0,
            ready_at_ms: Some(207_000),
            requested_at_ms: 207_000 + 300_000,
        };
        let receipt = usage_receipt(
            "job-3",
            &plan,
            clock,
            runtime_secs(&plan, clock),
            "vast:instance:1:destroyed".into(),
        )
        .unwrap();

        assert_eq!(receipt.runtime_secs, 300);
        assert_eq!(receipt.charged_usdc_micros, 60_000);
        assert_eq!(receipt.provisioning_secs, 207);
        assert_eq!(receipt.provisioning_usdc_micros, 41_400);
    }

    #[test]
    fn credential_faults_are_not_reported_as_a_transient_outage() {
        assert!(matches!(
            map_vast_error(VastError::UnexpectedStatus {
                operation: "offer search",
                status: StatusCode::UNAUTHORIZED,
            }),
            ProviderError::Configuration
        ));
        assert!(matches!(
            map_vast_error(VastError::CredentialsMissing),
            ProviderError::Configuration
        ));
        assert!(matches!(
            map_vast_error(VastError::Refused {
                operation: "workspace creation",
            }),
            ProviderError::Rejected
        ));
    }

    #[test]
    fn compute_offer_reports_returned_cuda_compatibility() {
        let offer = compute_offer(Offer {
            id: 7,
            machine_id: 70,
            gpu_model: "L40S".into(),
            gpu_memory_mib: 46_068,
            hourly_micros: 590_000,
            verification: "verified".into(),
            reliability: 0.999,
            rentable: true,
            rented: false,
            direct_port_count: 2,
            cuda_max_good: CudaVersion {
                major: 13,
                minor: 0,
            },
            gpu_count: 1,
            gpu_arch: "nvidia".into(),
            cpu_arch: "amd64".into(),
        })
        .unwrap();

        assert_eq!(offer.id, "vast:7:70");
        assert_eq!(offer.gpu.cuda_major, 13);
    }
}
