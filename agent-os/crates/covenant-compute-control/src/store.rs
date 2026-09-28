//! The control plane's own bookkeeping. The coordinator holds the money
//! and the lease; this holds only what the coordinator does not — which
//! beta owner launched a job, the plan it committed, and the running
//! total of an owner's live reservations, so a spend cap is enforced
//! before a launch rather than discovered after. It is in-memory: a
//! restart forgets the index but not the leases, which the coordinator
//! keeps, and a beta restart is operator-driven.

use std::collections::HashMap;
use std::sync::Mutex;

use thiserror::Error;
use uuid::Uuid;

use crate::provider::ProviderJob;
use crate::wire::{ComputeJob, JobStatus, LaunchPlan};

#[derive(Default)]
pub struct JobStore {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    jobs: HashMap<Uuid, Record>,
    by_key: HashMap<(String, String), Uuid>,
}

struct Record {
    owner: String,
    idempotency_key: String,
    plan: LaunchPlan,
    /// The last projection served for this job, always without an access
    /// credential: a listing never carries one, and the cache backs the
    /// listing.
    cached: ComputeJob,
}

/// The outcome of a [`JobStore::reserve`]. `Fresh` holds a new job the
/// caller must launch and then record; `Existing` is a duplicate that
/// raced the same idempotency key and is returned untouched, so the key
/// never opens a second lease.
#[derive(Debug)]
pub enum Reserved {
    Fresh(Uuid),
    Existing(Box<ComputeJob>),
}

impl JobStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the recorded job for a repeated (owner, key), or a conflict
    /// if the same key now names a different launch. A fresh key answers
    /// `None`. This is the fast path a sequential retry takes before any
    /// market check; [`JobStore::reserve`] repeats the lookup under its own
    /// lock to catch a duplicate that raced this read.
    pub fn replay(
        &self,
        owner: &str,
        key: &str,
        plan: &LaunchPlan,
    ) -> Result<Option<ComputeJob>, StoreError> {
        self.lock().recorded(owner, key, plan)
    }

    /// Reserves an owner's spend and records a funding job in one locked
    /// step, so two launches racing the same cap cannot both pass. A
    /// duplicate that raced [`JobStore::replay`] for the same key finds
    /// its job already recorded here and is returned as `Existing`, so the
    /// key opens exactly one lease however the requests interleave. Fails
    /// closed when a fresh launch would carry the owner past the cap, or
    /// when the cap already sits below what the owner has committed.
    pub fn reserve(
        &self,
        owner: &str,
        spend_cap: u64,
        key: &str,
        plan: &LaunchPlan,
    ) -> Result<Reserved, StoreError> {
        let mut inner = self.lock();
        if let Some(job) = inner.recorded(owner, key, plan)? {
            return Ok(Reserved::Existing(Box::new(job)));
        }
        let reserved = inner.reserved(owner);
        if reserved > spend_cap {
            return Err(StoreError::SpendCapBelowCommitments);
        }
        if reserved.saturating_add(plan.maximum_usdc_micros) > spend_cap {
            return Err(StoreError::SpendCapExceeded);
        }
        let job_id = Uuid::new_v4();
        inner.jobs.insert(
            job_id,
            Record {
                owner: owner.to_owned(),
                idempotency_key: key.to_owned(),
                plan: plan.clone(),
                cached: funding_job(job_id, plan),
            },
        );
        inner
            .by_key
            .insert((owner.to_owned(), key.to_owned()), job_id);
        Ok(Reserved::Fresh(job_id))
    }

    /// Folds a provider observation into the record and returns the job as
    /// the client sees it now: with an access credential while the session
    /// is live, and none once it is terminal.
    pub fn record(&self, job_id: Uuid, provider: ProviderJob) -> Result<ComputeJob, StoreError> {
        let mut inner = self.lock();
        let record = inner.jobs.get_mut(&job_id).ok_or(StoreError::NotFound)?;
        // Terminal is final. A launch's own read can land after a concurrent
        // cancel already settled the job; letting it overwrite would resurrect
        // a cancelled lease to `running` with an access credential and re-hold
        // the freed reservation until the next poll corrects it.
        if record.cached.status.terminal() {
            return Ok(record.cached.clone());
        }
        let full = merge(job_id, &record.plan, provider);
        record.cached = ComputeJob {
            access_url: None,
            ..full.clone()
        };
        Ok(full)
    }

    /// Marks a job the provider refused as failed, keeping it visible to
    /// its owner. A terminal job no longer counts toward the spend cap.
    pub fn fail(&self, job_id: Uuid, reason: &str) -> Result<ComputeJob, StoreError> {
        let mut inner = self.lock();
        let record = inner.jobs.get_mut(&job_id).ok_or(StoreError::NotFound)?;
        // Terminal is final, the same guard `record` applies: a job that
        // already settled — a completed session that charged the buyer — must
        // not be relabelled `failed`, which would show the owner a failed job
        // they were in fact billed for. Today's only caller fails a freshly
        // reserved job that cannot be terminal; the guard holds the invariant
        // if that ever changes.
        if record.cached.status.terminal() {
            return Ok(record.cached.clone());
        }
        record.cached.status = JobStatus::Failed;
        record.cached.access_url = None;
        record.cached.error = Some(reason.to_owned());
        Ok(record.cached.clone())
    }

    /// Drops a reservation whose launch never reached the provider, so an
    /// owner's cap is not held against a machine that was never allocated.
    pub fn release(&self, job_id: Uuid) {
        let mut inner = self.lock();
        if let Some(record) = inner.jobs.remove(&job_id) {
            inner.by_key.remove(&(record.owner, record.idempotency_key));
        }
    }

    /// Every job an owner launched, as last projected. A listing, so each
    /// carries no access credential.
    pub fn jobs(&self, owner: &str) -> Vec<ComputeJob> {
        self.lock()
            .jobs
            .values()
            .filter(|record| record.owner == owner)
            .map(|record| record.cached.clone())
            .collect()
    }

    /// The plan and last projection of one of an owner's jobs. A job that
    /// belongs to another owner reads as absent, so a lookup or a cancel
    /// cannot probe for another owner's ids.
    pub fn get(&self, owner: &str, job_id: Uuid) -> Result<(LaunchPlan, ComputeJob), StoreError> {
        let inner = self.lock();
        let record = inner
            .jobs
            .get(&job_id)
            .filter(|record| record.owner == owner)
            .ok_or(StoreError::NotFound)?;
        Ok((record.plan.clone(), record.cached.clone()))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("job store mutex poisoned")
    }
}

