use async_trait::async_trait;
use covenant_compute::{ComputeOffer, ComputeReceipt, JobStatus, LaunchPlan};
use thiserror::Error;

/// Durable timestamps a provider needs to bill and to report the provisioning
/// window. Billing runs from `ready_at_ms`; the window before it is provider
/// cost the operator absorbs.
#[derive(Debug, Clone, Copy)]
pub struct JobClock {
    pub created_at_ms: u64,
    pub ready_at_ms: Option<u64>,
    pub requested_at_ms: u64,
}

impl JobClock {
    pub fn billed_from_ms(&self) -> u64 {
        self.ready_at_ms.unwrap_or(self.requested_at_ms)
    }

    pub fn provisioning_secs(&self) -> u64 {
        self.ready_at_ms.map_or(0, |ready| {
            ready.saturating_sub(self.created_at_ms).div_ceil(1_000)
        })
    }
}

#[derive(Debug, Clone)]
pub struct ProviderLaunch {
    pub job_id: String,
    pub idempotency_key: String,
    pub plan: LaunchPlan,
    pub clock: JobClock,
}

#[derive(Debug, Clone)]
pub struct ProviderCancel {
    pub job_id: String,
    pub provider_job_id: Option<String>,
    pub plan: LaunchPlan,
    pub clock: JobClock,
}

#[derive(Debug, Clone)]
pub struct ProviderPoll {
    pub job_id: String,
    pub provider_job_id: String,
    pub plan: LaunchPlan,
    pub clock: JobClock,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderJob {
    pub id: String,
    pub status: JobStatus,
    pub access_url: Option<String>,
    pub error: Option<String>,
    pub receipt: Option<ComputeReceipt>,
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("provider is temporarily unavailable")]
    Unavailable,
    #[error("provider rejected the workload")]
    Rejected,
    #[error("provider returned invalid state")]
    InvalidState,
    #[error("provider credentials or configuration are rejected")]
    Configuration,
    #[error("provider operation failed")]
    Operation,
}

#[async_trait]
pub trait ProviderBackend: Send + Sync + 'static {
    async fn offers(&self) -> Result<Vec<ComputeOffer>, ProviderError>;

    /// Launches one workload. Implementations must treat `job_id` as a stable
    /// provider idempotency key so recovery cannot allocate a second machine.
    async fn launch(&self, request: ProviderLaunch) -> Result<ProviderJob, ProviderError>;

    async fn job(&self, request: ProviderPoll) -> Result<ProviderJob, ProviderError>;

    /// Cancels the workload identified by the stable control-plane job id.
    /// Implementations must make repeated cancellation safe.
    async fn cancel(&self, request: ProviderCancel) -> Result<ProviderJob, ProviderError>;
}
