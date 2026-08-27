use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use covenant_compute::{
    AppCatalog, ComputeOffer, ComputeReceipt, GpuSpec, JobStatus, LaunchPlan, TrustClass,
};
use tempfile::TempDir;
use tokio::sync::{Mutex, Notify};
use tower::ServiceExt;

use crate::{
    router, AuthRegistry, BetaCredential, ControlPlane, Principal, ProviderBackend, ProviderCancel,
    ProviderError, ProviderJob, ProviderLaunch, ProviderPoll, ServiceError, SqliteStore,
    StoreError,
};

struct FakeProvider {
    offers: Vec<ComputeOffer>,
    available: AtomicBool,
    launch_available: AtomicBool,
    launch_provisioning: AtomicBool,
    block_next_launch: AtomicBool,
    block_next_poll: AtomicBool,
    allocations: AtomicUsize,
    active_allocations: AtomicUsize,
    cancellations: AtomicUsize,
    launch_entered: Notify,
    launch_release: Notify,
    poll_entered: Notify,
    poll_release: Notify,
    jobs: Mutex<HashMap<String, (LaunchPlan, ProviderJob)>>,
    control_ids: Mutex<HashMap<String, String>>,
    last_cancel_window: Mutex<Option<(u64, u64)>>,
}

impl FakeProvider {
    fn new(offers: Vec<ComputeOffer>) -> Self {
        Self {
            offers,
            available: AtomicBool::new(true),
            launch_available: AtomicBool::new(true),
            launch_provisioning: AtomicBool::new(false),
            block_next_launch: AtomicBool::new(false),
            block_next_poll: AtomicBool::new(false),
            allocations: AtomicUsize::new(0),
            active_allocations: AtomicUsize::new(0),
            cancellations: AtomicUsize::new(0),
            launch_entered: Notify::new(),
            launch_release: Notify::new(),
            poll_entered: Notify::new(),
            poll_release: Notify::new(),
            jobs: Mutex::new(HashMap::new()),
            control_ids: Mutex::new(HashMap::new()),
            last_cancel_window: Mutex::new(None),
        }
    }

    fn set_launch_available(&self, available: bool) {
        self.launch_available.store(available, Ordering::SeqCst);
    }

    fn set_launch_provisioning(&self, provisioning: bool) {
        self.launch_provisioning
            .store(provisioning, Ordering::SeqCst);
    }

    fn block_next_launch(&self) {
        self.block_next_launch.store(true, Ordering::SeqCst);
    }

    async fn wait_for_blocked_launch(&self) {
        self.launch_entered.notified().await;
    }

    fn release_blocked_launch(&self) {
        self.launch_release.notify_one();
    }

    fn block_next_poll(&self) {
        self.block_next_poll.store(true, Ordering::SeqCst);
    }

    async fn wait_for_blocked_poll(&self) {
        self.poll_entered.notified().await;
    }

    fn release_blocked_poll(&self) {
        self.poll_release.notify_one();
    }

    async fn mark_running(&self) {
        for (_, job) in self.jobs.lock().await.values_mut() {
            job.status = JobStatus::Running;
        }
    }
}

#[async_trait]
impl ProviderBackend for FakeProvider {
    async fn offers(&self) -> Result<Vec<ComputeOffer>, ProviderError> {
        if !self.available.load(Ordering::SeqCst) {
            return Err(ProviderError::Unavailable);
        }
        Ok(self.offers.clone())
    }