impl Inner {
    /// The job a recorded (owner, key) names, or `None` for a fresh key.
    /// A conflict if the key already names a launch with a different plan.
    fn recorded(
        &self,
        owner: &str,
        key: &str,
        plan: &LaunchPlan,
    ) -> Result<Option<ComputeJob>, StoreError> {
        let Some(job_id) = self.by_key.get(&(owner.to_owned(), key.to_owned())) else {
            return Ok(None);
        };
        let record = self.jobs.get(job_id).ok_or(StoreError::NotFound)?;
        if &record.plan != plan {
            return Err(StoreError::IdempotencyConflict);
        }
        Ok(Some(record.cached.clone()))
    }

    fn reserved(&self, owner: &str) -> u64 {
        self.jobs
            .values()
            .filter(|record| record.owner == owner && !record.cached.status.terminal())
            .map(|record| record.cached.maximum_usdc_micros)
            .fold(0, u64::saturating_add)
    }
}

fn funding_job(job_id: Uuid, plan: &LaunchPlan) -> ComputeJob {
    ComputeJob {
        id: job_id.to_string(),
        app_id: plan.app.id.clone(),
        offer_id: plan.offer.id.clone(),
        status: JobStatus::Funding,
        maximum_usdc_micros: plan.maximum_usdc_micros,
        access_url: None,
        error: None,
        receipt: None,
    }
}

fn merge(job_id: Uuid, plan: &LaunchPlan, provider: ProviderJob) -> ComputeJob {
    let access_url = if provider.status.terminal() {
        None
    } else {
        provider.access_url
    };
    ComputeJob {
        id: job_id.to_string(),
        app_id: plan.app.id.clone(),
        offer_id: plan.offer.id.clone(),
        status: provider.status,
        maximum_usdc_micros: plan.maximum_usdc_micros,
        access_url,
        error: provider.error,
        receipt: provider.receipt,
    }
}

