use async_trait::async_trait;
use covenant_compute::{ComputeOffer, ComputeReceipt, GpuSpec, JobStatus, LaunchPlan, TrustClass};
use covenant_compute_vast::{
    workspace_label, Offer, OfferQuote, VastClient, VastError, WorkspaceFacts, WorkspaceLaunch,
    WorkspaceLaunchRequest,
};
use thiserror::Error;

use crate::{
    ProviderBackend, ProviderCancel, ProviderError, ProviderJob, ProviderLaunch, ProviderPoll,
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
        let client = VastClient::from_environment()
            .map_err(|_| VastBackendConfigError::Invalid)?
            .ok_or(VastBackendConfigError::Missing)?;
        Ok(Self::new(client))
    }

    async fn workspace_facts(
        &self,
        launch: &WorkspaceLaunch,
    ) -> Result<WorkspaceFacts, ProviderError> {
        match self.client.workspace(launch).await {
            Ok(facts) => Ok(facts),
            Err(error) if invalid_workspace_state(&error) => {
                let provider_error = map_vast_error(error);
                self.client
                    .destroy(launch.instance_id)
                    .await
                    .map_err(map_vast_error)?;
                Err(provider_error)
            }
            Err(error) => Err(map_vast_error(error)),
        }
    }

    async fn workspace(
        &self,
        job_id: &str,
        provider_job_id: u64,
        plan: &LaunchPlan,
        started_at_ms: u64,
        requested_at_ms: u64,
    ) -> Result<ProviderJob, ProviderError> {
        let launch = workspace_launch(job_id, provider_job_id, plan)?;
        let facts = self.workspace_facts(&launch).await?;
        if provider_failed(&facts) {
            self.client
                .destroy(facts.instance_id)
                .await
                .map_err(map_vast_error)?;
        }
        provider_job(facts, job_id, plan, started_at_ms, requested_at_ms)
    }

    async fn recover_one(
        &self,
        job_id: &str,
        plan: &LaunchPlan,
        started_at_ms: u64,
        requested_at_ms: u64,
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
        self.workspace(job_id, primary, plan, started_at_ms, requested_at_ms)
            .await
            .map(Some)
    }
}

#[async_trait]
impl ProviderBackend for VastBackend {
    async fn offers(&self) -> Result<Vec<ComputeOffer>, ProviderError> {
        self.client
            .ranked_offers(64, &[], self.client.config().max_hourly_micros)
            .await
            .map_err(map_vast_error)?
            .into_iter()
            .map(compute_offer)
            .collect()
    }

    async fn launch(&self, request: ProviderLaunch) -> Result<ProviderJob, ProviderError> {
        if let Some(job) = self
            .recover_one(
                &request.job_id,
                &request.plan,
                request.started_at_ms,
                request.requested_at_ms,
            )
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
        provider_job(
            facts,
            &request.job_id,
            &request.plan,
            request.started_at_ms,
            request.requested_at_ms,
        )
    }

    async fn job(&self, request: ProviderPoll) -> Result<ProviderJob, ProviderError> {
        let instance_id = provider_instance_id(&request.provider_job_id)?;
        self.workspace(
            &request.job_id,
            instance_id,
            &request.plan,
            request.started_at_ms,
            request.requested_at_ms,
        )
        .await
    }

    async fn cancel(&self, request: ProviderCancel) -> Result<ProviderJob, ProviderError> {
        let known_instance = request
            .provider_job_id
            .as_deref()
            .map(provider_instance_id)
            .transpose()?;
        if let Some(instance) = known_instance {
            self.client
                .destroy(instance)
                .await
                .map_err(map_vast_error)?;
        }
        let mut instances = self
            .client
            .recover(&request.job_id)
            .await
            .map_err(map_vast_error)?;
        instances.extend(known_instance);
        instances.sort_unstable();
        instances.dedup();
        for instance in &instances {
            self.client
                .destroy(*instance)
                .await
                .map_err(map_vast_error)?;
        }

        let runtime_secs = if instances.is_empty() {
            0
        } else {
            request
                .requested_at_ms
                .saturating_sub(request.started_at_ms)
                .div_ceil(1_000)
                .min(request.plan.duration_secs)
        };
        let charge = quote_maximum(request.plan.offer.rate_usdc_micros_per_hour, runtime_secs)?
            .min(request.plan.maximum_usdc_micros);
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
            receipt: Some(ComputeReceipt {
                id: format!("vast-{}", request.job_id),
                job_id: request.job_id,
                app_id: request.plan.app.id,
                provider: "vast".into(),
                runtime_secs,
                charged_usdc_micros: charge,
                refunded_usdc_micros: request.plan.maximum_usdc_micros - charge,
                commitment,
                transaction: None,
            }),
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
    started_at_ms: u64,
    requested_at_ms: u64,
) -> Result<ProviderJob, ProviderError> {
    let terminal_failure = provider_failed(&facts);
    let receipt = terminal_failure
        .then(|| {
            usage_receipt(
                job_id,
                plan,
                started_at_ms,
                requested_at_ms,
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
    started_at_ms: u64,
    requested_at_ms: u64,
    commitment: String,
) -> Result<ComputeReceipt, ProviderError> {
    let runtime_secs = requested_at_ms
        .saturating_sub(started_at_ms)
        .div_ceil(1_000)
        .min(plan.duration_secs);
    let charge = quote_maximum(plan.offer.rate_usdc_micros_per_hour, runtime_secs)?
        .min(plan.maximum_usdc_micros);
    Ok(ComputeReceipt {
        id: format!("vast-{job_id}"),
        job_id: job_id.to_owned(),
        app_id: plan.app.id.clone(),
        provider: "vast".into(),
        runtime_secs,
        charged_usdc_micros: charge,
        refunded_usdc_micros: plan.maximum_usdc_micros - charge,
        commitment,
        transaction: None,
    })
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
        VastError::NoCapacity | VastError::OfferChanged | VastError::InvalidRequest(_) => {
            ProviderError::Rejected
        }
        VastError::Transport { .. } => ProviderError::Unavailable,
        VastError::UnexpectedStatus { status, .. }
            if status.is_server_error() || status.as_u16() == 429 =>
        {
            ProviderError::Unavailable
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
    #[error("Vast provider credentials are not configured")]
    Missing,
    #[error("Vast provider configuration is invalid")]
    Invalid,
}

#[cfg(test)]
mod tests {
    use covenant_compute_vast::CudaVersion;

    use super::*;

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