    async fn launch(&self, request: ProviderLaunch) -> Result<ProviderJob, ProviderError> {
        if !self.launch_available.load(Ordering::SeqCst) {
            return Err(ProviderError::Unavailable);
        }
        if self.block_next_launch.swap(false, Ordering::SeqCst) {
            self.launch_entered.notify_one();
            self.launch_release.notified().await;
        }
        if let Some(provider_id) = self.control_ids.lock().await.get(&request.job_id).cloned() {
            return self
                .jobs
                .lock()
                .await
                .get(&provider_id)
                .map(|(_, job)| job.clone())
                .ok_or(ProviderError::InvalidState);
        }

        let provider_id = format!("provider-{}", request.job_id);
        let job = ProviderJob {
            id: provider_id.clone(),
            status: if self.launch_provisioning.load(Ordering::SeqCst) {
                JobStatus::Provisioning
            } else {
                JobStatus::Running
            },
            access_url: Some("https://workload.invalid/session?token=ephemeral-secret".into()),
            error: None,
            receipt: None,
        };
        self.control_ids
            .lock()
            .await
            .insert(request.job_id, provider_id.clone());
        self.jobs
            .lock()
            .await
            .insert(provider_id, (request.plan, job.clone()));
        self.allocations.fetch_add(1, Ordering::SeqCst);
        self.active_allocations.fetch_add(1, Ordering::SeqCst);
        Ok(job)
    }

    async fn job(&self, request: ProviderPoll) -> Result<ProviderJob, ProviderError> {
        if !self.available.load(Ordering::SeqCst) {
            return Err(ProviderError::Unavailable);
        }
        if self.block_next_poll.swap(false, Ordering::SeqCst) {
            self.poll_entered.notify_one();
            self.poll_release.notified().await;
        }
        self.jobs
            .lock()
            .await
            .get(&request.provider_job_id)
            .map(|(_, job)| job.clone())
            .ok_or(ProviderError::InvalidState)
    }

    async fn cancel(&self, request: ProviderCancel) -> Result<ProviderJob, ProviderError> {
        if !self.available.load(Ordering::SeqCst) {
            return Err(ProviderError::Unavailable);
        }
        self.cancellations.fetch_add(1, Ordering::SeqCst);
        *self.last_cancel_window.lock().await =
            Some((request.started_at_ms, request.requested_at_ms));
        let provider_id = match request.provider_job_id {
            Some(id) => id,
            None => match self.control_ids.lock().await.get(&request.job_id).cloned() {
                Some(id) => id,
                None => {
                    return Ok(ProviderJob {
                        id: format!("absent-{}", request.job_id),
                        status: JobStatus::Cancelled,
                        access_url: None,
                        error: None,
                        receipt: Some(ComputeReceipt {
                            id: format!("receipt-{}", request.job_id),
                            job_id: request.job_id,
                            app_id: request.plan.app.id,
                            provider: "fake".into(),
                            runtime_secs: 0,
                            charged_usdc_micros: 0,
                            refunded_usdc_micros: request.plan.maximum_usdc_micros,
                            commitment: "test:absent".into(),
                            transaction: None,
                        }),
                    });
                }
            },
        };
        let mut jobs = self.jobs.lock().await;
        let (plan, provider) = jobs
            .get_mut(&provider_id)
            .ok_or(ProviderError::InvalidState)?;
        if provider.status != JobStatus::Cancelled {
            provider.status = JobStatus::Cancelled;
            provider.access_url = None;
            self.active_allocations.fetch_sub(1, Ordering::SeqCst);
            provider.receipt = Some(ComputeReceipt {
                id: format!("receipt-{}", request.job_id),
                job_id: request.job_id.clone(),
                app_id: plan.app.id.clone(),
                provider: "fake".into(),
                runtime_secs: 0,
                charged_usdc_micros: 0,
                refunded_usdc_micros: plan.maximum_usdc_micros,
                commitment: format!("test:{}", request.job_id),
                transaction: None,
            });
        }
        Ok(provider.clone())
    }
}

fn offer() -> ComputeOffer {
    ComputeOffer {
        id: "offer-a".into(),
        gpu: GpuSpec {
            model: "L40S".into(),
            vram_mib: 48_000,
            cuda_major: 12,
        },
        rate_usdc_micros_per_hour: 720_000,
        trust_class: TrustClass::Open,
        online: true,
    }
}

fn plan(duration_secs: u64) -> LaunchPlan {
    let offer = offer();
    LaunchPlan {
        app: AppCatalog::builtin().app("gpu-workspace").unwrap().clone(),
        offer,
        duration_secs,
        maximum_usdc_micros: 720_000 * duration_secs / 3_600,
    }
}

