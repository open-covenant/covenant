//! The supply the control plane sells. [`ProviderBackend`] is the seam
//! between the customer API and wherever the GPUs come from: the
//! engine-backed implementation opens leases against the coordinator and
//! reads their meters back, but the service layer sees only this trait,
//! so its logic — the plan checks, the spend caps, the idempotency —
//! is exercised against an in-memory provider with no coordinator in the
//! loop.

use async_trait::async_trait;
use thiserror::Error;
use uuid::Uuid;

use crate::wire::{ComputeOffer, ComputeReceipt, JobStatus, LaunchPlan};

/// Why a provider call could not be completed. The service layer folds
/// most arms into a single `provider_unavailable` the client can retry;
/// a `Rejected` launch becomes a failed job instead. The distinctions are
/// for logs and recovery, not the wire.
#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("the compute provider is temporarily unavailable")]
    Unavailable,
    #[error("the compute provider refused the launch")]
    Rejected,
}

/// The provider's account of one job: where it is, how to reach it while
/// it runs, and what it settled to. The service layer joins this with the
/// plan it holds to build the customer job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderJob {
    pub status: JobStatus,
    pub access_url: Option<String>,
    pub error: Option<String>,
    pub receipt: Option<ComputeReceipt>,
}

#[async_trait]
pub trait ProviderBackend: Send + Sync + 'static {
    /// The supply bookable right now, one entry per offer a launch can
    /// name. The service layer screens the list before it is served.
    async fn offers(&self) -> Result<Vec<ComputeOffer>, ProviderError>;

    /// Opens a session for a checked plan. `job_id` is the control-plane
    /// id the coordinator lease is opened under; implementations must treat
    /// it as a stable idempotency key so a retry cannot allocate a second
    /// machine.
    async fn launch(&self, job_id: Uuid, plan: &LaunchPlan) -> Result<ProviderJob, ProviderError>;

    /// The provider's current account of a job. `Unavailable` leaves the
    /// caller on the last state it recorded rather than erroring.
    async fn poll(&self, job_id: Uuid, plan: &LaunchPlan) -> Result<ProviderJob, ProviderError>;

    /// Ends a session. Implementations must make repeated calls safe:
    /// closing a job that already settled returns its settled account.
    async fn cancel(&self, job_id: Uuid, plan: &LaunchPlan) -> Result<ProviderJob, ProviderError>;
}

#[cfg(test)]
pub(crate) mod testing {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    /// The endpoint the stub hands back for a running session.
    pub(crate) const STUB_ENDPOINT: &str = "ssh renter@stub.test -p 2222";

    /// A provider with no coordinator behind it: it answers `offers` from a
    /// fixed list and `launch` with an immediately-running session, or is
    /// configured to report itself unavailable or to refuse launches, so
    /// the service layer's failure handling runs without a live engine.
    pub(crate) struct MemoryProvider {
        offers: Vec<ComputeOffer>,
        available: bool,
        reject: bool,
        launched: Mutex<HashMap<Uuid, ProviderJob>>,
    }

    impl MemoryProvider {
        pub(crate) fn new(offers: Vec<ComputeOffer>) -> Self {
            Self {
                offers,
                available: true,
                reject: false,
                launched: Mutex::new(HashMap::new()),
            }
        }

        pub(crate) fn unavailable() -> Self {
            Self {
                offers: Vec::new(),
                available: false,
                reject: false,
                launched: Mutex::new(HashMap::new()),
            }
        }

        pub(crate) fn rejecting(offers: Vec<ComputeOffer>) -> Self {
            Self {
                offers,
                available: true,
                reject: true,
                launched: Mutex::new(HashMap::new()),
            }
        }

        fn sessions(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, ProviderJob>> {
            self.launched.lock().unwrap()
        }
    }

    #[async_trait]
    impl ProviderBackend for MemoryProvider {
        async fn offers(&self) -> Result<Vec<ComputeOffer>, ProviderError> {
            if !self.available {
                return Err(ProviderError::Unavailable);
            }
            Ok(self.offers.clone())
        }

        async fn launch(
            &self,
            job_id: Uuid,
            _plan: &LaunchPlan,
        ) -> Result<ProviderJob, ProviderError> {
            if !self.available {
                return Err(ProviderError::Unavailable);
            }
            if self.reject {
                return Err(ProviderError::Rejected);
            }
            let job = ProviderJob {
                status: JobStatus::Running,
                access_url: Some(STUB_ENDPOINT.to_owned()),
                error: None,
                receipt: None,
            };
            self.sessions().insert(job_id, job.clone());
            Ok(job)
        }

        async fn poll(
            &self,
            job_id: Uuid,
            _plan: &LaunchPlan,
        ) -> Result<ProviderJob, ProviderError> {
            if !self.available {
                return Err(ProviderError::Unavailable);
            }
            self.sessions()
                .get(&job_id)
                .cloned()
                .ok_or(ProviderError::Unavailable)
        }

        async fn cancel(
            &self,
            job_id: Uuid,
            plan: &LaunchPlan,
        ) -> Result<ProviderJob, ProviderError> {
            if !self.available {
                return Err(ProviderError::Unavailable);
            }
            let settled = ProviderJob {
                status: JobStatus::Completed,
                access_url: None,
                error: None,
                receipt: Some(stub_receipt(job_id, plan)),
            };
            self.sessions().insert(job_id, settled.clone());
            Ok(settled)
        }
    }

    fn stub_receipt(job_id: Uuid, plan: &LaunchPlan) -> ComputeReceipt {
        let charged = 100_000;
        ComputeReceipt {
            id: format!("stub-{job_id}"),
            job_id: job_id.to_string(),
            app_id: plan.app.id.clone(),
            provider: "stub".to_owned(),
            runtime_secs: 60,
            provisioning_secs: 0,
            provisioning_usdc_micros: 0,
            charged_usdc_micros: charged,
            refunded_usdc_micros: plan.maximum_usdc_micros.saturating_sub(charged),
            commitment: "stub-commitment".to_owned(),
            transaction: None,
        }
    }
}