/// Why a store operation could not be completed. [`crate::web`] maps each
/// to a status and a stable code.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum StoreError {
    #[error("job was not found")]
    NotFound,
    #[error("the idempotency key identifies a different launch")]
    IdempotencyConflict,
    #[error("open sessions would exceed the beta spend cap")]
    SpendCapExceeded,
    #[error("the configured spend cap is below this owner's reservations")]
    SpendCapBelowCommitments,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::AppCatalog;
    use crate::wire::{ComputeOffer, ComputeReceipt, GpuSpec, TrustClass};

    fn launch_plan(maximum_usdc_micros: u64) -> LaunchPlan {
        LaunchPlan {
            app: AppCatalog::builtin().app("gpu-workspace").unwrap().clone(),
            offer: ComputeOffer {
                id: "rtx-4090".into(),
                gpu: GpuSpec {
                    model: "rtx-4090".into(),
                    vram_mib: 24_576,
                    cuda_major: 12,
                },
                rate_usdc_micros_per_hour: 1_000_000,
                trust_class: TrustClass::Open,
                online: true,
            },
            duration_secs: 1_800,
            maximum_usdc_micros,
        }
    }

    #[test]
    fn a_duplicate_key_racing_reserve_opens_one_lease_and_reserves_once() {
        let store = JobStore::new();
        let plan = launch_plan(500_000);
        // Both requests passed replay before either reserved — the exact
        // interleaving submit's replay-then-reserve gap produces.
        let Reserved::Fresh(job_id) = store.reserve("beta-a", 1_000_000, "dup", &plan).unwrap()
        else {
            panic!("the first reserve opens a fresh job");
        };
        let Reserved::Existing(job) = store.reserve("beta-a", 1_000_000, "dup", &plan).unwrap()
        else {
            panic!("a duplicate key must not open a second job");
        };
        assert_eq!(job.id, job_id.to_string());
        // One reservation, not two: the second launch's 500_000 never
        // counted against the cap.
        assert_eq!(store.lock().reserved("beta-a"), 500_000);
    }

    #[test]
    fn a_duplicate_key_racing_reserve_with_a_different_plan_conflicts() {
        let store = JobStore::new();
        store
            .reserve("beta-a", 1_000_000, "k", &launch_plan(500_000))
            .unwrap();
        let mut other = launch_plan(500_000);
        other.duration_secs = 3_600;
        assert_eq!(
            store.reserve("beta-a", 1_000_000, "k", &other).unwrap_err(),
            StoreError::IdempotencyConflict
        );
    }

    #[test]
    fn a_cap_lowered_below_commitments_refuses_new_work_but_still_replays_the_committed() {
        let store = JobStore::new();
        let plan = launch_plan(500_000);
        let Reserved::Fresh(job_id) = store.reserve("beta-a", 1_000_000, "live", &plan).unwrap()
        else {
            panic!("the first reserve opens a fresh job under the original cap");
        };

        // The operator tightens the owner's cap below what they have already
        // committed. A fresh launch fails closed — admitting it would carry
        // the owner past the new cap — and reserves nothing.
        assert_eq!(
            store.reserve("beta-a", 400_000, "next", &plan).unwrap_err(),
            StoreError::SpendCapBelowCommitments
        );
        assert_eq!(store.lock().reserved("beta-a"), 500_000);

        // A retry of the already-committed launch still replays, though: its
        // money moved once already, so the tightened cap must not strand the
        // session the owner is running.
        let Reserved::Existing(job) = store.reserve("beta-a", 400_000, "live", &plan).unwrap()
        else {
            panic!("the committed key replays regardless of the lowered cap");
        };
        assert_eq!(job.id, job_id.to_string());
    }

    #[test]
    fn a_terminal_job_is_not_resurrected_by_a_late_record() {
        let store = JobStore::new();
        let plan = launch_plan(500_000);
        let Reserved::Fresh(job_id) = store.reserve("beta-a", 1_000_000, "k", &plan).unwrap()
        else {
            panic!("the first reserve opens a fresh job");
        };
        // A cancel settles the job terminal and frees the reservation.
        let cancelled = store
            .record(
                job_id,
                ProviderJob {
                    status: JobStatus::Cancelled,
                    access_url: None,
                    error: None,
                    receipt: None,
                },
            )
            .unwrap();
        assert!(cancelled.status.terminal());
        assert_eq!(store.lock().reserved("beta-a"), 0);
        // The launch's own read lands late with a live view. Terminal is
        // final: it must not overwrite the settled state, resurface an
        // access credential, or re-hold the freed reservation.
        let after = store
            .record(
                job_id,
                ProviderJob {
                    status: JobStatus::Running,
                    access_url: Some("ssh renter@203.0.113.7 -p 2222".into()),
                    error: None,
                    receipt: None,
                },
            )
            .unwrap();
        assert_eq!(after.status, JobStatus::Cancelled);
        assert_eq!(after.access_url, None);
        assert_eq!(store.lock().reserved("beta-a"), 0);
    }

    #[test]
    fn fail_does_not_relabel_a_settled_job_that_charged() {
        let store = JobStore::new();
        let plan = launch_plan(500_000);
        let Reserved::Fresh(job_id) = store.reserve("beta-a", 1_000_000, "k", &plan).unwrap()
        else {
            panic!("the first reserve opens a fresh job");
        };
        // The session ran and settled: a completed job that charged the buyer.
        let completed = store
            .record(
                job_id,
                ProviderJob {
                    status: JobStatus::Completed,
                    access_url: None,
                    error: None,
                    receipt: Some(ComputeReceipt {
                        id: "r1".into(),
                        job_id: job_id.to_string(),
                        app_id: plan.app.id.clone(),
                        provider: "engine".into(),
                        runtime_secs: 60,
                        provisioning_secs: 0,
                        provisioning_usdc_micros: 0,
                        charged_usdc_micros: 60_000,
                        refunded_usdc_micros: 440_000,
                        commitment: "c1".into(),
                        transaction: None,
                    }),
                },
            )
            .unwrap();
        assert_eq!(completed.status, JobStatus::Completed);
        // A stray `fail` after settlement must not relabel it: the owner would
        // otherwise see a failed job they were charged 60_000 micro-USDC for.
        let after = store.fail(job_id, "provider_rejected").unwrap();
        assert_eq!(after.status, JobStatus::Completed);
        assert_eq!(after.error, None);
        assert_eq!(
            after.receipt.as_ref().map(|r| r.charged_usdc_micros),
            Some(60_000)
        );
    }
}