fn principal(id: &str, cap: u64) -> Principal {
    Principal {
        id: id.into(),
        spend_cap_usdc_micros: cap,
    }
}

async fn control_plane(
    temp: &TempDir,
    provider: Arc<FakeProvider>,
) -> Result<ControlPlane, StoreError> {
    let store = SqliteStore::open(temp.path().join("control.sqlite3")).await?;
    Ok(ControlPlane::new(AppCatalog::builtin(), store, provider))
}

#[tokio::test]
async fn restart_recovers_prepared_launch_without_a_second_allocation() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let owner = principal("owner-a", 500_000);
    let control = control_plane(&temp, Arc::clone(&provider)).await.unwrap();

    provider.set_launch_available(false);
    let error = control
        .submit(&owner, "restart-key", plan(1_800))
        .await
        .unwrap_err();
    assert!(matches!(error, ServiceError::Provider(_)));

    provider.set_launch_available(true);
    let restarted = control_plane(&temp, Arc::clone(&provider)).await.unwrap();
    let report = restarted.recover().await.unwrap();
    assert_eq!(report.recovered, 1);
    assert_eq!(provider.allocations.load(Ordering::SeqCst), 1);

    let replay = restarted
        .submit(&owner, "restart-key", plan(1_800))
        .await
        .unwrap();
    assert_eq!(replay.status, JobStatus::Running);
    assert_eq!(provider.allocations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn same_key_replays_same_plan_and_rejects_a_different_plan() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let control = control_plane(&temp, provider).await.unwrap();
    let owner = principal("owner-a", 2_000_000);

    let first = control
        .submit(&owner, "same-key", plan(1_800))
        .await
        .unwrap();
    let replay = control
        .submit(&owner, "same-key", plan(1_800))
        .await
        .unwrap();
    assert_eq!(first.id, replay.id);

    let error = control
        .submit(&owner, "same-key", plan(3_600))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ServiceError::Store(StoreError::IdempotencyConflict)
    ));
}

#[tokio::test]
async fn concurrent_same_key_requests_allocate_once_and_return_one_job() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let control = control_plane(&temp, Arc::clone(&provider)).await.unwrap();
    let owner = principal("owner-a", 1_000_000);

    let first = control.submit(&owner, "concurrent-replay", plan(1_800));
    let second = control.submit(&owner, "concurrent-replay", plan(1_800));
    let (first, second) = tokio::join!(first, second);
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(provider.allocations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn foreign_owner_cannot_read_or_cancel_a_job() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let control = control_plane(&temp, provider).await.unwrap();
    let owner = principal("owner-a", 1_000_000);
    let foreign = principal("owner-b", 1_000_000);
    let job = control
        .submit(&owner, "owned-job", plan(1_800))
        .await
        .unwrap();

    assert!(matches!(
        control.job(&foreign, &job.id).await.unwrap_err(),
        ServiceError::Store(StoreError::NotFound)
    ));
    assert!(matches!(
        control.cancel(&foreign, &job.id).await.unwrap_err(),
        ServiceError::Store(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn concurrent_launches_cannot_exceed_the_durable_cap() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let control = control_plane(&temp, provider).await.unwrap();
    let owner = principal("owner-a", 360_000);

    let first = control.submit(&owner, "cap-a", plan(1_800));
    let second = control.submit(&owner, "cap-b", plan(1_800));
    let (first, second) = tokio::join!(first, second);
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    let error = first.err().or_else(|| second.err()).unwrap();
    assert!(matches!(
        error,
        ServiceError::Store(StoreError::SpendCapExceeded)
    ));
}

#[tokio::test]
async fn cancellation_is_idempotent_and_releases_unused_reservation() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let control = control_plane(&temp, provider).await.unwrap();
    let owner = principal("owner-a", 360_000);
    let job = control
        .submit(&owner, "cancel-a", plan(1_800))
        .await
        .unwrap();

    let cancelled = control.cancel(&owner, &job.id).await.unwrap();
    assert_eq!(cancelled.status, JobStatus::Cancelled);
    assert!(cancelled.receipt.is_some());
    assert_eq!(
        control.cancel(&owner, &job.id).await.unwrap().status,
        JobStatus::Cancelled
    );

    control
        .submit(&owner, "after-cancel", plan(1_800))
        .await
        .expect("released reservation permits the next launch");
}

#[tokio::test]
async fn cancel_before_ready_bills_zero_runtime() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    provider.set_launch_provisioning(true);
    let control = control_plane(&temp, Arc::clone(&provider)).await.unwrap();
    let owner = principal("owner-a", 360_000);
    let job = control
        .submit(&owner, "never-ready", plan(1_800))
        .await
        .unwrap();
    assert_eq!(job.status, JobStatus::Provisioning);

    control.cancel(&owner, &job.id).await.unwrap();
    let (started, requested) = provider.last_cancel_window.lock().await.unwrap();
    // Never went running: the billing window collapses to zero.
    assert!(started >= requested);
}

#[tokio::test]
async fn cancel_after_ready_bills_from_first_running_observation() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    provider.set_launch_provisioning(true);
    let control = control_plane(&temp, Arc::clone(&provider)).await.unwrap();
    let owner = principal("owner-a", 360_000);
    let job = control
        .submit(&owner, "goes-ready", plan(1_800))
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    let before_ready = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    provider.mark_running().await;
    let refreshed = control.job(&owner, &job.id).await.unwrap();
    assert_eq!(refreshed.status, JobStatus::Running);

    control.cancel(&owner, &job.id).await.unwrap();
    let (started, requested) = provider.last_cancel_window.lock().await.unwrap();
    // Billing starts at the recorded ready observation, not at submission.
    assert!(started >= before_ready);
    assert!(started <= requested);
}

