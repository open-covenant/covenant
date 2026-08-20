use async_trait::async_trait;
use covenant_compute::{ComputeOffer, ComputeReceipt, JobStatus, LaunchPlan};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct ProviderLaunch {
    pub job_id: String,
    pub idempotency_key: String,
    pub plan: LaunchPlan,
    pub started_at_ms: u64,
    pub requested_at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct ProviderCancel {
    pub job_id: String,
    pub provider_job_id: Option<String>,
    pub plan: LaunchPlan,
    pub started_at_ms: u64,
    pub requested_at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct ProviderPoll {
    pub job_id: String,
    pub provider_job_id: String,
    pub plan: LaunchPlan,
    pub started_at_ms: u64,
    pub requested_at_ms: u64,
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