#[tokio::test]
async fn v1_routes_require_bearer_auth_while_health_does_not() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let control = control_plane(&temp, provider).await.unwrap();
    let auth = Arc::new(
        AuthRegistry::new(vec![BetaCredential {
            owner: "owner-a".into(),
            token: "test-token-long-enough".into(),
            spend_cap_usdc_micros: 1_000_000,
        }])
        .unwrap(),
    );
    let app = router(auth, control);

    let health = app
        .clone()
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);

    let unauthorized = app
        .oneshot(Request::get("/v1/offers").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        unauthorized
            .headers()
            .get("www-authenticate")
            .unwrap()
            .to_str()
            .unwrap(),
        "Bearer realm=\"covenant-compute\""
    );
}

#[tokio::test]
async fn submitted_plan_must_match_live_offer_and_catalog_exactly() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let control = control_plane(&temp, provider).await.unwrap();
    let owner = principal("owner-a", 1_000_000);
    let mut tampered = plan(1_800);
    tampered.offer.gpu.model = "Different GPU".into();

    assert!(matches!(
        control
            .submit(&owner, "tampered", tampered)
            .await
            .unwrap_err(),
        ServiceError::StaleOffer
    ));
}

#[tokio::test]
async fn tokenized_access_url_is_returned_but_never_persisted() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let control = control_plane(&temp, provider).await.unwrap();
    let owner = principal("owner-a", 1_000_000);
    let job = control
        .submit(&owner, "ephemeral-access", plan(1_800))
        .await
        .unwrap();
    assert_eq!(
        job.access_url.as_deref(),
        Some("https://workload.invalid/session?token=ephemeral-secret")
    );
    assert_eq!(control.jobs(&owner).await.unwrap()[0].access_url, None);

    for suffix in ["", "-wal", "-shm"] {
        let path = temp.path().join(format!("control.sqlite3{suffix}"));
        if path.exists() {
            let bytes = std::fs::read(path).unwrap();
            assert!(
                !bytes
                    .windows(b"ephemeral-secret".len())
                    .any(|window| window == b"ephemeral-secret"),
                "access token leaked into SQLite{suffix}"
            );
        }
    }
}

#[tokio::test]
async fn provisioning_is_durable_and_only_becomes_running_when_provider_is_ready() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    provider.set_launch_provisioning(true);
    let control = control_plane(&temp, Arc::clone(&provider)).await.unwrap();
    let owner = principal("owner-a", 1_000_000);
    let job = control
        .submit(&owner, "provisioning", plan(1_800))
        .await
        .unwrap();
    assert_eq!(job.status, JobStatus::Provisioning);

    provider.mark_running().await;
    let running = control.job(&owner, &job.id).await.unwrap();
    assert_eq!(running.status, JobStatus::Running);
}

#[tokio::test]
async fn restart_reconciliation_cancels_an_overdue_allocated_job() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let store = SqliteStore::open(temp.path().join("control.sqlite3"))
        .await
        .unwrap();
    let launch_plan = plan(1);
    let submitted = store
        .submit("owner-a", 1_000_000, "overdue", &launch_plan, 0)
        .await
        .unwrap();
    let provider_job = provider
        .launch(ProviderLaunch {
            job_id: submitted.job.id.clone(),
            idempotency_key: "overdue".into(),
            plan: launch_plan,
            started_at_ms: 0,
            requested_at_ms: 1,
        })
        .await
        .unwrap();
    store
        .record_provider(&submitted.job.id, provider_job, 1)
        .await
        .unwrap();

    let control = ControlPlane::new(AppCatalog::builtin(), store, provider);
    let report = control.recover().await.unwrap();
    assert_eq!(report.recovered, 1);
    let job = control
        .job(&principal("owner-a", 1_000_000), &submitted.job.id)
        .await
        .unwrap();
    assert_eq!(job.status, JobStatus::Cancelled);
    assert!(job.receipt.is_some());
}

#[tokio::test]
async fn cancellation_during_launch_waits_and_cleans_the_allocation() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    provider.block_next_launch();
    let store = SqliteStore::open(temp.path().join("control.sqlite3"))
        .await
        .unwrap();
    let control = ControlPlane::new(
        AppCatalog::builtin(),
        store.clone(),
        Arc::clone(&provider) as Arc<dyn ProviderBackend>,
    );
    let owner = principal("owner-a", 1_000_000);

    let submit = tokio::spawn({
        let control = control.clone();
        let owner = owner.clone();
        async move {
            control
                .submit(&owner, "cancel-launch-race", plan(1_800))
                .await
        }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        provider.wait_for_blocked_launch(),
    )
    .await
    .unwrap();
    let job = store.recoverable_jobs().await.unwrap().remove(0);

    let cancel = tokio::spawn({
        let control = control.clone();
        let owner = owner.clone();
        let id = job.id.clone();
        async move { control.cancel(&owner, &id).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if store
                .job(&owner.id, &job.id)
                .await
                .unwrap()
                .is_cancel_requested()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    provider.release_blocked_launch();
    let submitted = submit.await.unwrap().unwrap();
    let cancelled = cancel.await.unwrap().unwrap();
    assert_eq!(submitted.status, JobStatus::Cancelled);
    assert_eq!(cancelled.status, JobStatus::Cancelled);
    assert_eq!(provider.allocations.load(Ordering::SeqCst), 1);
    assert_eq!(provider.active_allocations.load(Ordering::SeqCst), 0);
    assert_eq!(provider.cancellations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn late_launch_result_is_explicitly_cleaned_after_terminal_cancellation() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    provider.block_next_launch();
    let store = SqliteStore::open(temp.path().join("control.sqlite3"))
        .await
        .unwrap();
    let control = ControlPlane::new(
        AppCatalog::builtin(),
        store.clone(),
        Arc::clone(&provider) as Arc<dyn ProviderBackend>,
    );
    let owner = principal("owner-a", 1_000_000);
    let launch_plan = plan(1_800);

    let submit = tokio::spawn({
        let control = control.clone();
        let owner = owner.clone();
        let launch_plan = launch_plan.clone();
        async move {
            control
                .submit(&owner, "late-launch-cleanup", launch_plan)
                .await
        }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        provider.wait_for_blocked_launch(),
    )
    .await
    .unwrap();
    let job = store.recoverable_jobs().await.unwrap().remove(0);
    store.request_cancel(&owner.id, &job.id, 1).await.unwrap();
    store
        .record_provider(
            &job.id,
            ProviderJob {
                id: format!("absent-{}", job.id),
                status: JobStatus::Cancelled,
                access_url: None,
                error: None,
                receipt: Some(ComputeReceipt {
                    id: format!("receipt-{}", job.id),
                    job_id: job.id.clone(),
                    app_id: launch_plan.app.id.clone(),
                    provider: "external-canceller".into(),
                    runtime_secs: 0,
                    charged_usdc_micros: 0,
                    refunded_usdc_micros: launch_plan.maximum_usdc_micros,
                    commitment: "external:absent".into(),
                    transaction: None,
                }),
            },
            1,
        )
        .await
        .unwrap();

    provider.release_blocked_launch();
    let result = submit.await.unwrap().unwrap();
    assert_eq!(result.status, JobStatus::Cancelled);
    assert_eq!(provider.allocations.load(Ordering::SeqCst), 1);
    assert_eq!(provider.active_allocations.load(Ordering::SeqCst), 0);
    assert_eq!(provider.cancellations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn prepared_job_past_deadline_is_cancelled_before_allocation() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let store = SqliteStore::open(temp.path().join("control.sqlite3"))
        .await
        .unwrap();
    let owner = principal("owner-a", 1_000_000);
    let launch_plan = plan(1_800);
    store
        .submit(
            &owner.id,
            owner.spend_cap_usdc_micros,
            "expired-prepared",
            &launch_plan,
            0,
        )
        .await
        .unwrap();
    let control = ControlPlane::new(
        AppCatalog::builtin(),
        store,
        Arc::clone(&provider) as Arc<dyn ProviderBackend>,
    );

    let result = control
        .submit(&owner, "expired-prepared", launch_plan)
        .await
        .unwrap();
    assert_eq!(result.status, JobStatus::Cancelled);
    assert_eq!(provider.allocations.load(Ordering::SeqCst), 0);
    assert_eq!(provider.active_allocations.load(Ordering::SeqCst), 0);
    assert_eq!(provider.cancellations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stalled_poll_does_not_delay_another_jobs_deadline_cancellation() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let store = SqliteStore::open(temp.path().join("control.sqlite3"))
        .await
        .unwrap();
    let now_ms = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let stalled_plan = plan(21_600);
    let expired_plan = plan(1);
    let stalled = store
        .submit(
            "owner-a",
            10_000_000,
            "stalled-poll",
            &stalled_plan,
            now_ms - 2_000,
        )
        .await
        .unwrap();
    let expired = store
        .submit(
            "owner-a",
            10_000_000,
            "parallel-deadline",
            &expired_plan,
            now_ms - 1_500,
        )
        .await
        .unwrap();
    for (submitted, launch_plan) in [
        (&stalled, stalled_plan.clone()),
        (&expired, expired_plan.clone()),
    ] {
        let provider_job = provider
            .launch(ProviderLaunch {
                job_id: submitted.job.id.clone(),
                idempotency_key: submitted.job.idempotency_key.clone(),
                plan: launch_plan,
                started_at_ms: submitted.job.created_at_ms,
                requested_at_ms: now_ms,
            })
            .await
            .unwrap();
        store
            .record_provider(&submitted.job.id, provider_job, now_ms)
            .await
            .unwrap();
    }

    provider.block_next_poll();
    let control = ControlPlane::new(
        AppCatalog::builtin(),
        store.clone(),
        Arc::clone(&provider) as Arc<dyn ProviderBackend>,
    );
    let recovery = tokio::spawn({
        let control = control.clone();
        async move { control.recover().await }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        provider.wait_for_blocked_poll(),
    )
    .await
    .unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if store
                .job("owner-a", &expired.job.id)
                .await
                .unwrap()
                .is_terminal()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!recovery.is_finished());

    provider.release_blocked_poll();
    let report = recovery.await.unwrap().unwrap();
    assert_eq!(report.recovered, 2);
    assert_eq!(report.deferred, 0);
    assert_eq!(provider.active_allocations.load(Ordering::SeqCst), 1);
}
